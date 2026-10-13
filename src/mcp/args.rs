//! The input-parsing layer — the wire boundary. Each MCP tool takes one of these structs,
//! deserialized from the loose JSON arguments by rmcp. The one hand-written parse is
//! [`Target::resolve`]: it turns a loose `Target` (all-optional address fields) into a
//! validated [`Address`], rejecting incoherent shapes. Everything downstream consumes the
//! typed `Address` — this is the only place an address can be malformed.

use crate::agent::{Address, Position};
use anyhow::{Result, anyhow};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;

/// Structured symbol address: a `file` plus EITHER a `symbol` name OR a `line` (+ optional `column`).
#[derive(Deserialize, JsonSchema)]
pub(crate) struct Target {
    /// Path to the source file.
    file: String,
    /// Address by symbol NAME — resolved by the language server (no line/column needed, and
    /// durable across edits). Provide this OR a `line`.
    #[serde(default)]
    symbol: Option<String>,
    /// Address by 1-based line — with `column` for an exact position, or alone for the line's first symbol.
    #[serde(default)]
    line: Option<u32>,
    /// 1-based column (used with `line`).
    #[serde(default)]
    column: Option<u32>,
}

impl Target {
    /// The input-validation step: parse the loose wire fields into a validated [`Address`].
    /// A non-empty `symbol` wins; otherwise a `line` (with optional `column`); otherwise an error.
    /// This is where incoherent input (a file with no symbol and no line) is rejected — the
    /// `Address` type itself has no fallible constructor.
    pub(crate) fn resolve(self) -> Result<Address> {
        let Target {
            file,
            symbol,
            line,
            column,
        } = self;
        let file = PathBuf::from(file);
        if let Some(name) = symbol.filter(|s| !s.is_empty()) {
            return Ok(Address::Name { file, name });
        }
        match (line, column) {
            (Some(line), Some(column)) => Ok(Address::Position {
                file,
                pos: Position::new(line, column),
            }),
            (Some(line), None) => Ok(Address::Line { file, line1: line }),
            _ => Err(anyhow!(
                "address a symbol by `symbol` (a name), or by `line` (with optional `column`)"
            )),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct FileArgs {
    /// Path to a source file (.ts / .tsx / .rs).
    pub(crate) file: String,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct QueryArgs {
    /// Symbol name (or partial) to search for across the project.
    pub(crate) query: String,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ReloadArgs {
    /// Reload only the language server for this file's language. Omit to reload ALL warm servers.
    #[serde(default)]
    pub(crate) file: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct FormatArgs {
    /// Path to the source file to format.
    pub(crate) file: String,
    /// Write the formatted result to disk (default: dry-run, returns a diff).
    #[serde(default)]
    pub(crate) apply: bool,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct CheckArgs {
    /// Path to the source file to check.
    pub(crate) file: String,
    /// The full proposed file content. Applied in-memory only — the file on disk is untouched.
    pub(crate) content: String,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct RenameArgs {
    #[serde(flatten)]
    pub(crate) target: Target,
    /// The new name.
    pub(crate) new_name: String,
    /// Write edits to disk (default: dry-run, returns a diff).
    #[serde(default)]
    pub(crate) apply: bool,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct FixArgs {
    #[serde(flatten)]
    pub(crate) target: Target,
    /// Apply the chosen action's edits to disk (default: dry-run, returns a diff).
    #[serde(default)]
    pub(crate) apply: bool,
    /// Which action to preview/apply when several are offered (0-based).
    #[serde(default)]
    pub(crate) pick: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(symbol: Option<&str>, line: Option<u32>, column: Option<u32>) -> Target {
        Target {
            file: "a.rs".into(),
            symbol: symbol.map(Into::into),
            line,
            column,
        }
    }

    #[test]
    fn name_takes_precedence_over_position() {
        assert_eq!(
            target(Some("Backend"), Some(9), Some(9)).resolve().unwrap(),
            Address::Name {
                file: "a.rs".into(),
                name: "Backend".into()
            }
        );
    }

    #[test]
    fn position_from_line_and_col() {
        assert_eq!(
            target(None, Some(10), Some(5)).resolve().unwrap(),
            Address::Position {
                file: "a.rs".into(),
                pos: Position::new(10, 5)
            }
        );
    }

    #[test]
    fn line_only() {
        assert_eq!(
            target(None, Some(10), None).resolve().unwrap(),
            Address::Line {
                file: "a.rs".into(),
                line1: 10
            }
        );
    }

    #[test]
    fn incoherent_input_is_rejected() {
        assert!(target(Some(""), None, None).resolve().is_err()); // empty symbol isn't a name
        assert!(target(None, None, None).resolve().is_err()); // nothing to point at
        assert!(target(None, None, Some(5)).resolve().is_err()); // column without line
    }
}
