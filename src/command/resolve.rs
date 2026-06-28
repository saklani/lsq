//! `lsq resolve <selector>` — turn an agent-shaped selector into an exact position.
//!
//! Thin client: the actual `documentSymbol` lookup + UTF-16 math happens warm inside the
//! daemon (see [`crate::lsp::session`]). This verb mostly exists to exercise/inspect the
//! selector layer that `rename`/`refs`/`def` will all build on.

use crate::ipc::{self, Request};
use anyhow::Result;

pub fn run(selector: String) -> Result<()> {
    let root = std::env::current_dir()?;
    let result = ipc::query(&root, &Request::Resolve { selector })?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
