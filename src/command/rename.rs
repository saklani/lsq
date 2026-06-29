//! `lsq rename <selector> <new_name>` — semantic rename via the language server.
//!
//! Prints the resulting `WorkspaceEdit` as a unified diff (dry-run). Pass `--apply` to
//! write the edits to disk. The selector is resolved to the exact identifier position by
//! the daemon (see [`crate::lsp::session`]).

use crate::ipc::{self, Request};
use anyhow::Result;
use serde_json::Value;

pub fn run(selector: String, new_name: String, apply: bool) -> Result<()> {
    let root = std::env::current_dir()?;
    let result = ipc::query(
        &root,
        &Request::Rename {
            selector,
            new_name,
            apply,
        },
    )?;

    // The diff is the primary artifact → stdout.
    if let Some(diff) = result.get("diff").and_then(Value::as_str) {
        print!("{diff}");
    }

    // Summary + hints → stderr (keeps stdout a clean diff).
    let edits = result.get("edits").and_then(Value::as_u64).unwrap_or(0);
    let files = result
        .get("files_changed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let applied = result
        .get("applied")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    eprintln!(
        "{edits} edit(s) across {files} file(s){}",
        if applied {
            " — applied"
        } else {
            " — dry run (pass --apply to write)"
        }
    );
    if let Some(ops) = result.get("resource_operations").and_then(Value::as_array)
        && !ops.is_empty()
    {
        eprintln!(
            "note: {} resource operation(s) (e.g. file rename) were NOT auto-applied",
            ops.len()
        );
    }
    Ok(())
}
