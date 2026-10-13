//! Structured symbol addressing — how the agent names a location WITHOUT hand-computing
//! UTF-16 offsets. An [`Address`] is a resident of the agent vocabulary: its coordinates are
//! [`agent::Position`](super::Position) (1-based, character columns), not wire coordinates.
//!
//! It is a pure sum type: one of three valid shapes for "where in the code." Every variant is
//! valid by construction, so there is no fallible constructor here — turning loose request
//! fields into an `Address` (and rejecting incoherent ones) is *input validation*, and lives
//! at the boundary that owns the wire format, not on this type.
//!
//! Because an `Address` carries the path as a plain `file` field (never split out of a
//! `FILE@NAME` string), an `@` / `:` / `::` in a path is just data — a whole class of parse bug
//! cannot occur. The three shapes:
//!
//!   * **by name** — the symbol named NAME, located via `textDocument/documentSymbol`. The
//!     high-value form: the agent never has to know where on the line a symbol sits, and it's
//!     *durable across edits* (re-resolved every call), unlike a line/column which one edit voids.
//!   * **by position** — an explicit [`agent::Position`](super::Position) (1-based line + column).
//!   * **by line** — a 1-based line alone (resolves to its first non-whitespace column).

use super::Position;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Address {
    Position { file: PathBuf, pos: Position },
    Line { file: PathBuf, line1: u32 },
    Name { file: PathBuf, name: String },
}

impl Address {
    pub fn file(&self) -> &Path {
        match self {
            Self::Position { file, .. } | Self::Line { file, .. } | Self::Name { file, .. } => file,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_is_returned_for_every_variant() {
        // a path with `@` in it round-trips untouched — it's just the `file` field, never parsed.
        let p = Path::new("src/@types/x.ts");
        assert_eq!(
            Address::Position {
                file: p.into(),
                pos: Position::new(10, 5)
            }
            .file(),
            p
        );
        assert_eq!(
            Address::Line {
                file: p.into(),
                line1: 10
            }
            .file(),
            p
        );
        assert_eq!(
            Address::Name {
                file: p.into(),
                name: "Ky".into()
            }
            .file(),
            p
        );
    }
}
