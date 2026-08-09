//! Trait seams between the runtime and the outside world.
//!
//! Both sides point down at this module: domain code calls these traits,
//! adapters implement them. Neither imports the other.

pub mod discord;
pub mod stt;
pub mod wake;
