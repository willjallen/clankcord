use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use reqwest::blocking::multipart;
use serde_json::Value;

use crate::Result;
use crate::adapters::discord::api::read_secret_value;
use crate::runtime::timeline::{TimelineStore, isoformat_z, parse_instant, write_json_file};
use crate::runtime::{Job, JobOutput, JobState};

pub const ELEVENLABS_STT_URL: &str = "https://api.elevenlabs.io/v1/speech-to-text";

pub fn elevenlabs_api_key() -> String {
    read_secret_value("ELEVENLABS_API_KEY", "ELEVENLABS_API_KEY_FILE", "")
}

pub fn submit_elevenlabs_stt(
    audio_path: &Path,
    model_id: &str,
    diarize: bool,
    timestamps_granularity: &str,
    num_speakers: Option<usize>,
    webhook_metadata: Option<Value>,
) -> Result<Value> {
    let api_key = elevenlabs_api_key();
    if api_key.is_empty() {
        anyhow::bail!("ELEVENLABS_API_KEY is not configured");
    }
    let mut form = multipart::Form::new()
        .text("model_id", model_id.to_string())
        .text(
            "diarize",
            if diarize { "true" } else { "false" }.to_string(),
        )
        .text("timestamps_granularity", timestamps_granularity.to_string());
    if let Some(count) = num_speakers.filter(|count| *count > 0) {
        form = form.text("num_speakers", count.to_string());
    }
    let webhook_url = env::var("ELEVENLABS_STT_WEBHOOK_URL").unwrap_or_default();
    if !webhook_url.trim().is_empty() {
        form = form
            .text("webhook", "true")
            .text("webhook_url", webhook_url);
        if let Some(metadata) = webhook_metadata {
            form = form.text("webhook_metadata", serde_json::to_string(&metadata)?);
        }
    }
    let bytes = fs::read(audio_path)?;
    let part = multipart::Part::bytes(bytes)
        .file_name(
            audio_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .mime_str("audio/wav")?;
    let form = form.part("file", part);
    let timeout = env::var("ELEVENLABS_STT_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600);
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()?
        .post(ELEVENLABS_STT_URL)
        .header("xi-api-key", api_key)
        .multipart(form)
        .send()?
        .error_for_status()?;
    let payload: Value = response.json()?;
    Ok(if payload.is_object() {
        payload
    } else {
        serde_json::json!({})
    })
}

pub fn provider_words(payload: &Value) -> Vec<Value> {
    if let Some(words) = payload.get("words").and_then(Value::as_array) {
        return words
            .iter()
            .filter(|word| word.is_object())
            .cloned()
            .collect();
    }
    let mut collected = Vec::new();
    if let Some(segments) = payload.get("segments").and_then(Value::as_array) {
        for segment in segments {
            let Some(segment_map) = segment.as_object() else {
                continue;
            };
            if let Some(words) = segment.get("words").and_then(Value::as_array) {
                for word in words.iter().filter(|word| word.is_object()) {
                    let mut merged = word.as_object().cloned().unwrap_or_default();
                    merged.entry("speaker_id".to_string()).or_insert_with(|| {
                        segment_map
                            .get("speaker_id")
                            .or_else(|| segment_map.get("speaker"))
                            .cloned()
                            .unwrap_or(Value::Null)
                    });
                    collected.push(Value::Object(merged));
                }
            }
        }
    }
    collected
}

pub fn word_text(word: &Value) -> String {
    first_value_string(word, &["text", "word"])
}

pub fn word_start(word: &Value) -> f64 {
    first_number(word, &["start", "start_time", "start_offset"]).unwrap_or(0.0)
}

pub fn word_end(word: &Value) -> f64 {
    first_number(word, &["end", "end_time", "end_offset"])
        .unwrap_or_else(|| word_start(word) + 0.05)
}

pub fn word_speaker(word: &Value) -> String {
    let value = first_value_string(word, &["speaker_id", "speaker"]);
    if value.is_empty() {
        "speaker_unknown".to_string()
    } else {
        value
    }
}

pub fn refined_text_from_payload(payload: &Value, alignment: &Value) -> String {
    let assignments: BTreeMap<String, String> = alignment
        .get("assignments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| {
            let provider = string_field(&item, "provider_speaker_id");
            if provider.is_empty() {
                return None;
            }
            let label = first_value_string(
                &item,
                &["speaker_label", "discord_user_id", "provider_speaker_id"],
            );
            Some((provider, label))
        })
        .collect();
    let words = provider_words(payload);
    if words.is_empty() {
        return string_field(payload, "text");
    }
    let mut lines = Vec::new();
    let mut current_speaker = String::new();
    let mut current_words = Vec::new();
    for word in words {
        let speaker_id = word_speaker(&word);
        let label = assignments.get(&speaker_id).cloned().unwrap_or(speaker_id);
        if label != current_speaker && !current_words.is_empty() {
            lines.push(format!(
                "{current_speaker}: {}",
                current_words.join(" ").trim()
            ));
            current_words.clear();
        }
        current_speaker = label;
        let token = word_text(&word);
        if !token.is_empty() {
            current_words.push(token);
        }
    }
    if !current_words.is_empty() {
        lines.push(format!(
            "{current_speaker}: {}",
            current_words.join(" ").trim()
        ));
    }
    lines.join("\n").trim().to_string()
}

pub fn align_speakers(provider_payload: &Value, sidecar: &Value) -> Value {
    let words = provider_words(provider_payload);
    let local_segments = sidecar
        .get("local_speaker_segments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut overlap: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    let mut labels = BTreeMap::new();
    for word in words {
        let provider = word_speaker(&word);
        let start = word_start(&word);
        let mut end = word_end(&word);
        if end <= start {
            end = start + 0.05;
        }
        for segment in &local_segments {
            let discord_user_id = string_field(segment, "speaker_user_id");
            if discord_user_id.is_empty() {
                continue;
            }
            labels.insert(
                discord_user_id.clone(),
                first_value_string(segment, &["speaker_label", "speaker_user_id"]),
            );
            let local_start = number_field(segment, "start_offset").unwrap_or(0.0);
            let local_end = number_field(segment, "end_offset").unwrap_or(local_start);
            let shared = (end.min(local_end) - start.max(local_start)).max(0.0);
            if shared > 0.0 {
                *overlap
                    .entry(provider.clone())
                    .or_default()
                    .entry(discord_user_id)
                    .or_default() += shared;
            }
        }
    }
    let mut assignments = Vec::new();
    let mut used_users = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    for (provider, scores) in &overlap {
        let mut ranked = scores.iter().collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
        let selected = ranked
            .into_iter()
            .find(|(user_id, _)| !used_users.contains(*user_id));
        let total: f64 = scores.values().sum();
        let Some((user_id, score)) = selected else {
            unresolved.insert(provider.clone());
            continue;
        };
        if total <= 0.0 {
            unresolved.insert(provider.clone());
            continue;
        }
        let confidence = *score / total;
        if confidence < 0.55 {
            unresolved.insert(provider.clone());
            continue;
        }
        used_users.insert(user_id.clone());
        assignments.push(serde_json::json!({
            "provider_speaker_id": provider,
            "discord_user_id": user_id,
            "speaker_label": labels.get(user_id).cloned().unwrap_or_else(|| user_id.clone()),
            "confidence": (confidence * 1000.0).round() / 1000.0
        }));
    }
    for provider in provider_words(provider_payload).iter().map(word_speaker) {
        if !overlap.contains_key(&provider) {
            unresolved.insert(provider);
        }
    }
    serde_json::json!({
        "alignment_id": "",
        "window_id": string_field(sidecar, "window_id"),
        "method": "temporal_overlap_greedy_assignment",
        "assignments": assignments,
        "unresolved_provider_speakers": unresolved.into_iter().collect::<Vec<_>>(),
        "notes": [],
        "created_at": isoformat_z(None)
    })
}

pub fn run_refinement_job(store: &TimelineStore, job_id: &str) -> Result<Value> {
    let mut job = store.get_job(job_id)?;
    if job.id.is_empty() {
        anyhow::bail!("unknown refinement job: {job_id}");
    }
    let guild_id = job.guild_id.clone();
    let channel_id = job.voice_channel_id.clone();
    let payload = job
        .refinement_payload()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("job {job_id} is not a refinement job"))?;
    let window_id = payload.window_id;
    let publication_id = payload.publication_id;
    let window = store.get_window(&window_id)?;
    if guild_id.is_empty() || channel_id.is_empty() || !window.is_object() {
        anyhow::bail!("refinement job {job_id} is missing guild/channel/window");
    }
    job.mark_running();
    job.attempts += 1;
    store.update_job(&job)?;
    match run_refinement_job_inner(
        store,
        job,
        &guild_id,
        &channel_id,
        &window_id,
        &publication_id,
        &window,
    ) {
        Ok(value) => Ok(value),
        Err(error) => {
            let mut failed = store.get_job(job_id)?;
            failed.set_state(JobState::FailedDraftRetained);
            failed.metadata.error = error.to_string();
            store.update_job(&failed)?;
            let mut publication = store.get_publication(&publication_id)?;
            if publication.is_object() {
                publication["state"] = Value::String("failed_draft_retained".to_string());
                publication["last_error"] = Value::String(error.to_string());
                store.update_publication(&publication)?;
            }
            Err(error)
        }
    }
}

fn run_refinement_job_inner(
    store: &TimelineStore,
    mut job: Job,
    guild_id: &str,
    channel_id: &str,
    window_id: &str,
    publication_id: &str,
    window: &Value,
) -> Result<Value> {
    let sidecar = store.export_mixed_audio(guild_id, channel_id, window_id, &job.id)?;
    let mixed_path = PathBuf::from(string_field(&sidecar, "mixed_audio_path"));
    let speaker_count = sidecar
        .get("local_speaker_segments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|seg| string_field(&seg, "speaker_user_id"))
        .filter(|id| !id.is_empty())
        .collect::<BTreeSet<_>>()
        .len();
    let provider_payload = submit_elevenlabs_stt(
        &mixed_path,
        &env::var("ELEVENLABS_STT_MODEL_ID").unwrap_or_else(|_| "scribe_v2".to_string()),
        true,
        "word",
        Some(speaker_count),
        Some(serde_json::json!({
            "job_id": job.id,
            "publication_id": publication_id,
            "window_id": window_id,
            "guild_id": guild_id,
            "voice_channel_id": channel_id
        })),
    )?;
    let artifact_dir = store.durable_publications_dir().join(publication_id);
    fs::create_dir_all(&artifact_dir)?;
    let provider_path = artifact_dir.join("elevenlabs.raw.json");
    write_json_file(&provider_path, &provider_payload)?;
    let mut alignment = align_speakers(&provider_payload, &sidecar);
    alignment["alignment_id"] =
        Value::String(format!("align_{}", job.id.trim_start_matches("job_")));
    let alignment_path = artifact_dir.join("speaker_alignment.json");
    write_json_file(&alignment_path, &alignment)?;
    let refined_path = artifact_dir.join("transcript.refined.txt");
    let refined_text = refined_text_from_payload(&provider_payload, &alignment);
    fs::write(
        &refined_path,
        if refined_text.trim().is_empty() {
            String::new()
        } else {
            format!("{}\n", refined_text.trim())
        },
    )?;
    let start = parse_instant(&string_field(window, "start_time"))
        .ok_or_else(|| anyhow::anyhow!("window {window_id} has invalid start time"))?;
    let end = parse_instant(&string_field(window, "end_time"))
        .ok_or_else(|| anyhow::anyhow!("window {window_id} has invalid end time"))?;
    let span = store.create_authoritative_span(
        guild_id,
        channel_id,
        window_id,
        publication_id,
        "elevenlabs",
        start,
        end,
        &refined_path,
        &alignment_path,
        string_array(window, "capture_run_ids"),
        string_array(window, "voice_bot_ids"),
    )?;
    let mut publication = store.get_publication(publication_id)?;
    if publication.is_object() {
        publication["state"] = Value::String("refined".to_string());
        publication["refined_artifact_path"] = Value::String(refined_path.display().to_string());
        publication["recording_artifact_path"] = Value::String(mixed_path.display().to_string());
        publication["speaker_alignment_path"] = Value::String(alignment_path.display().to_string());
        store.update_publication(&publication)?;
    }
    job.mark_complete();
    let result = serde_json::json!({
        "span": span,
        "refined_artifact_path": refined_path.display().to_string(),
        "speaker_alignment_path": alignment_path.display().to_string()
    });
    job.metadata.output = Some(JobOutput::from_boundary_json(&result)?);
    store.update_job(&job)?;
    Ok(job.to_value())
}

fn string_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        _ => String::new(),
    }
}

fn first_value_string(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| string_field(value, key))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn first_number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| number_field(value, key))
}

fn number_field(value: &Value, key: &str) -> Option<f64> {
    match value.get(key) {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.parse().ok(),
        _ => None,
    }
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| match value {
            Value::String(text) => Some(text),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .collect()
}
