mod events;
mod jobs;
mod maintenance;
mod store;
mod transcripts;
mod util;

pub use store::{CaptureRunInput, RenderedTranscript, SpeechEventInput, TimelineStore};
pub use util::{
    event_end, event_speaker, event_start, event_text, first_value_string, instant_ms_dt,
    instant_ms_str, isoformat_z, json_dumps, ms_to_datetime, new_id, overlaps, parse_duration,
    parse_instant, read_json_file, read_wav_mono, resolve_time_reference, sha256_file,
    string_field, utc_now, write_json_file,
};

pub(crate) use std::collections::{BTreeMap, BTreeSet};
pub(crate) use std::fs;
pub(crate) use std::path::{Path, PathBuf};

pub(crate) use anyhow::Context;
pub(crate) use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
pub(crate) use regex::Regex;
pub(crate) use rusqlite::{Connection, OptionalExtension, Row, ToSql, params, params_from_iter};
pub(crate) use serde_json::{Map, Value};
pub(crate) use sha2::{Digest, Sha256};
pub(crate) use uuid::Uuid;

pub(crate) use crate::Result;
pub(crate) use crate::config::{durable_dir, slugify};
pub(crate) use crate::runtime::Job;

pub(crate) use util::{
    SPEECH_KINDS, compact_timeline_payload, event_ended_ms, event_started_ms, excerpt,
    first_string, non_empty, payload_from_row, round3, set, set_default_string, sorted_unique,
    string_field_map, timeline_event_payload, update_value_object,
};
