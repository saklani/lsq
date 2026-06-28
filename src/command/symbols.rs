//! `lsq symbols <file>` — list the symbols defined in a file.

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;

pub fn run(file: String) -> Result<()> {
    print_json(Request::Symbols { file })
}
