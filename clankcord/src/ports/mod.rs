//! Boundary vocabulary between the runtime and the outside world: the
//! DTOs adapters produce and domain consumes, plus the [`discord`] API
//! trait the engine dispatches through. Both sides point down at this
//! module.

pub mod discord;
pub mod stt;
pub mod voice;
pub mod wake;
