//! ty — Astral's native (Rust) Python type checker, run as a language server (`ty server`).
//! Pull-model like tsgo: it answers `textDocument/diagnostic` on demand and semantic queries
//! immediately, so it needs no readiness wait (uses the trait's no-op defaults). Installed via
//! `uv tool install ty`. It does NOT implement go-to-implementation, so `impl` returns empty for
//! Python — which is fine (interfaces aren't a Python concept; `impact` just gets 0 impls).

use super::{Adapter, pull_diagnostics};
use crate::lsp::client::Client;
use anyhow::Result;
use serde_json::Value;

pub struct Ty {
    program: String,
}

impl Ty {
    pub fn new(program: String) -> Self {
        Self { program }
    }
}

impl Adapter for Ty {
    fn program(&self) -> &str {
        &self.program
    }
    fn args(&self) -> Vec<String> {
        vec!["server".into()]
    }
    fn language_id(&self) -> &'static str {
        "python"
    }

    fn collect_diagnostics(&self, client: &mut Client, uri: &str) -> Result<Vec<Value>> {
        pull_diagnostics(client, uri)
    }
}
