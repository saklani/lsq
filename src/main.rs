//! lsq — agent-native semantic code queries over LSP.
//!
//! A single long-lived MCP (Model Context Protocol) server over stdio. It holds a warm LSP
//! [`Session`](lsp::session::Session) per language in-process, so project indexing is paid
//! once, and exposes IDE operations (navigation, diagnostics, edits) as MCP tools. The tool
//! set lives in [`mcp`]; the language clients in [`lsp::client`].
//!
//! Module layout:
//!   * `mcp`      — the MCP stdio front-end (the only entry point)
//!   * `engine`   — warm `Session`s + the request router
//!   * `lsp`      — the per-language client seam (one language server per language)
//!   * `jsonrpc`  — LSP wire framing
//!   * `agent` / `protocol` — the two sides lsq translates between (agent vs LSP), each with
//!     its own `Position`; `agent` also owns `Address` (structured symbol addresses)
//!   * `coordinate` — the one bridge between `agent` and `protocol` positions

mod agent;
mod coordinate;
mod engine;
mod jsonrpc;
mod lsp;
mod mcp;
mod protocol;

use anyhow::Result;

fn main() -> Result<()> {
    // No CLI surface anymore: any args (e.g. the conventional `mcp`) are ignored; `-v` raises
    // the log level. The process always serves MCP on stdio.
    let verbose = std::env::args().any(|a| a == "-v" || a == "--verbose");
    init_tracing(verbose);
    mcp::serve()
}

/// Logs go to STDERR so STDOUT stays a clean MCP transport channel. `LSQ_LOG=lsq=debug`
/// overrides the level.
fn init_tracing(verbose: bool) {
    use tracing_subscriber::EnvFilter;
    let default = if verbose { "lsq=debug" } else { "lsq=info" };
    let filter = std::env::var("LSQ_LOG")
        .ok()
        .and_then(|s| EnvFilter::try_new(s).ok())
        .unwrap_or_else(|| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .init();
}
