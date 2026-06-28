//! `lsq search <query>` — find where a symbol named `query` is defined, anywhere in the
//! workspace (the entry point when you know a name but not a file).

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;

pub fn run(query: String) -> Result<()> {
    print_json(Request::Search { query })
}
