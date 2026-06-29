//! The `Backend` trait — one implementation per language server.
//!
//! Everything that differs between servers (launch args, init options, readiness, the
//! diagnostics model) lives in the impls. `Session` stays backend-agnostic: it calls
//! `backend.await_ready(...)` before semantic queries and `backend.collect_diagnostics(...)`
//! for diagnostics, and never branches on which server is running.

mod rust_analyzer;
mod tsgo;
mod typescript_language_server;

use super::Language;
use super::client::LspClient;
use anyhow::{Result, anyhow};
use rust_analyzer::RustAnalyzer;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tsgo::Tsgo;
use typescript_language_server::TypescriptLanguageServer;

/// Identity of a backend, used to drive selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Tsgo,
    TypescriptLanguageServer,
    RustAnalyzer,
}

/// A language server lsq can drive. Implementors encapsulate all server-specific behavior.
pub trait Backend: Send {
    fn program(&self) -> &str;
    fn args(&self) -> Vec<String>;
    /// The LSP `languageId` used in `didOpen`.
    fn language_id(&self) -> &'static str;

    /// Backend-specific `initializationOptions` for the `initialize` request.
    fn initialization_options(&self) -> Value {
        Value::Null
    }

    /// Block until the server can answer SEMANTIC queries (references/rename/hover/def) for
    /// `uri`. Default: no wait — pull servers answer immediately.
    fn await_ready(&self, _client: &mut LspClient, _uri: &str) -> Result<()> {
        Ok(())
    }

    /// Whether `workspace/symbol` must be polled until the index is built — push/indexing
    /// servers (rust-analyzer) return an empty list while cold rather than an error.
    fn needs_index_poll(&self) -> bool {
        false
    }

    /// The per-project config file (e.g. `tsconfig.json`) for servers that load projects
    /// lazily one file at a time. When set, `workspace/symbol` only covers a project once a
    /// file in it is open, so lsq opens a representative file per project to span a monorepo.
    /// `None` for servers that index the whole workspace from the root (rust-analyzer reads
    /// the cargo workspace from `Cargo.toml`, so every member crate is already indexed).
    fn project_marker(&self) -> Option<&'static str> {
        None
    }

    /// Collect raw diagnostic JSON (LSP `Diagnostic` values) for `uri`.
    fn collect_diagnostics(&self, client: &mut LspClient, uri: &str) -> Result<Vec<Value>>;
}

/// Resolve the preferred available backend for `lang`.
pub fn resolve_for(lang: Language) -> Result<Box<dyn Backend>> {
    let candidates: &[BackendKind] = match lang {
        Language::Rust => &[BackendKind::RustAnalyzer],
        Language::TypeScript => &[BackendKind::Tsgo, BackendKind::TypescriptLanguageServer],
    };
    candidates
        .iter()
        .copied()
        .find_map(find)
        .ok_or_else(|| anyhow!("no LSP backend installed for {lang:?}"))
}

fn find(kind: BackendKind) -> Option<Box<dyn Backend>> {
    let bin = match kind {
        BackendKind::Tsgo => "tsgo",
        BackendKind::TypescriptLanguageServer => "typescript-language-server",
        BackendKind::RustAnalyzer => "rust-analyzer",
    };
    let program = super::which(bin)?;
    Some(match kind {
        BackendKind::Tsgo => Box::new(Tsgo::new(program)),
        BackendKind::TypescriptLanguageServer => Box::new(TypescriptLanguageServer::new(program)),
        BackendKind::RustAnalyzer => Box::new(RustAnalyzer::new(program)),
    })
}

// ---- shared readiness / diagnostics strategies (used by the push-model impls) -------

/// Readiness for push-model servers: block until `uri`'s first `publishDiagnostics` arrives.
pub(crate) fn wait_for_publish(client: &mut LspClient, uri: &str) -> Result<()> {
    if client.published.contains_key(uri) {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        client.pump(Duration::from_millis(100))?;
        if client.published.contains_key(uri) {
            break;
        }
    }
    Ok(())
}

/// Diagnostics for push-model servers: cold-wait for the first publish, or warm-settle to
/// absorb the publish triggered by didChange, then return the latest.
pub(crate) fn push_diagnostics(client: &mut LspClient, uri: &str) -> Result<Vec<Value>> {
    let had_prior = client.published.contains_key(uri);
    let cold_deadline = Instant::now() + Duration::from_secs(20);
    let warm_settle = Instant::now() + Duration::from_millis(800);
    loop {
        client.pump(Duration::from_millis(100))?;
        let now = Instant::now();
        if had_prior {
            if now >= warm_settle {
                break;
            }
        } else if client.published.contains_key(uri) || now >= cold_deadline {
            break;
        }
    }
    Ok(client.published.get(uri).cloned().unwrap_or_default())
}

/// Diagnostics for pull-model servers: one request + a brief absorb of any stray publish.
/// The `ContentModified` retry in [`LspClient::request`] waits out cold indexing for us.
pub(crate) fn pull_diagnostics(client: &mut LspClient, uri: &str) -> Result<Vec<Value>> {
    let mut raw = Vec::new();
    if let Ok(report) = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
        Duration::from_secs(60),
    ) && let Some(items) = report.get("items").and_then(Value::as_array)
    {
        raw.extend(items.iter().cloned());
    }
    client.pump(Duration::from_millis(300))?;
    if let Some(pushed) = client.published.get(uri) {
        raw.extend(pushed.iter().cloned());
    }
    Ok(raw)
}
