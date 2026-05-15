use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Result;
use crate::dashboard::{ALPINE_JS, APP_JS, INDEX_HTML, STYLES_CSS};
use crate::runtime::automations::AutomationState;
use crate::runtime::{
    CommandRequest, ContextResolveRequest, DebugOverviewRequest, JobsRequest,
    ListConversationsRequest, MemberGetRequest, MemberResolveRequest, MemberSearchRequest,
    ParticipantTraceRequest, RenderTranscriptRequest, RuntimeHandle, SearchTranscriptsRequest,
    TimelineRangeRequest, TimelineTailRequest,
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
    fn runtime_context(&self) -> Result<crate::runtime::Runtime> {
        self.handle.runtime_context()
    }
}

macro_rules! runtime_context {
    ($state:expr) => {
        match $state.runtime_context() {
            Ok(runtime) => runtime,
            Err(error) => return err(error),
        }
    };
}

pub fn router(handle: RuntimeHandle) -> Router {
    let state = AppState { handle };
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/status", get(status))
        .route("/v1/voice/pool/status", get(pool_status))
        .route("/v1/voice/status", get(voice_status))
        .route("/v1/voice/rooms/occupants", get(room_occupants))
        .route("/v1/voice/commands", post(command_submit))
        .route("/v1/voice/responses", post(response_submit))
        .route(
            "/v1/voice/automations",
            get(automations_list).post(automation_create),
        )
        .route("/v1/voice/automations/validate", post(automation_validate))
        .route("/v1/voice/automations/dry-run", post(automation_validate))
        .route("/v1/voice/automations/{automation_id}", get(automation_get))
        .route(
            "/v1/voice/automations/{automation_id}/cancel",
            post(automation_cancel),
        )
        .route("/v1/voice/timeline/tail", get(timeline_tail))
        .route("/v1/voice/timeline/range", get(timeline_range))
        .route("/v1/voice/transcript/render", get(transcript_render))
        .route("/v1/voice/transcript/search", get(transcript_search))
        .route("/v1/voice/conversations/list", get(conversations_list))
        .route("/v1/voice/context/resolve", get(context_resolve))
        .route("/v1/voice/participant/trace", get(participant_trace))
        .route("/v1/voice/members/search", get(members_search))
        .route("/v1/voice/members/resolve", get(members_resolve))
        .route("/v1/voice/members/{user_id}", get(members_get))
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
        .route("/v1/voice/debug/agents/{job_id}", get(debug_agent_job))
        .route("/debug", get(debug_dashboard))
        .route("/debug/dashboard.css", get(debug_dashboard_css))
        .route("/debug/dashboard.js", get(debug_dashboard_js))
        .route("/debug/alpine.min.js", get(debug_alpine_js))
        .with_state(state)
}

pub async fn serve(handle: RuntimeHandle, addr: std::net::SocketAddr) -> Result<()> {
    let app = router(handle);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz(State(state): State<AppState>) -> Response {
    let runtime = match state.runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return err(error),
    };
    ok(json!({
        "ok": true,
        "botsConfigured": runtime.bots.len(),
        "sessionsActive": runtime.sessions.len(),
        "roomsConfigured": runtime.rooms.len(),
    }))
}

async fn status(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = match state.runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return err(error),
    };
    ok(runtime
        .status_payload(
            query
                .get("room")
                .or_else(|| query.get("channel"))
                .map(String::as_str),
        )
        .await)
}

async fn pool_status(State(state): State<AppState>) -> Response {
    let runtime = match state.runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return err(error),
    };
    ok(runtime.status_payload(None).await)
}

async fn voice_status(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = match state.runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return err(error),
    };
    let guild = query_str(&query, &["guild"]);
    let channel = query_str(&query, &["channel"]);
    if !guild.is_empty() && !channel.is_empty() {
        match runtime.resolve_room_scope(&guild, Some(&channel)) {
            Ok(room) => {
                let mut payload = runtime.status_for_room(&room).await;
                if let Value::Object(object) = &mut payload {
                    let occupants = match state
                        .handle
                        .room_occupants(&room.guild_id, &room.channel_id)
                        .await
                    {
                        Ok(occupants) => occupants,
                        Err(error) => return err(error),
                    };
                    object.insert("liveOccupants".to_string(), json!(occupants));
                }
                ok(payload)
            }
            Err(error) => err(error),
        }
    } else {
        let mut payload = runtime
            .status_payload(non_empty_string(channel).as_deref())
            .await;
        if let Value::Object(object) = &mut payload {
            let occupancy = match state.handle.voice_occupancy_snapshot().await {
                Ok(occupancy) => occupancy,
                Err(error) => return err(error),
            };
            object.insert("liveVoiceOccupancy".to_string(), occupancy);
        }
        ok(payload)
    }
}

async fn room_occupants(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    let guild = query_str(&query, &["guild", "guildId"]);
    let channel = query_str(&query, &["channel", "channelId", "room"]);
    if guild.is_empty() || channel.is_empty() {
        return err(crate::errors::discord_tool_error(
            "guild and room/channel are required",
        ));
    }
    match runtime.resolve_room_scope(&guild, Some(&channel)) {
        Ok(room) => match state
            .handle
            .room_occupants(&room.guild_id, &room.channel_id)
            .await
        {
            Ok(occupants) => ok(json!({
                "guildId": room.guild_id,
                "channelId": room.channel_id,
                "room": room.to_json(),
                "occupants": occupants,
            })),
            Err(error) => err(error),
        },
        Err(error) => err(error),
    }
}

async fn command_submit(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let command = match CommandRequest::from_json(&payload) {
        Ok(command) => command,
        Err(error) => return err(error),
    };
    result(state.handle.submit_command(command).await)
}

async fn response_submit(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let job = {
        let runtime = match state.runtime_context() {
            Ok(runtime) => runtime,
            Err(error) => return err(error),
        };
        match runtime.response_job_from_value(&payload).await {
            Ok(job) => job,
            Err(error) => return err(error),
        }
    };
    result(state.handle.submit_job(job).await)
}

async fn automation_validate(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Response {
    let runtime = match state.runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return err(error),
    };
    result(runtime.validate_automation_from_value(&payload))
}

async fn automation_create(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let mut runtime = runtime_context!(state);
    let response = runtime.create_automation_from_value(&payload).await;
    result(response)
}

async fn automations_list(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    let state_filter = match query
        .get("state")
        .map(|value| value.parse::<AutomationState>())
    {
        Some(Ok(state)) => Some(state),
        Some(Err(error)) => return err(error),
        None => None,
    };
    result(
        runtime
            .list_automation_records(
                non_empty_string(query_str(&query, &["guild", "guildId"])).as_deref(),
                non_empty_string(query_str(
                    &query,
                    &["channel", "channelId", "voice_channel_id"],
                ))
                .as_deref(),
                state_filter,
            )
            .await,
    )
}

async fn automation_get(
    State(state): State<AppState>,
    Path(automation_id): Path<String>,
) -> Response {
    let runtime = runtime_context!(state);
    result(runtime.get_automation_record(&automation_id).await)
}

async fn automation_cancel(
    State(state): State<AppState>,
    Path(automation_id): Path<String>,
) -> Response {
    let mut runtime = runtime_context!(state);
    let response = runtime.cancel_automation_record(&automation_id).await;
    result(response)
}

async fn timeline_tail(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .timeline_tail(TimelineTailRequest {
                guild_id: query_str(&query, &["guild", "guildId", "guild_id"]),
                channel_id: query_str(&query, &["channel", "channelId", "voice_channel_id"]),
                since: query_str(&query, &["since"]),
                limit: query_usize(&query, &["limit"], 200),
                include_ephemeral: query_bool(&query, &["ephemeral"], false),
                verbose: query_bool(&query, &["verbose"], false),
            })
            .await,
    )
}

async fn timeline_range(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .timeline_range(TimelineRangeRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                from: query_str(&query, &["from", "from_time"]),
                to: query_str(&query, &["to"]),
                all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
                limit: query_usize(&query, &["limit"], 500),
                include_ephemeral: query_bool(&query, &["ephemeral"], false),
                verbose: query_bool(&query, &["verbose"], false),
            })
            .await,
    )
}

async fn transcript_render(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .render_transcript(RenderTranscriptRequest {
                window_id: query_str(&query, &["window", "windowId"]),
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                since: query_str(&query, &["since"]),
                from: query_str(&query, &["from", "from_time"]),
                to: query_str(&query, &["to"]),
                prefer_refined: query_bool(&query, &["preferRefined", "prefer_refined"], true),
                format: query_str(&query, &["format"]),
                verbose: query_bool(&query, &["verbose"], false),
            })
            .await,
    )
}

async fn transcript_search(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .search_transcripts(SearchTranscriptsRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
                query: query_str(&query, &["query"]),
                since: query_str(&query, &["since"]),
                prefer_refined: query_bool(&query, &["preferRefined", "prefer_refined"], true),
                limit: query_usize(&query, &["limit"], 50),
            })
            .await,
    )
}

async fn conversations_list(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .list_conversations(ListConversationsRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
                since: query_str(&query, &["since"]),
            })
            .await,
    )
}

async fn context_resolve(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .context_resolve(ContextResolveRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                reference: query_str(&query, &["reference"]),
            })
            .await,
    )
}

async fn participant_trace(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .participant_trace(ParticipantTraceRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                user_id: query_str(&query, &["user", "userId", "user_id"]),
                from: query_str(&query, &["from", "from_time"]),
                to: query_str(&query, &["to"]),
                include_speech_snippets: query_bool(
                    &query,
                    &["includeSpeechSnippets", "include_speech_snippets"],
                    false,
                ),
            })
            .await,
    )
}

async fn members_search(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .members_search(MemberSearchRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                query: query_str(&query, &["query"]),
                limit: query_usize(&query, &["limit"], 10),
            })
            .await,
    )
}

async fn members_resolve(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .members_resolve(MemberResolveRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                query: query_str(&query, &["query"]),
            })
            .await,
    )
}

async fn members_get(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .members_get(MemberGetRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                user_id,
            })
            .await,
    )
}

async fn jobs_list(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .jobs(JobsRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                state: query_str(&query, &["state"]),
                include_ephemeral: query_bool(&query, &["ephemeral"], false),
                verbose: query_bool(&query, &["verbose"], false),
            })
            .await,
    )
}

async fn jobs_run_due(State(state): State<AppState>) -> Response {
    result(state.handle.drain_ready_jobs().await)
}

async fn jobs_get(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(runtime.get_job_payload(&job_id, query_bool(&query, &["verbose"], false)).await)
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
    let runtime = runtime_context!(state);
    result(
        runtime
            .debug_overview(DebugOverviewRequest {
                jobs_limit: query_usize(&query, &["jobsLimit"], 120),
                agent_limit: query_usize(&query, &["agentLimit"], 120),
                timeline_since: query_str(&query, &["timelineSince"]),
                timeline_limit: query_usize(&query, &["timelineLimit"], 120),
                transcript_since: query_str(&query, &["transcriptSince"]),
                transcript_limit: query_usize(&query, &["transcriptLimit"], 500),
                publication_limit: query_usize(&query, &["publicationLimit"], 120),
            })
            .await,
    )
}

async fn debug_agent_job(State(state): State<AppState>, Path(job_id): Path<String>) -> Response {
    let runtime = runtime_context!(state);
    result(runtime.debug_agent_job(&job_id).await)
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

async fn debug_alpine_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        ALPINE_JS,
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
    let causes = error
        .chain()
        .skip(1)
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>();
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": error.to_string(), "causes": causes})),
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
