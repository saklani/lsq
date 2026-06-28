//! `lsq diagnostics <file>` — ranked diagnostics for a source file. The LSP work happens
//! warm inside the daemon (see [`crate::lsp::session`]).

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;
use std::path::PathBuf;

pub fn run(file: PathBuf) -> Result<()> {
    print_json(Request::Diagnostics { file: file.to_string_lossy().into_owned() })
}
