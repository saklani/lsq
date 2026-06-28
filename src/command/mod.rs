//! Command handlers — one module per user-facing command.
//!
//! The clap grammar lives in [`crate::cli`]; these modules implement behaviour. Keeping
//! them split (even while small) means each command grows independently without turning
//! into one sprawling dispatch file.

pub mod backend;
pub mod diagnostics;
pub mod help;
pub mod rename;
pub mod resolve;
