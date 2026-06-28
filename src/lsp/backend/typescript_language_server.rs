//! typescript-language-server — the older Node wrapper around tsserver. Push-model (like
//! rust-analyzer), kept as a fallback for environments without tsgo. Untested vs the verbs;
//! present mainly to exercise the trait's substitutability.

use super::{Backend, BackendKind, push_diagnostics, wait_for_publish};
use crate::lsp::client::LspClient;
use anyhow::Result;
use serde_json::Value;

pub struct TypescriptLanguageServer {
    program: String,
    version: String,
}

impl TypescriptLanguageServer {
    pub fn new(program: String, version: String) -> Self {
        Self { program, version }
    }
}

impl Backend for TypescriptLanguageServer {
    fn kind(&self) -> BackendKind {
        BackendKind::TypescriptLanguageServer
    }
    fn program(&self) -> &str {
        &self.program
    }
    fn args(&self) -> Vec<String> {
        vec!["--stdio".into()]
    }
    fn version(&self) -> &str {
        &self.version
    }
    fn language_id(&self) -> &'static str {
        "typescript"
    }

    fn await_ready(&self, client: &mut LspClient, uri: &str) -> Result<()> {
        wait_for_publish(client, uri)
    }

    fn needs_index_poll(&self) -> bool {
        true
    }

    fn collect_diagnostics(&self, client: &mut LspClient, uri: &str) -> Result<Vec<Value>> {
        push_diagnostics(client, uri)
    }
}
