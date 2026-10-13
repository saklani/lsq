//! The query engine: warm `Session`s + the request router.
//!
//! Holds one warm [`Session`] per language so `initialize` + project indexing is paid once,
//! and routes a [`Request`] to the right session method. The MCP front-end ([`crate::mcp`])
//! owns the `Sessions` map in-process and calls [`dispatch`] directly — it all runs in one
//! process, no socket.

use crate::agent::{self, Address};
use crate::coordinate;
use crate::protocol::{self, Language, session::Session};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A semantic query, addressed by file (diagnostics/symbols), by [`Address`] (the position
/// verbs), or by name (search). The MCP tools in [`crate::mcp`] build these from structured
/// `Target` fields — there is no address string to parse.
#[derive(Debug)]
pub(crate) enum Request {
    /// Diagnostics for an absolute file path.
    Diagnostics { file: String },
    /// Diagnostics for a speculative (unsaved) edit to a file.
    Check { file: String, content: String },
    /// Resolve an address to an exact position.
    Resolve { address: Address },
    /// Rename the symbol at an address across the workspace.
    Rename {
        address: Address,
        new_name: String,
        apply: bool,
    },
    /// List/apply code actions (quick-fixes, refactors) at an address.
    Fix {
        address: Address,
        apply: bool,
        pick: usize,
    },
    /// Find references to the symbol at an address.
    Refs { address: Address },
    /// Go to the definition of the symbol at an address.
    Def { address: Address },
    /// Go to the implementation(s) of the symbol at an address.
    Impl { address: Address },
    /// Go to the type definition of the symbol at an address.
    TypeDef { address: Address },
    /// Type / hover info at an address.
    Type { address: Address },
    /// List the symbols defined in a file.
    Symbols { file: String },
    /// Find callers (incoming calls) of the function at an address.
    Callers { address: Address },
    /// Blast radius of the symbol at an address (references + callers + implementations).
    Impact { address: Address },
    /// Search for a symbol by name across the (single) project.
    Search { query: String },
    /// Go to the declaration of the symbol at an address.
    Declaration { address: Address },
    /// Find callees (outgoing calls) of the function at an address.
    Callees { address: Address },
    /// Occurrences of the symbol at an address within its file.
    Highlight { address: Address },
    /// Signature / parameter help for the call at an address.
    Signature { address: Address },
    /// Completions available at an address.
    Complete { address: Address },
    /// Format a file (prints a diff; writes when apply is set).
    Format { file: String, apply: bool },
    /// Drop warm language server(s) so they re-index from disk on the next request. With a file,
    /// only its language; without, all warm languages.
    Reload { file: Option<String> },
}

pub(crate) type Sessions = Arc<Mutex<HashMap<Language, Session>>>;

/// An empty session map. The MCP front-end holds one of these in-process and warms it lazily.
pub(crate) fn new_sessions() -> Sessions {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Route a request to the warm session for its language, starting one (and paying indexing)
/// on first use.
pub(crate) fn dispatch(req: Request, root: &Path, sessions: &Sessions) -> Result<Value> {
    match req {
        // file-addressed verbs
        Request::Diagnostics { file } => on_file(sessions, root, &file, |s, f| s.diagnostics(f)),
        Request::Symbols { file } => on_file(sessions, root, &file, |s, f| s.symbols(f)),
        Request::Check { file, content } => {
            on_file(sessions, root, &file, |s, f| s.check(f, &content))
        }
        Request::Format { file, apply } => {
            on_file(sessions, root, &file, |s, f| s.format(f, apply))
        }

        // location-family verbs — resolve the address to a wire position, then one session
        // method distinguished by LSP method + include-decl.
        Request::Refs { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.locations(f, pos, "textDocument/references", true)
        }),
        Request::Def { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.locations(f, pos, "textDocument/definition", false)
        }),
        Request::Impl { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.locations(f, pos, "textDocument/implementation", false)
        }),
        Request::TypeDef { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.locations(f, pos, "textDocument/typeDefinition", false)
        }),
        Request::Declaration { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.locations(f, pos, "textDocument/declaration", false)
        }),

        // other symbol-addressed verbs
        Request::Resolve { address } => on_file(sessions, root, address.file(), |s, f| {
            resolve_report(s, f, &address)
        }),
        Request::Rename {
            address,
            new_name,
            apply,
        } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.rename(f, pos, &new_name, apply)
        }),
        Request::Fix {
            address,
            apply,
            pick,
        } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.fix(f, pos, apply, pick)
        }),
        Request::Type { address } => {
            on_file(sessions, root, address.file(), |s, f| hover(s, f, &address))
        }
        Request::Callers { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.callers(f, pos)
        }),
        Request::Callees { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.callees(f, pos)
        }),
        Request::Impact { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.impact(f, pos)
        }),
        Request::Highlight { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.highlight(f, pos)
        }),
        Request::Signature { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.signature(f, pos)
        }),
        Request::Complete { address } => on_file(sessions, root, address.file(), |s, f| {
            let pos = resolve(s, f, &address)?.pos;
            s.complete(f, pos)
        }),

        // verbs that don't resolve through a single file
        Request::Search { query } => {
            // workspace/symbol isn't file-scoped, so infer the language from the workspace.
            let lang = detect_lang(root)?;
            with_session(sessions, lang, root, |s| s.workspace_symbols(&query))
        }
        Request::Reload { file } => reload(sessions, file),
    }
}

/// Drop warm sessions so their servers re-index from current disk on next use — the cure for a
/// stale index after files are moved/created/deleted. With a `file`, reload only its language;
/// without, reload every warm language. Dropping a `Session` kills its child server (via `Drop`);
/// re-warming is lazy — the next request for that language pays the cold indexing again.
fn reload(sessions: &Sessions, file: Option<String>) -> Result<Value> {
    let mut map = sessions.lock().unwrap();
    let reloaded: Vec<String> = match file {
        Some(f) => {
            let lang = lang_of(Path::new(&f))?;
            map.remove(&lang)
                .map(|_| format!("{lang:?}"))
                .into_iter()
                .collect()
        }
        None => {
            let langs = map.keys().map(|l| format!("{l:?}")).collect();
            map.clear();
            langs
        }
    };
    Ok(json!({ "reloaded": reloaded }))
}

/// Infer the workspace language from project markers (for `search`, which has no file).
fn detect_lang(root: &Path) -> Result<Language> {
    if root.join("Cargo.toml").is_file() {
        Ok(Language::Rust)
    } else if root.join("tsconfig.json").is_file() || root.join("package.json").is_file() {
        Ok(Language::TypeScript)
    } else if root.join("pyproject.toml").is_file()
        || root.join("setup.py").is_file()
        || root.join("requirements.txt").is_file()
    {
        Ok(Language::Python)
    } else {
        Err(anyhow!(
            "could not detect workspace language for search in {} (no Cargo.toml, \
             tsconfig.json/package.json, or pyproject.toml/setup.py/requirements.txt)",
            root.display()
        ))
    }
}

fn lang_of(path: &Path) -> Result<Language> {
    Language::from_path(path).ok_or_else(|| anyhow!("unsupported file type: {}", path.display()))
}

/// Canonicalize a file path and detect its language. Used by both the file-addressed verbs
/// (the `file` arg) and the address verbs (`address.file()`).
fn file_target(file: impl AsRef<Path>) -> Result<(PathBuf, Language)> {
    let file = file.as_ref();
    let abs =
        std::fs::canonicalize(file).with_context(|| format!("no such file: {}", file.display()))?;
    let lang = lang_of(&abs)?;
    Ok((abs, lang))
}

/// Resolve `file` to its warm session (canonicalize + language-detect), then run `f` against
/// that session and the absolute path. The shared shape of every file/address-addressed verb —
/// only the closure differs per verb.
fn on_file<F, R>(sessions: &Sessions, root: &Path, file: impl AsRef<Path>, f: F) -> Result<R>
where
    F: FnOnce(&mut Session, &Path) -> Result<R>,
{
    let (abs, lang) = file_target(file)?;
    with_session(sessions, lang, root, |s| f(s, &abs))
}

/// Run `f` against the warm session for `lang`, starting one (and paying indexing) on
/// first use.
fn with_session<F, R>(sessions: &Sessions, lang: Language, root: &Path, f: F) -> Result<R>
where
    F: FnOnce(&mut Session) -> Result<R>,
{
    let mut map = sessions.lock().unwrap();
    let session = match map.entry(lang) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let adapter = protocol::resolve_for(lang)?;
            tracing::info!("warming {:?} adapter: {}", lang, adapter.program());
            e.insert(Session::start(adapter, root)?)
        }
    };
    f(session)
}

// ---- address resolution: the agent↔wire translation the protocol layer no longer does -------

/// A resolved address: the wire position to query, plus `{name, kind}` for the name form
/// (`Null` otherwise).
struct Located {
    pos: protocol::Position,
    symbol: Value,
}

/// `Address` → wire position. Position/Line are pure coordinate math on the file text; Name
/// delegates the lookup to the server via [`Session::resolve_name`] (the only case that needs it).
fn resolve(session: &mut Session, file: &Path, addr: &Address) -> Result<Located> {
    match addr {
        Address::Position { pos, .. } => {
            let text = std::fs::read_to_string(file)?;
            let line_text = nth_line(&text, pos.line)?;
            let p = coordinate::to_protocol(*pos, &line_text).ok_or_else(|| {
                anyhow!("column {} is out of range on line {}", pos.column, pos.line)
            })?;
            Ok(Located {
                pos: p,
                symbol: Value::Null,
            })
        }
        Address::Line { line1, .. } => {
            let text = std::fs::read_to_string(file)?;
            let line_text = nth_line(&text, *line1)?;
            let column = first_nonspace_column(&line_text);
            let p =
                coordinate::to_protocol(agent::Position::new(*line1, column), &line_text).unwrap();
            Ok(Located {
                pos: p,
                symbol: Value::Null,
            })
        }
        Address::Name { name, .. } => {
            let (pos, symbol) = session.resolve_name(file, name)?;
            Ok(Located { pos, symbol })
        }
    }
}

/// `type` / hover, plus the resolved `position` echo (in agent coordinates) that
/// [`Session::hover`] leaves to transport.
fn hover(session: &mut Session, file: &Path, addr: &Address) -> Result<Value> {
    let loc = resolve(session, file, addr)?;
    let mut out = session.hover(file, loc.pos)?;
    out["position"] = agent_position(file, loc.pos);
    Ok(out)
}

/// Resolve an address to an exact position (+ the symbol, for the name form) and report it —
/// pure translation, no LSP query beyond the name lookup inside [`resolve`].
fn resolve_report(session: &mut Session, file: &Path, addr: &Address) -> Result<Value> {
    let loc = resolve(session, file, addr)?;
    Ok(json!({
        "file": file.display().to_string(),
        "symbol": loc.symbol,
        "position": agent_position(file, loc.pos),
        "lsp": { "line": loc.pos.line, "character": loc.pos.character },
    }))
}

/// Wire position → agent-coordinate `{line, column}` JSON, reading `file` for the UTF-16 math.
fn agent_position(file: &Path, pos: protocol::Position) -> Value {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let line_text = nth_line(&text, pos.line + 1).unwrap_or_default();
    let ap = coordinate::to_agent(pos, &line_text);
    json!({ "line": ap.line, "column": ap.column })
}

/// The text of a 1-based line, without its terminator.
fn nth_line(text: &str, line1: u32) -> Result<String> {
    text.lines()
        .nth((line1 as usize).saturating_sub(1))
        .map(str::to_string)
        .ok_or_else(|| anyhow!("line {line1} is out of range"))
}

/// 1-based column of the first non-whitespace character (or 1 for a blank line).
fn first_nonspace_column(line: &str) -> u32 {
    line.chars().take_while(|c| c.is_whitespace()).count() as u32 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_helpers() {
        assert_eq!(nth_line("a\nb\nc", 2).unwrap(), "b");
        assert!(nth_line("a", 5).is_err());
        assert_eq!(first_nonspace_column("    x"), 5);
        assert_eq!(first_nonspace_column("y"), 1);
    }
}
