//! The long-lived, per-workspace daemon.
//!
//! Holds a warm [`Session`] per language so `initialize` + project indexing is paid once.
//! Listens on a unix socket keyed to the workspace root, serves requests serially (one
//! agent, and LSP access is serial anyway), shuts down after idle, and — importantly —
//! kills its child language servers on the way out so they don't leak.

use crate::ipc::{self, Request, Response};
use crate::lsp::{self, Language, session::Session};
use crate::selector::Selector;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

type Sessions = Arc<Mutex<HashMap<Language, Session>>>;

pub fn run() -> Result<()> {
    let root = std::env::current_dir()?;
    let path = ipc::socket_path(&root);

    // Don't clobber a live daemon; clear only a stale socket file.
    if UnixStream::connect(&path).is_ok() {
        tracing::info!("a daemon is already serving {}", path.display());
        return Ok(());
    }
    let _ = std::fs::remove_file(&path);

    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding socket {}", path.display()))?;
    tracing::info!("daemon listening on {}", path.display());

    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let last_active = Arc::new(Mutex::new(Instant::now()));

    spawn_idle_watchdog(sessions.clone(), last_active.clone(), path.clone());

    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        *last_active.lock().unwrap() = Instant::now();
        if let Err(e) = handle(conn, &root, &sessions, &path) {
            tracing::warn!("connection error: {e:#}");
        }
        *last_active.lock().unwrap() = Instant::now();
    }
    Ok(())
}

/// Exit the process once we've been idle too long — but drop sessions first so the child
/// language servers are killed (process::exit skips destructors).
fn spawn_idle_watchdog(sessions: Sessions, last_active: Arc<Mutex<Instant>>, path: PathBuf) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let idle = last_active.lock().unwrap().elapsed();
            if idle > IDLE_TIMEOUT {
                tracing::info!("idle for {idle:?}; shutting down");
                shutdown(&sessions, &path);
            }
        }
    });
}

/// Drop all sessions (kills child servers via `LspClient::drop`), remove the socket, exit.
fn shutdown(sessions: &Sessions, path: &Path) -> ! {
    sessions.lock().unwrap().clear();
    let _ = std::fs::remove_file(path);
    std::process::exit(0);
}

fn handle(conn: UnixStream, root: &Path, sessions: &Sessions, path: &Path) -> Result<()> {
    let mut writer = conn.try_clone()?;
    let mut reader = BufReader::new(conn);

    let Some(req): Option<Request> = ipc::read_msg(&mut reader)? else {
        return Ok(()); // client hung up
    };

    // Shutdown is special: ack, then tear down.
    if matches!(req, Request::Shutdown) {
        let _ = ipc::write_msg(&mut writer, &Response::Ok { result: json!({ "shutdown": true }) });
        shutdown(sessions, path);
    }

    let resp = match dispatch(req, root, sessions) {
        Ok(result) => Response::Ok { result },
        Err(e) => Response::Err { message: format!("{e:#}") },
    };
    ipc::write_msg(&mut writer, &resp)?;
    Ok(())
}

fn dispatch(req: Request, root: &Path, sessions: &Sessions) -> Result<Value> {
    match req {
        Request::Ping => Ok(json!({ "pong": true, "pid": std::process::id() })),
        Request::Shutdown => unreachable!("handled in handle()"),
        Request::Diagnostics { file } => {
            let abs = PathBuf::from(&file);
            let lang = lang_of(&abs)?;
            with_session(sessions, lang, root, |s| s.diagnostics(&abs, lang))
        }
        Request::Resolve { selector } => {
            let sel = Selector::parse(&selector)?;
            let abs = std::fs::canonicalize(sel.file())
                .with_context(|| format!("no such file: {}", sel.file().display()))?;
            let lang = lang_of(&abs)?;
            with_session(sessions, lang, root, |s| s.resolve(&sel, &abs, lang))
        }
        Request::Rename { selector, new_name, apply } => {
            let sel = Selector::parse(&selector)?;
            let abs = std::fs::canonicalize(sel.file())
                .with_context(|| format!("no such file: {}", sel.file().display()))?;
            let lang = lang_of(&abs)?;
            with_session(sessions, lang, root, |s| s.rename(&sel, &abs, lang, &new_name, apply))
        }
    }
}

fn lang_of(path: &Path) -> Result<Language> {
    Language::from_path(path).ok_or_else(|| anyhow!("unsupported file type: {}", path.display()))
}

/// Run `f` against the warm session for `lang`, starting one (and paying indexing) on
/// first use.
fn with_session<F, R>(sessions: &Sessions, lang: Language, root: &Path, f: F) -> Result<R>
where
    F: FnOnce(&mut Session) -> Result<R>,
{
    let mut map = sessions.lock().unwrap();
    if !map.contains_key(&lang) {
        let backend = lsp::resolve_for(lang)?;
        tracing::info!("warming {:?} backend: {}", lang, backend.program);
        map.insert(lang, Session::start(backend, root)?);
    }
    f(map.get_mut(&lang).unwrap())
}
