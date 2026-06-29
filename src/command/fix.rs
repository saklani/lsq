//! `lsq fix <selector>` — code actions (quick-fixes / refactors) via the language server.
//!
//! This is the read-then-*write* verb the host's built-in LSP tool can't offer: it returns a
//! mutating `WorkspaceEdit`. Point the selector at a diagnostic (`FILE:LINE:COL`) to list the
//! fixes available there. Dry-run by default — pass `--apply` to write the chosen action's
//! edits, `--pick N` to choose among several. The action list goes to stderr (so you can see
//! the options) and the picked action's diff to stdout (keeping stdout a clean diff).

use crate::ipc::{self, Request};
use anyhow::Result;
use serde_json::Value;

pub fn run(selector: String, apply: bool, pick: usize) -> Result<()> {
    let root = std::env::current_dir()?;
    let result = ipc::query(&root, &Request::Fix { selector, apply, pick })?;

    let count = result.get("count").and_then(Value::as_u64).unwrap_or(0);
    if count == 0 {
        eprintln!("no code actions available at this position");
        return Ok(());
    }

    // The available actions → stderr, marking the one previewed below.
    let picked = result.get("picked").and_then(Value::as_u64).unwrap_or(0);
    if let Some(actions) = result.get("actions").and_then(Value::as_array) {
        eprintln!("{count} code action(s):");
        for a in actions {
            let i = a.get("index").and_then(Value::as_u64).unwrap_or(0);
            let title = a.get("title").and_then(Value::as_str).unwrap_or("");
            let kind = a.get("kind").and_then(Value::as_str).unwrap_or("");
            let mark = if i == picked { "▶" } else { " " };
            let star = if a.get("preferred").and_then(Value::as_bool).unwrap_or(false) { " *" } else { "" };
            eprintln!("  {mark} [{i}] {title}{star}{}", if kind.is_empty() { String::new() } else { format!("  ({kind})") });
        }
    }

    // The picked action's diff is the primary artifact → stdout.
    if let Some(diff) = result.get("diff").and_then(Value::as_str) {
        print!("{diff}");
    }

    let edits = result.get("edits").and_then(Value::as_u64).unwrap_or(0);
    let files = result.get("files_changed").and_then(Value::as_u64).unwrap_or(0);
    let applied = result.get("applied").and_then(Value::as_bool).unwrap_or(false);
    let title = result.get("title").and_then(Value::as_str).unwrap_or("");
    if result.get("command_only").and_then(Value::as_bool).unwrap_or(false) {
        eprintln!("\"{title}\" runs a server-side command (no text edit to apply)");
        return Ok(());
    }
    eprintln!(
        "\"{title}\": {edits} edit(s) across {files} file(s){}",
        if applied { " — applied" } else { " — dry run (pass --apply to write)" }
    );
    if let Some(ops) = result.get("resource_operations").and_then(Value::as_array) {
        if !ops.is_empty() {
            eprintln!("note: {} resource operation(s) (e.g. file create) were NOT auto-applied", ops.len());
        }
    }
    Ok(())
}
