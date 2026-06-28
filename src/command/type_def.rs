//! `lsq type-def <selector>` — go to the type definition of the symbol at a selector.

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;

pub fn run(selector: String) -> Result<()> {
    print_json(Request::TypeDef { selector })
}
