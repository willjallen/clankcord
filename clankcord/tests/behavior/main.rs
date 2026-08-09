//! Subsystem behavior end-to-end against the store: sessions, automations,
//! rooms, wake, voice, dashboards, CLI.

#[path = "../support/mod.rs"]
mod support;

mod agent_sessions;
mod cli_help;
mod cli_transcripts;
mod dashboard_frontend;
mod dashboard_health;
mod dashboard_http;
mod dashboard_queries;
mod discord_slash;
mod members;
mod room_controls;
mod room_join;
mod voice_capture;
mod wake_activations;
