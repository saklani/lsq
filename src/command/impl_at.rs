//! `lsq impl <selector>` — go to the implementation(s) of the symbol at a selector.
//! (Module is `impl_at` because `impl` is a Rust keyword.)

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;

pub fn run(selector: String) -> Result<()> {
    print_json(Request::Impl { selector })
}
