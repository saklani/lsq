//! The agent-facing position: 1-based line, 1-based *character* column (Unicode scalar
//! values). Pure data — the translation to/from the wire's
//! [`protocol::Position`](crate::protocol::Position) lives in [`crate::coordinate`], not on this
//! type, so `agent` and `protocol` stay independent leaves.

/// A position as a human/agent types it: 1-based `line`, 1-based character `column`. The base
/// lives in the doc, not the field names — the type already distinguishes it from the wire's
/// [`protocol::Position`](crate::protocol::Position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

impl Position {
    pub fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}
