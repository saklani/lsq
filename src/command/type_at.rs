//! `lsq type <selector>` — type / hover info for the symbol at a selector.
//! (Module is `type_at` because `type` is a Rust keyword.)

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;

pub fn run(selector: String) -> Result<()> {
    print_json(Request::Type { selector })
}
