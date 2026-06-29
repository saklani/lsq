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

pub(crate) type Sessions = Arc<Mutex<HashMap<Language, Session>>>;

/// An empty session map. Used by both the daemon and the MCP front-end, which hold warm
/// sessions in-process.
pub(crate) fn new_sessions() -> Sessions {
    Arc::new(Mutex::new(HashMap::new()))
}

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

    let sessions: Sessions = new_sessions();
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

pub(crate) fn dispatch(req: Request, root: &Path, sessions: &Sessions) -> Result<Value> {
    match req {
        Request::Ping => Ok(json!({ "pong": true, "pid": std::process::id() })),
        Request::Shutdown => unreachable!("handled in handle()"),

        // file-addressed verbs
        Request::Diagnostics { file } => {
            let (abs, lang) = file_target(&file)?;
            with_session(sessions, lang, root, |s| s.diagnostics(&abs))
        }
        Request::Symbols { file } => {
            let (abs, lang) = file_target(&file)?;
            with_session(sessions, lang, root, |s| s.symbols(&abs))
        }

        // selector-addressed verbs
        Request::Resolve { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.resolve(&sel, &abs))
        }
        Request::Rename { selector, new_name, apply } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.rename(&sel, &abs, &new_name, apply))
        }
        Request::Fix { selector, apply, pick } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.fix(&sel, &abs, apply, pick))
        }
        Request::Refs { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| {
                s.locations(&sel, &abs, "textDocument/references", true)
            })
        }
        Request::Def { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| {
                s.locations(&sel, &abs, "textDocument/definition", false)
            })
        }
        Request::Impl { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| {
                s.locations(&sel, &abs, "textDocument/implementation", false)
            })
        }
        Request::TypeDef { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| {
                s.locations(&sel, &abs, "textDocument/typeDefinition", false)
            })
        }
        Request::Type { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.hover(&sel, &abs))
        }
        Request::Callers { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.callers(&sel, &abs))
        }
        Request::Search { query } => {
            // workspace/symbol isn't file-scoped, so infer the language from the workspace.
            let lang = detect_lang(root)?;
            with_session(sessions, lang, root, |s| s.workspace_symbols(&query))
        }
    }
}

/// Infer the workspace language from project markers (for `search`, which has no file).
fn detect_lang(root: &Path) -> Result<Language> {
    if root.join("Cargo.toml").is_file() {
        Ok(Language::Rust)
    } else if root.join("tsconfig.json").is_file() || root.join("package.json").is_file() {
        Ok(Language::TypeScript)
    } else {
        Err(anyhow!(
            "could not detect workspace language for search in {} \
             (no Cargo.toml or tsconfig.json/package.json)",
            root.display()
        ))
    }
}

fn lang_of(path: &Path) -> Result<Language> {
    Language::from_path(path).ok_or_else(|| anyhow!("unsupported file type: {}", path.display()))
}

/// Resolve a selector string to (parsed selector, canonical file, language).
fn target(selector: &str) -> Result<(Selector, PathBuf, Language)> {
    let sel = Selector::parse(selector)?;
    let abs = std::fs::canonicalize(sel.file())
        .with_context(|| format!("no such file: {}", sel.file().display()))?;
    let lang = lang_of(&abs)?;
    Ok((sel, abs, lang))
}

/// Resolve a file string to (canonical file, language).
fn file_target(file: &str) -> Result<(PathBuf, Language)> {
    let abs = std::fs::canonicalize(file).with_context(|| format!("no such file: {file}"))?;
    let lang = lang_of(&abs)?;
    Ok((abs, lang))
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
        tracing::info!("warming {:?} backend: {}", lang, backend.program());
        map.insert(lang, Session::start(backend, root)?);
    }
    f(map.get_mut(&lang).unwrap())
}
