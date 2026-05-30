use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Result;
use crate::dashboard::{
    ALPINE_JS, APP_JS, CHARTS_JS, ECHARTS_JS, EXPLORER_JS, INDEX_HTML, JSON_JS, STYLES_CSS,
    TABLES_JS, TABULATOR_CSS, TABULATOR_JS,
};
use crate::runtime::automations::AutomationState;
use crate::runtime::util::first_value_string;
use crate::runtime::{
    CommandRequest, ContextResolveRequest, DebugOverviewRequest, JobsRequest,
    ListConversationsRequest, MemberGetRequest, MemberResolveRequest, MemberSearchRequest,
    ParticipantTraceRequest, RenderTranscriptRequest, RuntimeHandle, SearchTranscriptsRequest,
    TimelineRangeRequest, TimelineTailRequest,
};

static HTTP_REQUEST_METRICS: OnceLock<HttpRequestMetrics> = OnceLock::new();

#[derive(Debug)]
struct HttpRequestMetrics {
    started_at: String,
    total_started: AtomicU64,
    completed: AtomicU64,
    in_flight: AtomicU64,
    successful: AtomicU64,
    client_errors: AtomicU64,
    server_errors: AtomicU64,
    other_statuses: AtomicU64,
    total_latency_micros: AtomicU64,
    max_latency_micros: AtomicU64,
    routes: Mutex<BTreeMap<String, HttpRouteMetrics>>,
}

#[derive(Debug, Default, Clone)]
struct HttpRouteMetrics {
    total_started: u64,
    completed: u64,
    in_flight: u64,
    successful: u64,
    client_errors: u64,
    server_errors: u64,
    other_statuses: u64,
    total_latency_micros: u64,
    max_latency_micros: u64,
}

impl HttpRequestMetrics {
    fn new() -> Self {
        Self {
            started_at: Utc::now().to_rfc3339(),
            total_started: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            successful: AtomicU64::new(0),
            client_errors: AtomicU64::new(0),
            server_errors: AtomicU64::new(0),
            other_statuses: AtomicU64::new(0),
            total_latency_micros: AtomicU64::new(0),
            max_latency_micros: AtomicU64::new(0),
            routes: Mutex::new(BTreeMap::new()),
        }
    }

    fn start(&self, route: &str) {
        self.total_started.fetch_add(1, Ordering::Relaxed);
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        let mut routes = self.routes.lock().expect("http metrics mutex poisoned");
        let route = routes.entry(route.to_string()).or_default();
        route.total_started += 1;
        route.in_flight += 1;
    }

    fn finish(&self, route: &str, status: StatusCode, duration: Duration) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        let latency_micros = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        self.total_latency_micros
            .fetch_add(latency_micros, Ordering::Relaxed);
        fetch_max(&self.max_latency_micros, latency_micros);
        match status.as_u16() {
            200..=399 => {
                self.successful.fetch_add(1, Ordering::Relaxed);
            }
            400..=499 => {
                self.client_errors.fetch_add(1, Ordering::Relaxed);
            }
            500..=599 => {
                self.server_errors.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.other_statuses.fetch_add(1, Ordering::Relaxed);
            }
        };

        let mut routes = self.routes.lock().expect("http metrics mutex poisoned");
        let route = routes.entry(route.to_string()).or_default();
        route.completed += 1;
        route.in_flight -= 1;
        route.total_latency_micros += latency_micros;
        route.max_latency_micros = route.max_latency_micros.max(latency_micros);
        match status.as_u16() {
            200..=399 => route.successful += 1,
            400..=499 => route.client_errors += 1,
            500..=599 => route.server_errors += 1,
            _ => route.other_statuses += 1,
        }
    }

    fn snapshot(&self) -> Value {
        let total_started = self.total_started.load(Ordering::Relaxed);
        let completed = self.completed.load(Ordering::Relaxed);
        let total_latency_micros = self.total_latency_micros.load(Ordering::Relaxed);
        let routes = self
            .routes
            .lock()
            .expect("http metrics mutex poisoned")
            .iter()
            .map(|(route, metrics)| route_metrics_payload(route, metrics))
            .collect::<Vec<_>>();
        let mut routes = routes;
        routes.sort_by(|left, right| {
            json_u64(right, "totalStarted")
                .cmp(&json_u64(left, "totalStarted"))
                .then_with(|| route_name(left).cmp(&route_name(right)))
        });
        routes.truncate(24);

        json!({
            "startedAt": self.started_at,
            "totalStarted": total_started,
            "completed": completed,
            "inFlight": self.in_flight.load(Ordering::Relaxed),
            "successful": self.successful.load(Ordering::Relaxed),
            "clientErrors": self.client_errors.load(Ordering::Relaxed),
            "serverErrors": self.server_errors.load(Ordering::Relaxed),
            "otherStatuses": self.other_statuses.load(Ordering::Relaxed),
            "averageLatencyMicros": average_u64(total_latency_micros, completed),
            "maxLatencyMicros": self.max_latency_micros.load(Ordering::Relaxed),
            "routes": routes,
        })
    }
}

fn http_metrics() -> &'static HttpRequestMetrics {
    HTTP_REQUEST_METRICS.get_or_init(HttpRequestMetrics::new)
}

fn http_request_metrics_snapshot() -> Value {
    http_metrics().snapshot()
}

async fn track_http_request(request: Request, next: Next) -> Response {
    let route = request_route(&request);
    let started = Instant::now();
    http_metrics().start(&route);
    let response = next.run(request).await;
    http_metrics().finish(&route, response.status(), started.elapsed());
    response
}

fn request_route(request: &Request) -> String {
    let path = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or_else(|| request.uri().path());
    format!("{} {path}", request.method())
}

fn fetch_max(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

fn route_metrics_payload(route: &str, metrics: &HttpRouteMetrics) -> Value {
    json!({
        "route": route,
        "totalStarted": metrics.total_started,
        "completed": metrics.completed,
        "inFlight": metrics.in_flight,
        "successful": metrics.successful,
        "clientErrors": metrics.client_errors,
        "serverErrors": metrics.server_errors,
        "otherStatuses": metrics.other_statuses,
        "averageLatencyMicros": average_u64(metrics.total_latency_micros, metrics.completed),
        "maxLatencyMicros": metrics.max_latency_micros,
    })
}

fn average_u64(total: u64, count: u64) -> u64 {
    if count == 0 { 0 } else { total / count }
}

fn json_u64(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .expect("http route metrics contain numeric sort key")
}

fn route_name(value: &Value) -> String {
    value
        .get("route")
        .and_then(Value::as_str)
        .expect("http route metrics contain route")
        .to_string()
}

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

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AgentSessionSunsetBody {
    #[serde(default)]
    requested_by_user_id: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AgentSessionResumeBody {
    #[serde(default)]
    route_kind: String,
    #[serde(default)]
    guild_id: String,
    #[serde(default)]
    scope_id: String,
    #[serde(default)]
    requested_by_user_id: String,
    #[serde(default)]
    message: String,
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
        .route("/v1/pool/status", get(pool_status))
        .route("/v1/rooms/occupants", get(room_occupants))
        .route("/v1/commands", post(command_submit))
        .route("/v1/responses", post(response_submit))
        .route("/v1/feedback", post(feedback_submit))
        .route(
            "/v1/automations",
            get(automations_list).post(automation_create),
        )
        .route("/v1/automations/validate", post(automation_validate))
        .route("/v1/automations/dry-run", post(automation_validate))
        .route("/v1/automations/{automation_id}", get(automation_get))
        .route(
            "/v1/automations/{automation_id}/cancel",
            post(automation_cancel),
        )
        .route("/v1/timeline/tail", get(timeline_tail))
        .route("/v1/timeline/range", get(timeline_range))
        .route("/v1/transcript/render", get(transcript_render))
        .route("/v1/transcript/search", get(transcript_search))
        .route("/v1/conversations/list", get(conversations_list))
        .route("/v1/context/resolve", get(context_resolve))
        .route("/v1/participant/trace", get(participant_trace))
        .route("/v1/members/search", get(members_search))
        .route("/v1/members/resolve", get(members_resolve))
        .route("/v1/members/{user_id}", get(members_get))
        .route("/v1/agent-sessions/current", get(agent_sessions_current))
        .route("/v1/agent-sessions", get(agent_sessions_list))
        .route("/v1/agent-sessions/search", get(agent_sessions_search))
        .route(
            "/v1/agent-sessions/{agent_session_id}",
            get(agent_sessions_get),
        )
        .route(
            "/v1/agent-sessions/{agent_session_id}/sunset",
            post(agent_sessions_sunset),
        )
        .route(
            "/v1/agent-sessions/{agent_session_id}/resume",
            post(agent_sessions_resume),
        )
        .route("/v1/jobs", get(jobs_list))
        .route("/v1/jobs/run-due", post(jobs_run_due))
        .route("/v1/jobs/{job_id}", get(jobs_get))
        .route("/v1/jobs/{job_id}/retry", post(jobs_retry))
        .route(
            "/v1/confirmations/{job_id}/approve",
            post(confirmation_approve),
        )
        .route(
            "/v1/confirmations/{job_id}/cancel",
            post(confirmation_cancel),
        )
        .route("/v1/debug/overview", get(debug_overview))
        .route("/v1/debug/agents/{job_id}", get(debug_agent_job))
        .route("/debug", get(debug_dashboard))
        .route("/debug/dashboard.css", get(debug_dashboard_css))
        .route(
            "/debug/tabulator_midnight.min.css",
            get(debug_tabulator_css),
        )
        .route("/debug/echarts.min.js", get(debug_echarts_js))
        .route("/debug/tabulator.min.js", get(debug_tabulator_js))
        .route("/debug/dashboard-json.js", get(debug_dashboard_json_js))
        .route("/debug/dashboard-charts.js", get(debug_dashboard_charts_js))
        .route("/debug/dashboard-tables.js", get(debug_dashboard_tables_js))
        .route(
            "/debug/dashboard-explorer.js",
            get(debug_dashboard_explorer_js),
        )
        .route("/debug/dashboard.js", get(debug_dashboard_js))
        .route("/debug/alpine.min.js", get(debug_alpine_js))
        .layer(middleware::from_fn(track_http_request))
        .with_state(state)
}

pub async fn serve_until_shutdown(
    handle: RuntimeHandle,
    addr: std::net::SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let app = router(handle);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

async fn healthz(State(state): State<AppState>) -> Response {
    let runtime = match state.runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return err(error),
    };
    let bots = match runtime.timeline_store.list_voice_bot_states().await {
        Ok(bots) => bots,
        Err(error) => return err(error),
    };
    let sessions = match runtime.timeline_store.list_active_capture_sessions().await {
        Ok(sessions) => sessions,
        Err(error) => return err(error),
    };
    let rooms = match runtime.timeline_store.list_room_configs().await {
        Ok(rooms) => rooms,
        Err(error) => return err(error),
    };
    ok(json!({
        "ok": true,
        "botsObserved": bots.len(),
        "activeSessions": sessions.len(),
        "roomsConfigured": rooms.len(),
    }))
}

async fn status(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = match state.runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return err(error),
    };
    let guild = query_str(&query, &["guild"]);
    let channel = query_str(&query, &["channel"]);
    if !guild.is_empty() && !channel.is_empty() {
        match runtime.resolve_room_scope(&guild, Some(&channel)).await {
            Ok(room) => {
                let mut payload = match runtime.status_for_room(&room).await {
                    Ok(payload) => payload,
                    Err(error) => return err(error),
                };
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
        let mut payload = match runtime
            .status_payload(non_empty_string(channel).as_deref())
            .await
        {
            Ok(payload) => payload,
            Err(error) => return err(error),
        };
        if let Value::Object(object) = &mut payload {
            let occupancy = match state.handle.voice_occupancy_snapshot().await {
                Ok(occupancy) => occupancy,
                Err(error) => return err(error),
            };
            object.insert("liveOccupancy".to_string(), occupancy);
        }
        ok(payload)
    }
}

async fn pool_status(State(state): State<AppState>) -> Response {
    let runtime = match state.runtime_context() {
        Ok(runtime) => runtime,
        Err(error) => return err(error),
    };
    match runtime.status_payload(None).await {
        Ok(payload) => ok(payload),
        Err(error) => err(error),
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
    match runtime.resolve_room_scope(&guild, Some(&channel)).await {
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
        match runtime.text_delivery_job_from_value(&payload).await {
            Ok(job) => job,
            Err(error) => return err(error),
        }
    };
    result(state.handle.submit_job(job).await)
}

async fn feedback_submit(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let runtime = runtime_context!(state);
    result(submit_feedback_event(&runtime, &payload).await)
}

async fn submit_feedback_event(
    runtime: &crate::runtime::Runtime,
    payload: &Value,
) -> Result<Value> {
    let guild_id = first_value_string(payload, &["guild_id", "guildId"]);
    if guild_id.is_empty() {
        anyhow::bail!("feedback requires guild_id");
    }
    let channel_id = first_value_string(payload, &["scope_id", "channel_id", "channelId"]);
    if channel_id.is_empty() {
        anyhow::bail!("feedback requires scope_id");
    }
    let message = first_value_string(payload, &["content", "message", "feedback_message"]);
    if message.trim().is_empty() {
        anyhow::bail!("feedback requires content");
    }
    let requested_by_user_id = first_value_string(payload, &["requested_by_user_id", "user_id"]);
    let source_job_id = first_value_string(payload, &["source_job_id", "job_id"]);
    let event = runtime
        .timeline_store
        .append_event(
            &guild_id,
            &channel_id,
            json!({
                "event_kind": "feedback",
                "kind": "feedback",
                "source": "agent_cli",
                "job_id": source_job_id,
                "speaker_user_id": requested_by_user_id,
                "text": &message,
                "feedback_message": &message,
            }),
        )
        .await?;
    Ok(json!({
        "recorded": true,
        "feedback": event,
    }))
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
                non_empty_string(query_str(&query, &["channel", "channelId"])).as_deref(),
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
                channel_id: query_str(&query, &["channel", "channelId"]),
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

async fn members_search(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
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

async fn agent_sessions_current(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .agent_session_current(
                &query_str(&query, &["guild", "guildId"]),
                &query_str(&query, &["channel", "channelId"]),
            )
            .await,
    )
}

async fn agent_sessions_list(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .agent_session_list(
                &query_str(&query, &["guild", "guildId"]),
                &query_str(&query, &["channel", "channelId"]),
                &query_str(&query, &["state"]),
                query_usize(&query, &["limit"], 50),
            )
            .await,
    )
}

async fn agent_sessions_search(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        runtime
            .agent_session_search(
                &query_str(&query, &["guild", "guildId"]),
                &query_str(&query, &["channel", "channelId"]),
                &query_str(&query, &["state"]),
                &query_str(&query, &["query"]),
                &query_str(&query, &["since"]),
                query_usize(&query, &["limit"], 25),
            )
            .await,
    )
}

async fn agent_sessions_get(
    State(state): State<AppState>,
    Path(agent_session_id): Path<String>,
) -> Response {
    let runtime = runtime_context!(state);
    result(runtime.agent_session_get(&agent_session_id).await)
}

async fn agent_sessions_sunset(
    State(state): State<AppState>,
    Path(agent_session_id): Path<String>,
    Json(payload): Json<AgentSessionSunsetBody>,
) -> Response {
    if payload.reason.trim().is_empty() {
        return err(crate::errors::discord_tool_error(
            "agent session sunset requires reason",
        ));
    }
    result(
        state
            .handle
            .submit_job(crate::runtime::Job::agent_session_sunset(
                agent_session_id,
                payload.requested_by_user_id,
                payload.reason,
            ))
            .await,
    )
}

async fn agent_sessions_resume(
    State(state): State<AppState>,
    Path(agent_session_id): Path<String>,
    Json(payload): Json<AgentSessionResumeBody>,
) -> Response {
    result(
        state
            .handle
            .submit_job(crate::runtime::Job::agent_session_resume(
                agent_session_id,
                payload.route_kind,
                payload.guild_id,
                payload.scope_id,
                payload.requested_by_user_id,
                payload.message,
            ))
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
    result(
        runtime
            .get_job_payload(&job_id, query_bool(&query, &["verbose"], false))
            .await,
    )
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
                timeline_window: query_str(&query, &["timelineWindow"]),
                timeline_start: query_str(&query, &["timelineStart"]),
                timeline_end: query_str(&query, &["timelineEnd"]),
                timeline_limit: query_usize(&query, &["timelineLimit"], 120),
                timeline_query: query_str(&query, &["timelineSearch"]),
                timeline_query_field: query_str(&query, &["timelineSearchField"]),
                transcript_since: query_str(&query, &["transcriptSince"]),
                transcript_limit: query_usize(&query, &["transcriptLimit"], 250),
                transcript_channel: query_str(&query, &["transcriptChannel"]),
                transcript_query: query_str(&query, &["transcriptSearch"]),
                publication_limit: query_usize(&query, &["publicationLimit"], 120),
                http_requests: http_request_metrics_snapshot(),
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

async fn debug_tabulator_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        TABULATOR_CSS,
    )
}

async fn debug_echarts_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        ECHARTS_JS,
    )
}

async fn debug_tabulator_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        TABULATOR_JS,
    )
}

async fn debug_dashboard_json_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        JSON_JS,
    )
}

async fn debug_dashboard_charts_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        CHARTS_JS,
    )
}

async fn debug_dashboard_tables_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        TABLES_JS,
    )
}

async fn debug_dashboard_explorer_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        EXPLORER_JS,
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
