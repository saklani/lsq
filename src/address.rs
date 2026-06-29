//! Structured symbol addressing — point at a location WITHOUT computing UTF-16 offsets by hand.
//!
//! A [`Address`] is built from explicit fields, never parsed from a single string — so there's
//! no ambiguity from `@` / `:` / `::` appearing in a file path (a whole class of bug that a
//! `FILE@NAME`-style string would have). Two ways to address a symbol:
//!
//!   * **by name** — the symbol named NAME, located via `textDocument/documentSymbol`. The
//!     high-value form: the agent never has to know where on the line a symbol sits, and it's
//!     *durable across edits* (re-resolved every call), unlike a line/column which one edit voids.
//!   * **by position** — an explicit 1-based line, with an optional column (the line alone
//!     resolves to its first non-whitespace column).

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Position { file: PathBuf, line: u32, col: u32 },
    Line { file: PathBuf, line: u32 },
    Name { file: PathBuf, name: String },
}

impl Address {
    pub fn file(&self) -> &Path {
        match self {
            Address::Position { file, .. }
            | Address::Line { file, .. }
            | Address::Name { file, .. } => file,
        }
    }

    /// Build an address from structured fields: a non-empty `symbol` (a name) takes precedence;
    /// otherwise a `line` with an optional `col` gives a position / line address. Errors if
    /// neither a symbol nor a line is supplied.
    pub fn new(
        file: impl Into<PathBuf>,
        symbol: Option<String>,
        line: Option<u32>,
        col: Option<u32>,
    ) -> Result<Address> {
        let file = file.into();
        if let Some(name) = symbol.filter(|s| !s.is_empty()) {
            return Ok(Address::Name { file, name });
        }
        match (line, col) {
            (Some(line), Some(col)) => Ok(Address::Position { file, line, col }),
            (Some(line), None) => Ok(Address::Line { file, line }),
            _ => Err(anyhow!(
                "address a symbol by `symbol` (a name), or by `line` (with optional `col`)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_takes_precedence_over_position() {
        assert_eq!(
            Address::new("a.rs", Some("Backend".into()), Some(9), Some(9)).unwrap(),
            Address::Name {
                file: "a.rs".into(),
                name: "Backend".into()
            }
        );
    }

    #[test]
    fn position_from_line_and_col() {
        assert_eq!(
            Address::new("a.rs", None, Some(10), Some(5)).unwrap(),
            Address::Position {
                file: "a.rs".into(),
                line: 10,
                col: 5
            }
        );
    }

    #[test]
    fn line_only() {
        assert_eq!(
            Address::new("a.rs", None, Some(10), None).unwrap(),
            Address::Line {
                file: "a.rs".into(),
                line: 10
            }
        );
    }

    #[test]
    fn needs_a_symbol_or_a_line() {
        assert!(Address::new("a.rs", Some(String::new()), None, None).is_err());
        assert!(Address::new("a.rs", None, None, None).is_err());
        assert!(Address::new("a.rs", None, None, Some(5)).is_err()); // col without line
    }

    #[test]
    fn at_in_path_is_a_nonissue() {
        // the whole `@`-in-path bug is gone: the path is just the `file` field, never parsed
        assert_eq!(
            Address::new("src/@types/x.ts", None, Some(10), Some(5)).unwrap(),
            Address::Position {
                file: "src/@types/x.ts".into(),
                line: 10,
                col: 5
            }
        );
    }
}
