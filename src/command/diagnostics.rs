//! `lsq diagnostics <file>` — thin client: resolve the path, ask the workspace daemon,
//! print its shaped report. All the LSP work happens warm inside the daemon (see
//! [`crate::daemon`] and [`crate::lsp::session`]).

use crate::ipc::{self, Request};
use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn run(file: PathBuf) -> Result<()> {
    let abs = std::fs::canonicalize(&file)
        .with_context(|| format!("no such file: {}", file.display()))?;
    let root = std::env::current_dir()?;

    let result = ipc::query(
        &root,
        &Request::Diagnostics { file: abs.to_string_lossy().into_owned() },
    )?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
