//! `lsq refs <selector>` — find all references to the symbol at a selector.

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;

pub fn run(selector: String) -> Result<()> {
    print_json(Request::Refs { selector })
}
