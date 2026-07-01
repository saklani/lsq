//! The agent-facing side of lsq — the vocabulary and coordinates the agent/CLI speaks.
//! Mirror of [`crate::protocol`] (the wire side); the type discipline lives at this boundary
//! because it's one we own. First resident: [`Position`], the user coordinate system.

pub mod position;

pub use position::Position;
