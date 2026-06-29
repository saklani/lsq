//! MCP stdio front-end (built on the official `rmcp` SDK).
//!
//! This is the "MCP-only could delete the daemon" experiment made concrete: the MCP server
//! is itself long-lived, so it holds the warm `Session`s **in-process** (via
//! [`daemon::new_sessions`]) and reuses [`daemon::dispatch`] — no separate daemon process,
//! no unix socket. The CLI and this MCP server are two thin front-ends over the same engine.
//!
//! The engine is synchronous (LSP calls block); each async tool hands off to
//! `spawn_blocking` so the rmcp runtime stays responsive.

use crate::engine::{self, Request, Sessions};
use crate::selector::Selector;
use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;

/// Structured symbol address: a `file` plus EITHER a `symbol` name OR a `line` (+ optional `col`).
#[derive(Deserialize, JsonSchema)]
struct Target {
    /// Path to the source file.
    file: String,
    /// Address by symbol NAME — resolved by the language server (no line/column needed, and
    /// durable across edits). Provide this OR a `line`.
    #[serde(default)]
    symbol: Option<String>,
    /// Address by 1-based line — with `col` for an exact position, or alone for the line's first symbol.
    #[serde(default)]
    line: Option<u32>,
    /// 1-based column (used with `line`).
    #[serde(default)]
    col: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
struct FileArgs {
    /// Path to a source file (.ts / .tsx / .rs).
    file: String,
}

#[derive(Deserialize, JsonSchema)]
struct QueryArgs {
    /// Symbol name (or partial) to search for across the project.
    query: String,
}

#[derive(Deserialize, JsonSchema)]
struct FormatArgs {
    /// Path to the source file to format.
    file: String,
    /// Write the formatted result to disk (default: dry-run, returns a diff).
    #[serde(default)]
    apply: bool,
}

#[derive(Deserialize, JsonSchema)]
struct CheckArgs {
    /// Path to the source file to check.
    file: String,
    /// The full proposed file content. Applied in-memory only — the file on disk is untouched.
    content: String,
}

#[derive(Deserialize, JsonSchema)]
struct RenameArgs {
    #[serde(flatten)]
    target: Target,
    /// The new name.
    new_name: String,
    /// Write edits to disk (default: dry-run, returns a diff).
    #[serde(default)]
    apply: bool,
}

#[derive(Deserialize, JsonSchema)]
struct FixArgs {
    #[serde(flatten)]
    target: Target,
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
            sessions: engine::new_sessions(),
            root,
            tool_router: Self::tool_router(),
        }
    }

    /// Build a `Selector` from a structured target, then dispatch — a build error (e.g. neither a
    /// symbol nor a line given) is surfaced as JSON rather than a protocol error.
    async fn call_target(&self, t: Target, make: impl FnOnce(Selector) -> Request) -> String {
        match Selector::new(t.file, t.symbol, t.line, t.col) {
            Ok(selector) => self.call(make(selector)).await,
            Err(e) => err_json(&format!("{e:#}")),
        }
    }

    /// Run a request against the warm sessions on a blocking thread, return JSON text.
    async fn call(&self, req: Request) -> String {
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

#[tool_router]
impl LsqMcp {
    #[tool(description = "Find all references to the symbol at a target (file + symbol|line)")]
    async fn refs(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Refs { selector })
            .await
    }

    #[tool(description = "Go to the definition of the symbol at a target")]
    async fn def(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Def { selector })
            .await
    }

    #[tool(
        name = "impl",
        description = "Go to the implementation(s) of the symbol at a target"
    )]
    async fn implementation(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Impl { selector })
            .await
    }

    #[tool(
        name = "type-def",
        description = "Go to the type definition of the symbol at a target"
    )]
    async fn type_def(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::TypeDef { selector })
            .await
    }

    #[tool(
        name = "type",
        description = "Show type / hover info for the symbol at a target"
    )]
    async fn type_at(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Type { selector })
            .await
    }

    #[tool(description = "Resolve a target to an exact position")]
    async fn resolve(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Resolve { selector })
            .await
    }

    #[tool(description = "Report diagnostics for a source file")]
    async fn diagnostics(&self, Parameters(a): Parameters<FileArgs>) -> String {
        self.call(Request::Diagnostics { file: a.file }).await
    }

    #[tool(
        description = "Preview diagnostics for a speculative edit: apply the full proposed `content` \
                       to the file in-memory (NOT written to disk) and return the diagnostics it \
                       would produce. Use to check what an edit breaks before committing it."
    )]
    async fn check(&self, Parameters(a): Parameters<CheckArgs>) -> String {
        self.call(Request::Check {
            file: a.file,
            content: a.content,
        })
        .await
    }

    #[tool(description = "List the symbols defined in a file")]
    async fn symbols(&self, Parameters(a): Parameters<FileArgs>) -> String {
        self.call(Request::Symbols { file: a.file }).await
    }

    #[tool(
        description = "Search for where a symbol named `query` is defined across the project. \
                       Single-project: in a TS monorepo covers only one package (Rust cargo \
                       workspaces are fully covered)."
    )]
    async fn search(&self, Parameters(a): Parameters<QueryArgs>) -> String {
        self.call(Request::Search { query: a.query }).await
    }

    #[tool(description = "Find callers (incoming calls) of the function at a target")]
    async fn callers(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Callers { selector })
            .await
    }

    #[tool(
        description = "Blast radius of the symbol at a target — references + callers + \
                       implementations in one answer, with the total distinct files touched. \
                       Use to gauge what depends on a symbol before changing it."
    )]
    async fn impact(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Impact { selector })
            .await
    }

    #[tool(description = "Go to the declaration of the symbol at a target")]
    async fn declaration(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Declaration { selector })
            .await
    }

    #[tool(description = "Find callees (outgoing calls) of the function at a target")]
    async fn callees(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Callees { selector })
            .await
    }

    #[tool(
        description = "Occurrences of the symbol at a target within its file (file-scoped refs)"
    )]
    async fn highlight(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Highlight { selector })
            .await
    }

    #[tool(description = "Signature / parameter help for the call at a target")]
    async fn signature(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Signature { selector })
            .await
    }

    #[tool(
        description = "Completions available at a target (what can go here / what a value exposes)"
    )]
    async fn complete(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |selector| Request::Complete { selector })
            .await
    }

    #[tool(description = "Format a source file (prints a unified diff; pass apply=true to write)")]
    async fn format(&self, Parameters(a): Parameters<FormatArgs>) -> String {
        self.call(Request::Format {
            file: a.file,
            apply: a.apply,
        })
        .await
    }

    #[tool(
        description = "Rename the symbol at a target across the workspace (dry-run unless apply=true)"
    )]
    async fn rename(&self, Parameters(a): Parameters<RenameArgs>) -> String {
        let (new_name, apply) = (a.new_name, a.apply);
        self.call_target(a.target, move |selector| Request::Rename {
            selector,
            new_name,
            apply,
        })
        .await
    }

    #[tool(
        description = "List/apply code actions (quick-fixes, refactors) at a target (dry-run unless apply=true)"
    )]
    async fn fix(&self, Parameters(a): Parameters<FixArgs>) -> String {
        let (apply, pick) = (a.apply, a.pick);
        self.call_target(a.target, move |selector| Request::Fix {
            selector,
            apply,
            pick,
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
            "Semantic code queries for TypeScript, Rust, and Python over LSP. Navigation: refs, \
             def, declaration, impl, type-def, type, symbols, search, highlight, callers, \
             callees, impact. Writing aids: signature, complete. Diagnostics: diagnostics, \
             check (previews an unsaved edit's diagnostics). Edits: rename, fix, format (each \
             dry-run unless apply=true). `impact` is a symbol's blast radius; `search` is \
             single-project (not a TS monorepo). Selectors are FILE:LINE:COL, FILE:LINE, or \
             FILE@NAME. Run from the workspace root."
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
