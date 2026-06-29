//! Agent-shaped selectors — address a symbol WITHOUT computing UTF-16 offsets by hand.
//!
//! Three forms, parsed unambiguously:
//!   * `FILE:LINE:COL` — explicit 1-based line and character column
//!   * `FILE:LINE`     — a line; resolves to its first non-whitespace column
//!   * `FILE@NAME`     — the symbol named NAME, located via `textDocument/documentSymbol`
//!
//! The name form is the high-value one: the agent never has to know where on the line a
//! symbol sits — the language server tells us. (Bare workspace-wide names via
//! `workspace/symbol` are a planned follow-up.)

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Position { file: PathBuf, line: u32, col: u32 },
    Line { file: PathBuf, line: u32 },
    Name { file: PathBuf, name: String },
}

impl Selector {
    pub fn file(&self) -> &Path {
        match self {
            Selector::Position { file, .. }
            | Selector::Line { file, .. }
            | Selector::Name { file, .. } => file,
        }
    }

    pub fn parse(s: &str) -> Result<Selector> {
        // FILE@NAME wins — '@' never appears in the numeric forms.
        if let Some((left, name)) = s.split_once('@') {
            if left.is_empty() || name.is_empty() {
                return Err(anyhow!("`FILE@NAME` needs both a file and a symbol name"));
            }
            return Ok(Selector::Name {
                file: left.into(),
                name: name.to_string(),
            });
        }

        // Otherwise peel numeric suffixes off the right (paths here have no ':').
        let segs: Vec<&str> = s.split(':').collect();
        let n = segs.len();
        if n >= 3
            && let (Ok(line), Ok(col)) = (segs[n - 2].parse::<u32>(), segs[n - 1].parse::<u32>())
        {
            return Ok(Selector::Position {
                file: segs[..n - 2].join(":").into(),
                line,
                col,
            });
        }
        if n >= 2
            && let Ok(line) = segs[n - 1].parse::<u32>()
        {
            return Ok(Selector::Line {
                file: segs[..n - 1].join(":").into(),
                line,
            });
        }
        Err(anyhow!(
            "could not parse selector `{s}` (expected FILE:LINE:COL, FILE:LINE, or FILE@NAME)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_position() {
        assert_eq!(
            Selector::parse("src/a.rs:10:5").unwrap(),
            Selector::Position {
                file: "src/a.rs".into(),
                line: 10,
                col: 5
            }
        );
    }

    #[test]
    fn parses_line() {
        assert_eq!(
            Selector::parse("src/a.rs:10").unwrap(),
            Selector::Line {
                file: "src/a.rs".into(),
                line: 10
            }
        );
    }

    #[test]
    fn parses_name() {
        assert_eq!(
            Selector::parse("src/lsp/mod.rs@Backend").unwrap(),
            Selector::Name {
                file: "src/lsp/mod.rs".into(),
                name: "Backend".into()
            }
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(Selector::parse("just-a-name").is_err());
        assert!(Selector::parse("file@").is_err());
    }
}
