//! The query engine: warm `Session`s + the request router.
//!
//! Holds one warm [`Session`] per language so `initialize` + project indexing is paid once,
//! and routes a [`Request`] to the right session method. The MCP front-end ([`crate::mcp`])
//! owns the `Sessions` map in-process and calls [`dispatch`] directly — there is no separate
//! daemon process and no socket.

use crate::lsp::{self, Language, session::Session};
use crate::selector::Selector;
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A semantic query, addressed by file (diagnostics/symbols), by selector (the position
/// verbs), or by name (search). The MCP tools in [`crate::mcp`] construct these.
#[derive(Debug)]
pub(crate) enum Request {
    /// Diagnostics for an absolute file path.
    Diagnostics { file: String },
    /// Diagnostics for a speculative (unsaved) edit to a file.
    Check { file: String, content: String },
    /// Resolve a selector (FILE:LINE:COL, FILE:LINE, FILE@NAME) to an exact position.
    Resolve { selector: String },
    /// Rename the symbol at a selector across the workspace.
    Rename {
        selector: String,
        new_name: String,
        apply: bool,
    },
    /// List/apply code actions (quick-fixes, refactors) at a selector.
    Fix {
        selector: String,
        apply: bool,
        pick: usize,
    },
    /// Find references to the symbol at a selector.
    Refs { selector: String },
    /// Go to the definition of the symbol at a selector.
    Def { selector: String },
    /// Go to the implementation(s) of the symbol at a selector.
    Impl { selector: String },
    /// Go to the type definition of the symbol at a selector.
    TypeDef { selector: String },
    /// Type / hover info at a selector.
    Type { selector: String },
    /// List the symbols defined in a file.
    Symbols { file: String },
    /// Find callers (incoming calls) of the function at a selector.
    Callers { selector: String },
    /// Blast radius of the symbol at a selector (references + callers + implementations).
    Impact { selector: String },
    /// Search for a symbol by name across the (single) project.
    Search { query: String },
    /// Go to the declaration of the symbol at a selector.
    Declaration { selector: String },
    /// Find callees (outgoing calls) of the function at a selector.
    Callees { selector: String },
    /// Occurrences of the symbol at a selector within its file.
    Highlight { selector: String },
    /// Signature / parameter help for the call at a selector.
    Signature { selector: String },
    /// Completions available at a selector.
    Complete { selector: String },
    /// Format a file (prints a diff; writes when apply is set).
    Format { file: String, apply: bool },
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
        Request::Diagnostics { file } => {
            let (abs, lang) = file_target(&file)?;
            with_session(sessions, lang, root, |s| s.diagnostics(&abs))
        }
        Request::Symbols { file } => {
            let (abs, lang) = file_target(&file)?;
            with_session(sessions, lang, root, |s| s.symbols(&abs))
        }
        Request::Check { file, content } => {
            let (abs, lang) = file_target(&file)?;
            with_session(sessions, lang, root, |s| s.check(&abs, &content))
        }

        // selector-addressed verbs
        Request::Resolve { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.resolve(&sel, &abs))
        }
        Request::Rename {
            selector,
            new_name,
            apply,
        } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| {
                s.rename(&sel, &abs, &new_name, apply)
            })
        }
        Request::Fix {
            selector,
            apply,
            pick,
        } => {
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
        Request::Impact { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.impact(&sel, &abs))
        }
        Request::Search { query } => {
            // workspace/symbol isn't file-scoped, so infer the language from the workspace.
            let lang = detect_lang(root)?;
            with_session(sessions, lang, root, |s| s.workspace_symbols(&query))
        }
        Request::Declaration { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| {
                s.locations(&sel, &abs, "textDocument/declaration", false)
            })
        }
        Request::Callees { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.callees(&sel, &abs))
        }
        Request::Highlight { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.highlight(&sel, &abs))
        }
        Request::Signature { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.signature(&sel, &abs))
        }
        Request::Complete { selector } => {
            let (sel, abs, lang) = target(&selector)?;
            with_session(sessions, lang, root, |s| s.complete(&sel, &abs))
        }
        Request::Format { file, apply } => {
            let (abs, lang) = file_target(&file)?;
            with_session(sessions, lang, root, |s| s.format(&abs, apply))
        }
    }
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
    let session = match map.entry(lang) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let backend = lsp::resolve_for(lang)?;
            tracing::info!("warming {:?} backend: {}", lang, backend.program());
            e.insert(Session::start(backend, root)?)
        }
    };
    f(session)
}
