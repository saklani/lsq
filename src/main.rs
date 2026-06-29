//! lsq — agent-native semantic code queries for TypeScript and Rust over LSP.
//!
//! A single long-lived MCP (Model Context Protocol) server over stdio. It holds a warm LSP
//! [`Session`](lsp::session::Session) per language in-process, so project indexing is paid
//! once, and exposes the verbs (refs, def, impl, type, type-def, symbols, search, callers,
//! diagnostics, rename, fix, resolve) as MCP tools.
//!
//! Module layout:
//!   * `mcp`      — the MCP stdio front-end (the only entry point)
//!   * `engine`   — warm `Session`s + the request router
//!   * `lsp`      — the backend seam (rust-analyzer / tsgo / typescript-language-server)
//!   * `jsonrpc`  — LSP wire framing
//!   * `position` / `address` — typed source positions and structured symbol addresses

mod address;
mod engine;
mod jsonrpc;
mod lsp;
mod mcp;
mod position;

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
