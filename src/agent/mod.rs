//! The agent-facing side of lsq — the vocabulary and coordinates the agent/CLI speaks.
//! Mirror of [`crate::protocol`] (the wire side); the type discipline lives at this boundary
//! because it's one we own. Residents: [`Position`] (the agent coordinate system) and
//! [`Address`] (how the agent names a location, in agent coordinates).

pub mod address;
pub mod position;

pub use address::Address;
pub use position::Position;
