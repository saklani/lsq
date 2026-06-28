//! `lsq backend` — detect and print every available LSP backend as JSON.

use crate::lsp;
use anyhow::{Result, anyhow};
use serde_json::Value;

pub fn run() -> Result<()> {
    let all = lsp::detect_all();
    if all.is_empty() {
        return Err(anyhow!(
            "no LSP backends found. Install rust-analyzer, tsgo \
             (`npm i @typescript/native-preview`), or typescript-language-server."
        ));
    }
    let arr: Vec<Value> = all.iter().map(lsp::ResolvedBackend::describe).collect();
    println!("{}", serde_json::to_string_pretty(&arr)?);
    Ok(())
}
