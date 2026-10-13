//! The LSP-facing position: 0-based line, 0-based UTF-16 code-unit offset (the LSP default
//! `positionEncoding`). Pure data — the translation to/from the agent's
//! [`agent::Position`](crate::agent::Position) lives in [`crate::coordinate`], not on this type.

/// A position as LSP wants it: 0-based `line`, 0-based UTF-16 `character` offset. The base and
/// encoding live in the doc, not the field names; `character` mirrors LSP's own naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}
