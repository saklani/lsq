//! `lsq callers <selector>` — find the callers (incoming calls) of the function at a
//! selector: the call graph "who depends on this", for blast-radius reasoning.

use super::print_json;
use crate::ipc::Request;
use anyhow::Result;

pub fn run(selector: String) -> Result<()> {
    print_json(Request::Callers { selector })
}
