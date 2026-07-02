//! The `Adapter` trait — one implementation per language server (the per-language driver that
//! knows how to launch and talk to rust-analyzer / tsgo / ty).
//!
//! Everything that differs between servers (launch args, init options, readiness, the
//! diagnostics model) lives in the impls. `Session` stays server-agnostic: it calls
//! `adapter.await_ready(...)` before semantic queries and `adapter.collect_diagnostics(...)`
//! for diagnostics, and never branches on which server is running.

mod rust_analyzer;
mod tsgo;
mod ty;
mod typescript_language_server;

use super::Language;
use super::client::Client;
use anyhow::{Result, anyhow};
use rust_analyzer::RustAnalyzer;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tsgo::Tsgo;
use ty::Ty;
use typescript_language_server::TypescriptLanguageServer;

/// Identity of a language server, used to drive selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Tsgo,
    TypescriptLanguageServer,
    RustAnalyzer,
    Ty,
}

/// A language server lsq can drive. Implementors encapsulate all server-specific behavior.
pub trait Adapter: Send {
    fn program(&self) -> &str;
    fn args(&self) -> Vec<String>;
    /// The LSP `languageId` used in `didOpen`.
    fn language_id(&self) -> &'static str;

    /// Server-specific `initializationOptions` for the `initialize` request.
    fn initialization_options(&self) -> Value {
        Value::Null
    }

    /// Block until the server can answer SEMANTIC queries (references/rename/hover/def) for
    /// `uri`. Default: no wait — pull servers answer immediately.
    fn await_ready(&self, _client: &mut Client, _uri: &str) -> Result<()> {
        Ok(())
    }

    /// Whether `workspace/symbol` must be polled until the index is built — push/indexing
    /// servers (rust-analyzer) return an empty list while cold rather than an error.
    fn needs_index_poll(&self) -> bool {
        false
    }

    /// Collect raw diagnostic JSON (LSP `Diagnostic` values) for `uri`.
    fn collect_diagnostics(&self, client: &mut Client, uri: &str) -> Result<Vec<Value>>;

    /// Diagnostics after a buffer EDIT (for `check`). Default: same as `collect_diagnostics`,
    /// which is correct for pull servers (they re-request the current buffer). Push servers
    /// override to wait for the re-analysis to settle — they emit an empty `publishDiagnostics`
    /// first and the real one only after analysis, so returning the first publish races to 0.
    fn collect_diagnostics_after_edit(
        &self,
        client: &mut Client,
        uri: &str,
    ) -> Result<Vec<Value>> {
        self.collect_diagnostics(client, uri)
    }
}

/// Resolve the preferred available client for `lang`.
pub fn resolve_for(lang: Language) -> Result<Box<dyn Adapter>> {
    let candidates: &[AdapterKind] = match lang {
        Language::Rust => &[AdapterKind::RustAnalyzer],
        Language::TypeScript => &[AdapterKind::Tsgo, AdapterKind::TypescriptLanguageServer],
        Language::Python => &[AdapterKind::Ty],
    };
    candidates
        .iter()
        .copied()
        .find_map(find)
        .ok_or_else(|| anyhow!("no language server installed for {lang:?}"))
}

fn find(kind: AdapterKind) -> Option<Box<dyn Adapter>> {
    let bin = match kind {
        AdapterKind::Tsgo => "tsgo",
        AdapterKind::TypescriptLanguageServer => "typescript-language-server",
        AdapterKind::RustAnalyzer => "rust-analyzer",
        AdapterKind::Ty => "ty",
    };
    let program = super::which(bin)?;
    Some(match kind {
        AdapterKind::Tsgo => Box::new(Tsgo::new(program)),
        AdapterKind::TypescriptLanguageServer => Box::new(TypescriptLanguageServer::new(program)),
        AdapterKind::RustAnalyzer => Box::new(RustAnalyzer::new(program)),
        AdapterKind::Ty => Box::new(Ty::new(program)),
    })
}

// ---- shared readiness / diagnostics strategies (used by the push-model impls) -------

/// Readiness for push-model servers: block until `uri`'s first `publishDiagnostics` arrives.
pub(crate) fn wait_for_publish(client: &mut Client, uri: &str) -> Result<()> {
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
pub(crate) fn push_diagnostics(client: &mut Client, uri: &str) -> Result<Vec<Value>> {
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

/// Diagnostics after an edit for push-model servers: pump until the stream of `publishDiagnostics`
/// *settles* (no change for `QUIET`), returning the latest. Two guards make it robust against the
/// publish race after `didChange`: a `FLOOR` so re-analysis gets time to run before "quiet" can end
/// it (servers emit an empty republish first, then the real one ~seconds later), and a `MAX` cap.
pub(crate) fn settle_diagnostics(client: &mut Client, uri: &str) -> Result<Vec<Value>> {
    const QUIET: Duration = Duration::from_millis(500);
    const FLOOR: Duration = Duration::from_secs(3);
    let start = Instant::now();
    let max = start + Duration::from_secs(10);
    let mut snapshot = client.published.get(uri).cloned();
    let mut last_change = start;
    loop {
        client.pump(Duration::from_millis(150))?;
        let cur = client.published.get(uri).cloned();
        if cur != snapshot {
            snapshot = cur;
            last_change = Instant::now();
        }
        let settled = start.elapsed() >= FLOOR && last_change.elapsed() >= QUIET;
        if settled || Instant::now() >= max {
            break;
        }
    }
    Ok(snapshot.unwrap_or_default())
}

/// Diagnostics for pull-model servers: one request + a brief absorb of any stray publish.
/// The `ContentModified` retry in [`Client::request`] waits out cold indexing for us.
pub(crate) fn pull_diagnostics(client: &mut Client, uri: &str) -> Result<Vec<Value>> {
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
