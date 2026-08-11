//! Bug pins: each test keeps a specific fixed bug dead. Files group by fix
//! family and cite the fix commits they hold in place.

#[path = "../support/mod.rs"]
mod support;

mod automation_voice_state;
mod cli_inputs;
mod job_state_classification;
mod orphan_voice_presence;
mod query_limits;
mod session_threads;
mod speech_gating;
mod transcription_recovery;
mod wake_settlement;
