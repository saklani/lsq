//! rust-analyzer — push-model server. Publishes diagnostics proactively and returns empty
//! (not ContentModified) for semantic queries until indexed, so it needs a readiness wait.
//! Launched lean (proc-macro / build-scripts / check-on-save off) to stay fast and stable.

use super::{Backend, push_diagnostics, wait_for_publish};
use crate::lsp::client::LspClient;
use anyhow::Result;
use serde_json::{Value, json};

pub struct RustAnalyzer {
    program: String,
}

impl RustAnalyzer {
    pub fn new(program: String) -> Self {
        Self { program }
    }
}

impl Backend for RustAnalyzer {
    fn program(&self) -> &str {
        &self.program
    }
    fn args(&self) -> Vec<String> {
        vec![]
    }
    fn language_id(&self) -> &'static str {
        "rust"
    }

    fn initialization_options(&self) -> Value {
        json!({
            "procMacro": { "enable": false },
            "cargo": { "buildScripts": { "enable": false } },
            "checkOnSave": false,
            "check": { "workspace": false }
        })
    }

    fn await_ready(&self, client: &mut LspClient, uri: &str) -> Result<()> {
        wait_for_publish(client, uri)
    }

    fn collect_diagnostics(&self, client: &mut LspClient, uri: &str) -> Result<Vec<Value>> {
        push_diagnostics(client, uri)
    }
}
