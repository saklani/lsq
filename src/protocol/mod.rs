//! The wire-facing side of lsq — the LSP / JSON-RPC vocabulary and coordinates the language
//! servers speak. Mirror of [`crate::user`] (the agent side). First resident: [`Position`],
//! the protocol coordinate system.

pub mod position;

pub use position::Position;
