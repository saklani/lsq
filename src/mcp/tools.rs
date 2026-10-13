//! The MCP tool surface: one thin `#[tool]` per verb. Each takes its typed arguments (see
//! [`super::args`]), builds a [`Request`], and hands it to the [`Server`] runtime (see
//! [`super::server`]). No logic lives here — the router is the front door, nothing else.

use super::args::{
    CheckArgs, FileArgs, FixArgs, FormatArgs, QueryArgs, ReloadArgs, RenameArgs, Target,
};
use super::server::Server;
use crate::engine::Request;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

#[tool_router(vis = "pub(crate)")]
impl Server {
    #[tool(description = "Find all references to the symbol at a target (file + symbol|line)")]
    async fn refs(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Refs { address })
            .await
    }

    #[tool(description = "Go to the definition of the symbol at a target")]
    async fn def(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Def { address })
            .await
    }

    #[tool(
        name = "impl",
        description = "Go to the implementation(s) of the symbol at a target"
    )]
    async fn implementation(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Impl { address })
            .await
    }

    #[tool(
        name = "type-def",
        description = "Go to the type definition of the symbol at a target"
    )]
    async fn type_def(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::TypeDef { address })
            .await
    }

    #[tool(
        name = "type",
        description = "Show type / hover info for the symbol at a target"
    )]
    async fn type_at(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Type { address })
            .await
    }

    #[tool(description = "Resolve a target to an exact position")]
    async fn resolve(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Resolve { address })
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
        self.call_target(t, |address| Request::Callers { address })
            .await
    }

    #[tool(
        description = "Blast radius of the symbol at a target — references + callers + \
                       implementations in one answer, with the total distinct files touched. \
                       Use to gauge what depends on a symbol before changing it."
    )]
    async fn impact(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Impact { address })
            .await
    }

    #[tool(description = "Go to the declaration of the symbol at a target")]
    async fn declaration(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Declaration { address })
            .await
    }

    #[tool(description = "Find callees (outgoing calls) of the function at a target")]
    async fn callees(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Callees { address })
            .await
    }

    #[tool(
        description = "Occurrences of the symbol at a target within its file (file-scoped refs)"
    )]
    async fn highlight(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Highlight { address })
            .await
    }

    #[tool(description = "Signature / parameter help for the call at a target")]
    async fn signature(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Signature { address })
            .await
    }

    #[tool(
        description = "Completions available at a target (what can go here / what a value exposes)"
    )]
    async fn complete(&self, Parameters(t): Parameters<Target>) -> String {
        self.call_target(t, |address| Request::Complete { address })
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
        description = "Drop warm language server(s) so they re-index from current disk on the next \
                       request. Use after moving/creating/deleting files to clear a stale index \
                       (cross-file ops like rename/refs/search rely on it). Omit `file` to reload \
                       every language; pass a `file` to reload only its language."
    )]
    async fn reload(&self, Parameters(a): Parameters<ReloadArgs>) -> String {
        self.call(Request::Reload { file: a.file }).await
    }

    #[tool(
        description = "Rename the symbol at a target across the workspace (dry-run unless apply=true)"
    )]
    async fn rename(&self, Parameters(a): Parameters<RenameArgs>) -> String {
        let (new_name, apply) = (a.new_name, a.apply);
        self.call_target(a.target, move |address| Request::Rename {
            address,
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
        self.call_target(a.target, move |address| Request::Fix {
            address,
            apply,
            pick,
        })
        .await
    }
}
