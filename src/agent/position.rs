//! The agent/CLI-facing position: 1-based line, 1-based *character* column (Unicode scalar
//! values). Pure data — the translation to/from the wire's
//! [`protocol::Position`](crate::protocol::Position) lives in [`crate::coordinate`], not on this
//! type, so `agent` and `protocol` stay independent leaves.

/// A position as a human/agent types it: 1-based line, 1-based character column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line1: u32,
    pub col1: u32,
}

impl Position {
    pub fn new(line1: u32, col1: u32) -> Self {
        Self { line1, col1 }
    }
}
