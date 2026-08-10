//! Subsystem behavior end-to-end against the store: sessions, automations,
//! rooms, wake, voice, dashboards, CLI.

#[path = "../support/mod.rs"]
mod support;

mod agent_sessions;
mod automations;
mod discord_slash;
mod members;
mod room_controls;
mod room_join;
mod room_placement;
mod transcription_mux;
mod voice_capture;
mod wake_activations;
