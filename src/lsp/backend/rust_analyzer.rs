//! rust-analyzer — push-model server. Publishes diagnostics proactively and returns empty
//! (not ContentModified) for semantic queries until indexed, so it needs a readiness wait.
//! Launched lean (proc-macro / build-scripts / check-on-save off) to stay fast and stable.

use super::{Backend, settle_diagnostics, wait_for_publish};
use crate::lsp::client::LspClient;
use anyhow::Result;
use serde_json::{Value, json};
use std::time::Duration;

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
        // `checkOnSave` runs `cargo check` — rust-analyzer's PRIMARY diagnostics source. With it
        // off, `diagnostics` returns nothing on an unbuilt crate (its native in-memory checks
        // don't fire reliably). NOTE: adding `check.workspace: false` silently disables cargo
        // check entirely, so we leave `check` at its default. procMacro/buildScripts stay off for
        // RA's in-memory analysis — cargo check runs them itself, so diagnostics stay accurate.
        json!({
            "procMacro": { "enable": false },
            "cargo": { "buildScripts": { "enable": false } },
            "checkOnSave": true
        })
    }

    fn await_ready(&self, client: &mut LspClient, uri: &str) -> Result<()> {
        wait_for_publish(client, uri)
    }

    fn needs_index_poll(&self) -> bool {
        true
    }

    fn collect_diagnostics(&self, client: &mut LspClient, uri: &str) -> Result<Vec<Value>> {
        // `didSave` re-triggers cargo check so diagnostics reflect the CURRENT disk content
        // rather than a stale earlier check (checkOnSave only fires on save). cargo check is
        // incremental — instant when nothing changed.
        client.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri } }),
        )?;
        // Wait out indexing + cargo check (whose results are the real diagnostics), not just
        // the first premature empty publish.
        client.wait_until_idle(Duration::from_millis(800), Duration::from_secs(60))?;
        Ok(client.published.get(uri).cloned().unwrap_or_default())
    }

    fn collect_diagnostics_after_edit(
        &self,
        client: &mut LspClient,
        uri: &str,
    ) -> Result<Vec<Value>> {
        settle_diagnostics(client, uri)
    }
}
