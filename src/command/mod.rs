//! Command handlers — one module per user-facing command.
//!
//! The clap grammar lives in [`crate::cli`]; these modules implement behaviour. Most verbs
//! just print the daemon's JSON result, so they share [`print_json`] rather than each
//! re-implementing connect → query → print.

pub mod backend;
pub mod def;
pub mod diagnostics;
pub mod help;
pub mod impl_at;
pub mod refs;
pub mod rename;
pub mod resolve;
pub mod symbols;
pub mod type_at;
pub mod type_def;

use crate::ipc::{self, Request};
use anyhow::Result;

/// Shared handler for verbs whose output is just the daemon's JSON result on stdout.
pub fn print_json(req: Request) -> Result<()> {
    let root = std::env::current_dir()?;
    let result = ipc::query(&root, &req)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
