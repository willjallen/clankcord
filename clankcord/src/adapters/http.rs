use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Result;
use crate::dashboard::{APP_JS, INDEX_HTML, STYLES_CSS};
use crate::runtime::{
    CommandRequest, ContextResolveRequest, DebugOverviewRequest, JobsRequest,
    ListConversationsRequest, ParticipantTraceRequest, RenderTranscriptRequest, Runtime,
    RuntimeHandle, SearchTranscriptsRequest, TimelineRangeRequest, TimelineTailRequest,
};

#[derive(Clone)]
pub struct AppState {
    pub handle: RuntimeHandle,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ConfirmationBody {
    #[serde(default)]
    approved_by_user_id: String,
    #[serde(default)]
    cancelled_by_user_id: String,
}

impl AppState {
    async fn runtime(&self) -> tokio::sync::OwnedMutexGuard<Runtime> {
        self.handle.runtime().lock_owned().await
    }
}

pub fn router(handle: RuntimeHandle) -> Router {
    let state = AppState { handle };
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/status", get(status))
        .route("/v1/voice/pool/status", get(pool_status))
        .route("/v1/voice/status", get(voice_status))
        .route("/v1/voice/commands", post(command_submit))
        .route("/v1/voice/timeline/tail", get(timeline_tail))
        .route("/v1/voice/timeline/range", get(timeline_range))
        .route("/v1/voice/transcript/render", get(transcript_render))
        .route("/v1/voice/transcript/search", get(transcript_search))
        .route("/v1/voice/conversations/list", get(conversations_list))
        .route("/v1/voice/context/resolve", get(context_resolve))
        .route("/v1/voice/participant/trace", get(participant_trace))
        .route("/v1/voice/jobs", get(jobs_list))
        .route("/v1/voice/jobs/run-due", post(jobs_run_due))
        .route("/v1/voice/jobs/{job_id}", get(jobs_get))
        .route("/v1/voice/jobs/{job_id}/retry", post(jobs_retry))
        .route(
            "/v1/voice/confirmations/{job_id}/approve",
            post(confirmation_approve),
        )
        .route(
            "/v1/voice/confirmations/{job_id}/cancel",
            post(confirmation_cancel),
        )
        .route("/v1/voice/debug/overview", get(debug_overview))
        .route("/debug", get(debug_dashboard))
        .route("/debug/dashboard.css", get(debug_dashboard_css))
        .route("/debug/dashboard.js", get(debug_dashboard_js))
        .with_state(state)
}

pub async fn serve(handle: RuntimeHandle, addr: std::net::SocketAddr) -> Result<()> {
    let app = router(handle);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz(State(state): State<AppState>) -> Response {
    let runtime = state.runtime().await;
    ok(json!({
        "ok": true,
        "botsConfigured": runtime.bots.len(),
        "sessionsActive": runtime.sessions.len(),
        "roomsConfigured": runtime.rooms.len(),
    }))
}

async fn status(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = state.runtime().await;
    ok(runtime.status_payload(
        query
            .get("room")
            .or_else(|| query.get("channel"))
            .map(String::as_str),
    ))
}

async fn pool_status(State(state): State<AppState>) -> Response {
    let runtime = state.runtime().await;
    ok(runtime.status_payload(None))
}

async fn voice_status(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = state.runtime().await;
    let guild = query_str(&query, &["guild"]);
    let channel = query_str(&query, &["channel"]);
    if !guild.is_empty() && !channel.is_empty() {
        match runtime.resolve_room_scope(&guild, Some(&channel)) {
            Ok(room) => ok(runtime.status_for_room(&room)),
            Err(error) => err(error),
        }
    } else {
        ok(runtime.status_payload(non_empty_string(channel).as_deref()))
    }
}

async fn command_submit(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let command = match CommandRequest::from_json(&payload) {
        Ok(command) => command,
        Err(error) => return err(error),
    };
    result(state.handle.submit_command(command).await)
}

async fn timeline_tail(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = state.runtime().await;
    result(runtime.timeline_tail(TimelineTailRequest {
        guild_id: query_str(&query, &["guild", "guildId", "guild_id"]),
        channel_id: query_str(&query, &["channel", "channelId", "voice_channel_id"]),
        since: query_str(&query, &["since"]),
    }))
}

async fn timeline_range(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = state.runtime().await;
    result(runtime.timeline_range(TimelineRangeRequest {
        guild_id: query_str(&query, &["guild", "guildId"]),
        channel_id: query_str(&query, &["channel", "channelId"]),
        from: query_str(&query, &["from", "from_time"]),
        to: query_str(&query, &["to"]),
        all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
    }))
}

async fn transcript_render(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = state.runtime().await;
    result(runtime.render_transcript(RenderTranscriptRequest {
        window_id: query_str(&query, &["window", "windowId"]),
        guild_id: query_str(&query, &["guild", "guildId"]),
        channel_id: query_str(&query, &["channel", "channelId"]),
        since: query_str(&query, &["since"]),
        from: query_str(&query, &["from", "from_time"]),
        to: query_str(&query, &["to"]),
        prefer_refined: query_bool(&query, &["preferRefined", "prefer_refined"], true),
        format: query_str(&query, &["format"]),
    }))
}

async fn transcript_search(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = state.runtime().await;
    result(runtime.search_transcripts(SearchTranscriptsRequest {
        guild_id: query_str(&query, &["guild", "guildId"]),
        channel_id: query_str(&query, &["channel", "channelId"]),
        all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
        query: query_str(&query, &["query"]),
        since: query_str(&query, &["since"]),
        prefer_refined: query_bool(&query, &["preferRefined", "prefer_refined"], true),
        limit: query_usize(&query, &["limit"], 50),
    }))
}

async fn conversations_list(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = state.runtime().await;
    result(runtime.list_conversations(ListConversationsRequest {
        guild_id: query_str(&query, &["guild", "guildId"]),
        channel_id: query_str(&query, &["channel", "channelId"]),
        all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
        since: query_str(&query, &["since"]),
    }))
}

async fn context_resolve(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = state.runtime().await;
    result(runtime.context_resolve(ContextResolveRequest {
        guild_id: query_str(&query, &["guild", "guildId"]),
        channel_id: query_str(&query, &["channel", "channelId"]),
        reference: query_str(&query, &["reference"]),
    }))
}

async fn participant_trace(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = state.runtime().await;
    result(runtime.participant_trace(ParticipantTraceRequest {
        guild_id: query_str(&query, &["guild", "guildId"]),
        user_id: query_str(&query, &["user", "userId", "user_id"]),
        from: query_str(&query, &["from", "from_time"]),
        to: query_str(&query, &["to"]),
        include_speech_snippets: query_bool(
            &query,
            &["includeSpeechSnippets", "include_speech_snippets"],
            false,
        ),
    }))
}

async fn jobs_list(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = state.runtime().await;
    result(runtime.jobs(JobsRequest {
        guild_id: query_str(&query, &["guild", "guildId"]),
        state: query_str(&query, &["state"]),
    }))
}

async fn jobs_run_due(State(state): State<AppState>) -> Response {
    result(state.handle.run_maintenance_once().await)
}

async fn jobs_get(State(state): State<AppState>, Path(job_id): Path<String>) -> Response {
    let runtime = state.runtime().await;
    result(runtime.get_job_payload(&job_id))
}

async fn jobs_retry(State(state): State<AppState>, Path(job_id): Path<String>) -> Response {
    result(state.handle.retry_job(job_id).await)
}

async fn confirmation_approve(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(payload): Json<ConfirmationBody>,
) -> Response {
    result(
        state
            .handle
            .approve_confirmation(job_id, payload.approved_by_user_id)
            .await,
    )
}

async fn confirmation_cancel(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(payload): Json<ConfirmationBody>,
) -> Response {
    result(
        state
            .handle
            .cancel_confirmation(job_id, payload.cancelled_by_user_id)
            .await,
    )
}

async fn debug_overview(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = state.runtime().await;
    result(runtime.debug_overview(DebugOverviewRequest {
        since: query_str(&query, &["since"]),
        limit: query_usize(&query, &["limit"], 80),
    }))
}

async fn debug_dashboard() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn debug_dashboard_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
}

async fn debug_dashboard_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
}

type BTreeQuery = std::collections::BTreeMap<String, String>;

fn ok(payload: Value) -> Response {
    Json(payload).into_response()
}

fn result(payload: Result<Value>) -> Response {
    match payload {
        Ok(payload) => ok(payload),
        Err(error) => err(error),
    }
}

fn err(error: anyhow::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": error.to_string()})),
    )
        .into_response()
}

fn query_str(query: &BTreeQuery, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| query.get(*key))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

fn query_bool(query: &BTreeQuery, keys: &[&str], fallback: bool) -> bool {
    let Some(value) = keys.iter().find_map(|key| query.get(*key)) else {
        return fallback;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => fallback,
    }
}

fn query_usize(query: &BTreeQuery, keys: &[&str], fallback: usize) -> usize {
    keys.iter()
        .find_map(|key| query.get(*key))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(fallback)
}

fn non_empty_string(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}
