//! The `Server` runtime: the warm per-language sessions + workspace root, the bridge that runs
//! a [`Request`] on a blocking thread, and the `ServerHandler` wiring rmcp needs. The tool
//! surface that builds those requests lives in [`super::tools`]; the input types in
//! [`super::args`].

use super::args::Target;
use crate::agent::Address;
use crate::engine::{self, Request, Sessions};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool_handler};
use std::path::PathBuf;

#[derive(Clone)]
pub(crate) struct Server {
    sessions: Sessions,
    root: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl Server {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            sessions: engine::new_sessions(),
            root,
            tool_router: Self::tool_router(),
        }
    }

    /// Build an `Address` from a structured target, then dispatch — a build error (e.g. neither a
    /// symbol nor a line given) is surfaced as JSON rather than a protocol error.
    pub(crate) async fn call_target(
        &self,
        t: Target,
        make: impl FnOnce(Address) -> Request,
    ) -> String {
        match t.resolve() {
            Ok(address) => self.call(make(address)).await,
            Err(e) => err_json(&format!("{e:#}")),
        }
    }

    /// Run a request against the warm sessions on a blocking thread, return JSON text.
    pub(crate) async fn call(&self, req: Request) -> String {
        let sessions = self.sessions.clone();
        let root = self.root.clone();
        let out =
            tokio::task::spawn_blocking(move || engine::dispatch(req, &root, &sessions)).await;
        match out {
            Ok(Ok(v)) => {
                serde_json::to_string_pretty(&v).unwrap_or_else(|e| err_json(&e.to_string()))
            }
            Ok(Err(e)) => err_json(&format!("{e:#}")),
            Err(e) => err_json(&format!("task panicked: {e}")),
        }
    }
}

fn err_json(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info.name = "lsq".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.instructions = Some(
            "Semantic code queries for TypeScript, Rust, and Python over LSP. Navigation: refs, \
             def, declaration, impl, type-def, type, symbols, search, highlight, callers, \
             callees, impact. Writing aids: signature, complete. Diagnostics: diagnostics, \
             check (previews an unsaved edit's diagnostics). Edits: rename, fix, format (each \
             dry-run unless apply=true). Maintenance: reload (drop warm servers to clear a stale \
             index after files move/create/delete). `impact` is a symbol's blast radius; `search` \
             is single-project (not a TS monorepo). Address a symbol with a `file` plus either a \
             `symbol` name (preferred — durable across edits) or a `line`/`column`. Run from the \
             workspace root."
                .into(),
        );
        info
    }
}
