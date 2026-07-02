//! typescript-language-server — the older Node wrapper around tsserver. Push-model (like
//! rust-analyzer), kept as a fallback for environments without tsgo. Untested vs the verbs;
//! present mainly to exercise the trait's substitutability.

use super::{Adapter, push_diagnostics, settle_diagnostics, wait_for_publish};
use crate::protocol::client::Client;
use anyhow::Result;
use serde_json::Value;

pub struct TypescriptLanguageServer {
    program: String,
}

impl TypescriptLanguageServer {
    pub fn new(program: String) -> Self {
        Self { program }
    }
}

impl Adapter for TypescriptLanguageServer {
    fn program(&self) -> &str {
        &self.program
    }
    fn args(&self) -> Vec<String> {
        vec!["--stdio".into()]
    }
    fn language_id(&self) -> &'static str {
        "typescript"
    }

    fn await_ready(&self, client: &mut Client, uri: &str) -> Result<()> {
        wait_for_publish(client, uri)
    }

    fn needs_index_poll(&self) -> bool {
        true
    }

    fn collect_diagnostics(&self, client: &mut Client, uri: &str) -> Result<Vec<Value>> {
        push_diagnostics(client, uri)
    }

    fn collect_diagnostics_after_edit(
        &self,
        client: &mut Client,
        uri: &str,
    ) -> Result<Vec<Value>> {
        settle_diagnostics(client, uri)
    }
}
