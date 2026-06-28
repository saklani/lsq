//! `lsq resolve <selector>` — turn an agent-shaped selector into an exact position.
//! Mostly exists to inspect the selector layer that `rename`/`refs`/`def`/`type` build on.

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;

pub fn run(selector: String) -> Result<()> {
    print_json(Request::Resolve { selector })
}
