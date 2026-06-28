//! Argument grammar (clap) only. Behaviour lives in [`crate::command`].

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "lsq",
    version,
    about = crate::command::help::ABOUT,
    long_about = crate::command::help::LONG_ABOUT
)]
pub struct Cli {
    /// Verbose logging (also: LSQ_LOG=lsq=debug)
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Detect and describe the TypeScript LSP backend (prefers tsgo).
    Backend,

    /// Report diagnostics for a source file (TypeScript or Rust).
    Diagnostics {
        /// Path to a .ts / .tsx / .rs file
        file: PathBuf,
    },

    /// Resolve a selector (FILE:LINE:COL, FILE:LINE, or FILE@NAME) to an exact position.
    Resolve {
        /// e.g. `src/lsp/mod.rs@Backend`
        selector: String,
    },

    /// Rename the symbol at a selector across the workspace (prints a diff; --apply writes).
    Rename {
        /// Symbol selector, e.g. `src/lsp/mod.rs@ResolvedBackend`
        selector: String,
        /// The new name
        new_name: String,
        /// Write the edits to disk (default: dry-run, just print the diff)
        #[arg(long)]
        apply: bool,
    },

    /// [internal] Run the long-lived per-workspace daemon.
    #[command(name = "__daemon", hide = true)]
    Daemon,
}
