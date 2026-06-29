//! MCP stdio front-end (built on the official `rmcp` SDK).
//!
//! This is the "MCP-only could delete the daemon" experiment made concrete: the MCP server
//! is itself long-lived, so it holds the warm `Session`s **in-process** (via
//! [`daemon::new_sessions`]) and reuses [`daemon::dispatch`] — no separate daemon process,
//! no unix socket. The CLI and this MCP server are two thin front-ends over the same engine.
//!
//! The engine is synchronous (LSP calls block); each async tool hands off to
//! `spawn_blocking` so the rmcp runtime stays responsive.

use crate::daemon::{self, Sessions};
use crate::ipc::Request;
use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, JsonSchema)]
struct SelectorArgs {
    /// Symbol selector: `FILE:LINE:COL`, `FILE:LINE`, or `FILE@NAME`.
    selector: String,
}

#[derive(Deserialize, JsonSchema)]
struct FileArgs {
    /// Path to a source file (.ts / .tsx / .rs).
    file: String,
}

#[derive(Deserialize, JsonSchema)]
struct QueryArgs {
    /// Symbol name (or partial) to search for across the workspace.
    query: String,
}

#[derive(Deserialize, JsonSchema)]
struct RenameArgs {
    /// Symbol selector.
    selector: String,
    /// The new name.
    new_name: String,
    /// Write edits to disk (default: dry-run, returns a diff).
    #[serde(default)]
    apply: bool,
}

#[derive(Deserialize, JsonSchema)]
struct FixArgs {
    /// Symbol selector — point at a diagnostic (`FILE:LINE:COL`).
    selector: String,
    /// Apply the chosen action's edits to disk (default: dry-run, returns a diff).
    #[serde(default)]
    apply: bool,
    /// Which action to preview/apply when several are offered (0-based).
    #[serde(default)]
    pick: usize,
}

#[derive(Clone)]
pub struct LsqMcp {
    sessions: Sessions,
    root: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl LsqMcp {
    fn new(root: PathBuf) -> Self {
        Self {
            sessions: daemon::new_sessions(),
            root,
            tool_router: Self::tool_router(),
        }
    }

    /// Run a request against the warm sessions on a blocking thread, return JSON text.
    async fn call(&self, req: Request) -> String {
        let sessions = self.sessions.clone();
        let root = self.root.clone();
        let out =
            tokio::task::spawn_blocking(move || daemon::dispatch(req, &root, &sessions)).await;
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

#[tool_router]
impl LsqMcp {
    #[tool(description = "Find all references to the symbol at a selector")]
    async fn refs(&self, Parameters(a): Parameters<SelectorArgs>) -> String {
        self.call(Request::Refs {
            selector: a.selector,
        })
        .await
    }

    #[tool(description = "Go to the definition of the symbol at a selector")]
    async fn def(&self, Parameters(a): Parameters<SelectorArgs>) -> String {
        self.call(Request::Def {
            selector: a.selector,
        })
        .await
    }

    #[tool(
        name = "impl",
        description = "Go to the implementation(s) of the symbol at a selector"
    )]
    async fn implementation(&self, Parameters(a): Parameters<SelectorArgs>) -> String {
        self.call(Request::Impl {
            selector: a.selector,
        })
        .await
    }

    #[tool(
        name = "type-def",
        description = "Go to the type definition of the symbol at a selector"
    )]
    async fn type_def(&self, Parameters(a): Parameters<SelectorArgs>) -> String {
        self.call(Request::TypeDef {
            selector: a.selector,
        })
        .await
    }

    #[tool(
        name = "type",
        description = "Show type / hover info for the symbol at a selector"
    )]
    async fn type_at(&self, Parameters(a): Parameters<SelectorArgs>) -> String {
        self.call(Request::Type {
            selector: a.selector,
        })
        .await
    }

    #[tool(description = "Resolve a selector to an exact position")]
    async fn resolve(&self, Parameters(a): Parameters<SelectorArgs>) -> String {
        self.call(Request::Resolve {
            selector: a.selector,
        })
        .await
    }

    #[tool(description = "Report diagnostics for a source file")]
    async fn diagnostics(&self, Parameters(a): Parameters<FileArgs>) -> String {
        self.call(Request::Diagnostics { file: a.file }).await
    }

    #[tool(description = "List the symbols defined in a file")]
    async fn symbols(&self, Parameters(a): Parameters<FileArgs>) -> String {
        self.call(Request::Symbols { file: a.file }).await
    }

    #[tool(description = "Search for a symbol by name across the whole workspace")]
    async fn search(&self, Parameters(a): Parameters<QueryArgs>) -> String {
        self.call(Request::Search { query: a.query }).await
    }

    #[tool(description = "Find callers (incoming calls) of the function at a selector")]
    async fn callers(&self, Parameters(a): Parameters<SelectorArgs>) -> String {
        self.call(Request::Callers {
            selector: a.selector,
        })
        .await
    }

    #[tool(
        description = "Rename the symbol at a selector across the workspace (dry-run unless apply=true)"
    )]
    async fn rename(&self, Parameters(a): Parameters<RenameArgs>) -> String {
        self.call(Request::Rename {
            selector: a.selector,
            new_name: a.new_name,
            apply: a.apply,
        })
        .await
    }

    #[tool(
        description = "List/apply code actions (quick-fixes, refactors) at a selector (dry-run unless apply=true)"
    )]
    async fn fix(&self, Parameters(a): Parameters<FixArgs>) -> String {
        self.call(Request::Fix {
            selector: a.selector,
            apply: a.apply,
            pick: a.pick,
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LsqMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info.name = "lsq".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.instructions = Some(
            "Semantic code queries for TypeScript and Rust over LSP: refs, def, impl, type, \
             type-def, symbols, search, callers, diagnostics, rename, fix, resolve. Selectors \
             are FILE:LINE:COL, FILE:LINE, or FILE@NAME. Run from the workspace root."
                .into(),
        );
        info
    }
}

/// Serve the MCP protocol on stdio. Blocks until the client disconnects.
pub fn serve() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let root = std::env::current_dir()?;
        let service = LsqMcp::new(root).serve(stdio()).await?;
        service.waiting().await?;
        Ok::<_, anyhow::Error>(())
    })
}
