//! tsgo — TypeScript's native (Go) compiler LSP. Pull-model: it answers `textDocument/
//! diagnostic` on demand and does NOT push `publishDiagnostics`, and it answers semantic
//! queries immediately — so it needs no readiness wait (uses the trait's no-op default).

use super::{Backend, BackendKind, pull_diagnostics};
use crate::lsp::client::LspClient;
use anyhow::Result;
use serde_json::Value;

pub struct Tsgo {
    program: String,
    version: String,
}

impl Tsgo {
    pub fn new(program: String, version: String) -> Self {
        Self { program, version }
    }
}

impl Backend for Tsgo {
    fn kind(&self) -> BackendKind {
        BackendKind::Tsgo
    }
    fn program(&self) -> &str {
        &self.program
    }
    fn args(&self) -> Vec<String> {
        vec!["--lsp".into(), "-stdio".into()]
    }
    fn version(&self) -> &str {
        &self.version
    }
    fn language_id(&self) -> &'static str {
        "typescript"
    }

    fn collect_diagnostics(&self, client: &mut LspClient, uri: &str) -> Result<Vec<Value>> {
        pull_diagnostics(client, uri)
    }
}
