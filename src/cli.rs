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

    /// Find all references to the symbol at a selector.
    Refs {
        /// Symbol selector, e.g. `src/lsp/mod.rs@Backend`
        selector: String,
    },

    /// Go to the definition of the symbol at a selector.
    Def {
        /// Symbol selector, e.g. `src/main.rs:42:10`
        selector: String,
    },

    /// Go to the implementation(s) of the symbol at a selector.
    Impl {
        /// Symbol selector (e.g. a trait/interface)
        selector: String,
    },

    /// Go to the type definition of the symbol at a selector.
    TypeDef {
        /// Symbol selector (e.g. a variable or field)
        selector: String,
    },

    /// Show type / hover info for the symbol at a selector.
    Type {
        /// Symbol selector
        selector: String,
    },

    /// List the symbols defined in a file.
    Symbols {
        /// Path to a source file
        file: String,
    },

    /// [internal] Run the long-lived per-workspace daemon.
    #[command(name = "__daemon", hide = true)]
    Daemon,
}
