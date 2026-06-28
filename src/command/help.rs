//! Help / about text, centralised so wording lives in one place rather than scattered
//! across clap attributes. (clap owns the `help` *subcommand*; this module owns the
//! *text* it and `--help` render.)

/// One-line description (clap `about`).
pub const ABOUT: &str = "Agent-native semantic code queries for TypeScript";

/// Longer overview (clap `long_about`, shown on `lsq --help`).
pub const LONG_ABOUT: &str = "\
lsq exposes IDE-grade semantic operations (diagnostics, references, rename, type-at-point, \
go-to-definition) for TypeScript as a fast CLI an agent can invoke like ripgrep. It drives a \
TypeScript language server (tsgo by default, typescript-language-server as fallback) and \
returns structured, token-budgeted output instead of raw text the agent must re-parse.";
