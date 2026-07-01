//! The LSP-facing position: 0-based line, 0-based UTF-16 code-unit offset (the LSP default
//! `positionEncoding`). Pure data — the translation to/from the agent's
//! [`user::Position`](crate::user::Position) lives in [`crate::coords`], not on this type.

/// A position as LSP wants it: 0-based line, 0-based UTF-16 code-unit offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line0: u32,
    pub char0_utf16: u32,
}
