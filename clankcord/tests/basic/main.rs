//! Pure-function tests: parsing, formatting, classification, config reading.
//! No Postgres, no subprocesses.

#[path = "../support/mod.rs"]
mod support;

mod agent_messages;
mod chunking;
mod codex_output;
mod codex_trace;
mod config;
mod discord_errors;
mod prompts;
mod stt;
mod wake_parsing;
