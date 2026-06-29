//! lsq — an agent-native semantic CLI for TypeScript.
//!
//! ONE binary, TWO roles:
//!   * the thin CLIENT  (`lsq diagnostics ...`, `lsq backend`, ...) — fast, short-lived
//!   * the hidden DAEMON (`lsq __daemon`)                          — long-lived, holds the
//!     warm LSP session so clients never re-pay project indexing.
//!
//! Module layout:
//!   * `cli`      — argument grammar (clap) only
//!   * `command`  — one handler module per user-facing command
//!   * `daemon`   — the long-lived per-workspace subsystem (M2)
//!   * `lsp`      — the backend seam (tsgo / typescript-language-server)
//!   * `jsonrpc`  — LSP wire framing
//!   * `position` — typed source positions

mod cli;
mod command;
mod daemon;
mod ipc;
mod jsonrpc;
mod lsp;
#[cfg(feature = "mcp")]
mod mcp;
mod position;
mod selector;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match cli.command {
        Command::Backend => command::backend::run(),
        Command::Diagnostics { file } => command::diagnostics::run(file),
        Command::Resolve { selector } => command::resolve::run(selector),
        Command::Rename {
            selector,
            new_name,
            apply,
        } => command::rename::run(selector, new_name, apply),
        Command::Fix {
            selector,
            apply,
            pick,
        } => command::fix::run(selector, apply, pick),
        Command::Refs { selector } => command::refs::run(selector),
        Command::Def { selector } => command::def::run(selector),
        Command::Impl { selector } => command::impl_at::run(selector),
        Command::TypeDef { selector } => command::type_def::run(selector),
        Command::Type { selector } => command::type_at::run(selector),
        Command::Symbols { file } => command::symbols::run(file),
        Command::Search { query } => command::search::run(query),
        Command::Callers { selector } => command::callers::run(selector),
        #[cfg(feature = "mcp")]
        Command::Mcp => mcp::serve(),
        Command::Daemon => daemon::run(),
    }
}

/// Logs go to STDERR so STDOUT remains a clean machine-readable channel (the agent
/// reads stdout). `LSQ_LOG=lsq=debug` overrides the level.
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
