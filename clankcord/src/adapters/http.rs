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
    CommandRequest, ContextResolveRequest, DashboardAgentsRequest, DashboardJobsRequest,
    DashboardOverviewRequest, DashboardTimelineRequest, DashboardTranscriptRequest, JobsRequest,
    ListConversationsRequest, MemberGetRequest, MemberResolveRequest, MemberSearchRequest,
    ParticipantTraceRequest, RenderTranscriptRequest, RuntimeHandle, RuntimeScope,
    RuntimeScopeKind, SearchTranscriptsRequest, TimelineRangeRequest, TimelineTailRequest,
    default_dashboard_categories, parse_dashboard_filter,
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
    fn runtime_context(&self) -> crate::runtime::Ctx {
        self.handle.runtime_context()
    }
}

macro_rules! runtime_context {
    ($state:expr) => {
        $state.runtime_context()
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
        .route("/v1/dashboard/timeline", get(dashboard_timeline))
        .route("/v1/dashboard/jobs", get(dashboard_jobs))
        .route("/v1/dashboard/summary", get(dashboard_summary))
        .route("/v1/dashboard/overview", get(dashboard_overview))
        .route("/v1/dashboard/agents", get(dashboard_agents))
        .route("/v1/dashboard/agents/{job_id}", get(dashboard_agent_detail))
        .route("/v1/dashboard/automations", get(dashboard_automations))
        .route("/v1/dashboard/health", get(dashboard_health))
        .route("/v1/dashboard/rooms", get(dashboard_rooms))
        .route("/v1/dashboard/transcript", get(dashboard_transcript))
        .route(
            "/v1/confirmations/{job_id}/approve",
            post(confirmation_approve),
        )
        .route(
            "/v1/confirmations/{job_id}/cancel",
            post(confirmation_cancel),
        )
        .merge(dashboard_asset_router())
        .layer(middleware::from_fn(track_http_request))
        .with_state(state)
}

pub fn dashboard_asset_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/dashboard", get(dashboard_index))
        .route("/dashboard/dashboard.css", get(dashboard_css))
        .route(
            "/dashboard/tabulator_midnight.min.css",
            get(dashboard_tabulator_css),
        )
        .route("/dashboard/echarts.min.js", get(dashboard_echarts_js))
        .route("/dashboard/tabulator.min.js", get(dashboard_tabulator_js))
        .route("/dashboard/dashboard-json.js", get(dashboard_json_js))
        .route("/dashboard/dashboard-charts.js", get(dashboard_charts_js))
        .route("/dashboard/dashboard-tables.js", get(dashboard_tables_js))
        .route(
            "/dashboard/dashboard-explorer.js",
            get(dashboard_explorer_js),
        )
        .route("/dashboard/dashboard.js", get(dashboard_js))
        .route("/dashboard/alpine.min.js", get(dashboard_alpine_js))
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
    let runtime = state.runtime_context();
    match crate::views::operations::operational_health_payload(&runtime).await {
        Ok(payload) => {
            let status = readiness_http_status(&payload);
            (status, Json(payload)).into_response()
        }
        Err(error) => health_error(error),
    }
}

async fn status(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = state.runtime_context();
    let guild = query_str(&query, &["guild"]);
    let channel = query_str(&query, &["channel"]);
    if !guild.is_empty() && !channel.is_empty() {
        match crate::runtime::rooms::catalog::resolve_room_scope(&runtime, &guild, Some(&channel))
            .await
        {
            Ok(room) => {
                let mut payload = match crate::views::status::status_for_room(&runtime, &room).await
                {
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
        let mut payload = match crate::views::status::status_payload(
            &runtime,
            non_empty_string(channel).as_deref(),
        )
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
    let runtime = state.runtime_context();
    match crate::views::status::status_payload(&runtime, None).await {
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
    match crate::runtime::rooms::catalog::resolve_room_scope(&runtime, &guild, Some(&channel)).await
    {
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
    let runtime = state.runtime_context();
    result(
        crate::runtime::domain::messaging::text_delivery::submit_agent_response_delivery(
            &runtime, &payload,
        )
        .await,
    )
}

async fn feedback_submit(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let runtime = runtime_context!(state);
    result(submit_feedback_event(&runtime, &payload).await)
}

async fn submit_feedback_event(runtime: &crate::runtime::Ctx, payload: &Value) -> Result<Value> {
    let message = first_value_string(payload, &["content", "message", "feedback_message"]);
    if message.trim().is_empty() {
        anyhow::bail!("feedback requires content");
    }
    let requested_by_user_id = first_value_string(payload, &["requested_by_user_id", "user_id"]);
    let source_job_id = first_value_string(payload, &["source_job_id", "job_id"]);
    let scope = if source_job_id.is_empty() {
        feedback_scope(payload)?
    } else {
        runtime.store.get_job(&source_job_id).await?.scope()
    };
    let event = runtime
        .store
        .append_scope_event(
            &scope,
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

fn feedback_scope(payload: &Value) -> Result<RuntimeScope> {
    let scope_kind = payload
        .get("scope_kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("feedback requires scope_kind"))?
        .parse::<RuntimeScopeKind>()?;
    let scope_id = payload
        .get("scope_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("feedback requires scope_id"))?;
    let guild_id = payload
        .get("guild_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match scope_kind {
        RuntimeScopeKind::VoiceChannel => {
            if guild_id.is_empty() {
                anyhow::bail!("voice-channel feedback requires guild_id");
            }
            Ok(RuntimeScope::voice_channel(guild_id, scope_id))
        }
        RuntimeScopeKind::Dm => {
            if !guild_id.is_empty() {
                anyhow::bail!("DM feedback requires an empty guild_id");
            }
            Ok(RuntimeScope::dm(scope_id))
        }
        RuntimeScopeKind::TextChannel => {
            if guild_id.is_empty() {
                anyhow::bail!("text-channel feedback requires guild_id");
            }
            Ok(RuntimeScope::text_channel(guild_id, scope_id))
        }
        RuntimeScopeKind::Thread => {
            if guild_id.is_empty() {
                anyhow::bail!("thread feedback requires guild_id");
            }
            Ok(RuntimeScope::thread(guild_id, scope_id))
        }
        RuntimeScopeKind::Ctx => {
            if !guild_id.is_empty() || scope_id != "runtime" {
                anyhow::bail!("runtime feedback requires empty guild_id and scope_id runtime");
            }
            Ok(RuntimeScope::runtime())
        }
    }
}

async fn automation_validate(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Response {
    let runtime = state.runtime_context();
    result(crate::runtime::automations::spec::validate_automation_from_value(&runtime, &payload))
}

async fn automation_create(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let runtime = runtime_context!(state);
    let response =
        crate::runtime::automations::spec::create_automation_from_value(&runtime, &payload).await;
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
        crate::runtime::automations::spec::list_automation_records(
            &runtime,
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
    result(crate::runtime::automations::spec::get_automation_record(&runtime, &automation_id).await)
}

async fn automation_cancel(
    State(state): State<AppState>,
    Path(automation_id): Path<String>,
) -> Response {
    let runtime = runtime_context!(state);
    let response =
        crate::runtime::automations::spec::cancel_automation_record(&runtime, &automation_id).await;
    result(response)
}

async fn timeline_tail(State(state): State<AppState>, Query(query): Query<BTreeQuery>) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::history::timeline_tail(
            &runtime,
            TimelineTailRequest {
                guild_id: query_str(&query, &["guild", "guildId", "guild_id"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                since: query_str(&query, &["since"]),
                limit: query_usize(&query, &["limit"], 200),
                include_ephemeral: query_bool(&query, &["ephemeral"], false),
                verbose: query_bool(&query, &["verbose"], false),
            },
        )
        .await,
    )
}

async fn timeline_range(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::history::timeline_range(
            &runtime,
            TimelineRangeRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                from: query_str(&query, &["from", "from_time"]),
                to: query_str(&query, &["to"]),
                all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
                limit: query_usize(&query, &["limit"], 500),
                include_ephemeral: query_bool(&query, &["ephemeral"], false),
                verbose: query_bool(&query, &["verbose"], false),
            },
        )
        .await,
    )
}

async fn transcript_render(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::history::render_transcript(
            &runtime,
            RenderTranscriptRequest {
                window_id: query_str(&query, &["window", "windowId"]),
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                since: query_str(&query, &["since"]),
                from: query_str(&query, &["from", "from_time"]),
                to: query_str(&query, &["to"]),
                format: query_str(&query, &["format"]),
                verbose: query_bool(&query, &["verbose"], false),
            },
        )
        .await,
    )
}

async fn transcript_search(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::history::search_transcripts(
            &runtime,
            SearchTranscriptsRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
                query: query_str(&query, &["query"]),
                since: query_str(&query, &["since"]),
                limit: query_usize(&query, &["limit"], 50),
            },
        )
        .await,
    )
}

async fn conversations_list(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::history::list_conversations(
            &runtime,
            ListConversationsRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                all_channels: query_bool(&query, &["allChannels", "all_channels"], false),
                since: query_str(&query, &["since"]),
            },
        )
        .await,
    )
}

async fn context_resolve(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::history::context_resolve(
            &runtime,
            ContextResolveRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                channel_id: query_str(&query, &["channel", "channelId"]),
                reference: query_str(&query, &["reference"]),
            },
        )
        .await,
    )
}

async fn participant_trace(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::history::participant_trace(
            &runtime,
            ParticipantTraceRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                user_id: query_str(&query, &["user", "userId", "user_id"]),
                from: query_str(&query, &["from", "from_time"]),
                to: query_str(&query, &["to"]),
                include_speech_snippets: query_bool(
                    &query,
                    &["includeSpeechSnippets", "include_speech_snippets"],
                    false,
                ),
            },
        )
        .await,
    )
}

async fn members_search(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::members::members_search(
            &runtime,
            MemberSearchRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                query: query_str(&query, &["query"]),
                limit: query_usize(&query, &["limit"], 10),
            },
        )
        .await,
    )
}

async fn members_resolve(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::members::members_resolve(
            &runtime,
            MemberResolveRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                query: query_str(&query, &["query"]),
            },
        )
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
        crate::views::members::members_get(
            &runtime,
            MemberGetRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                user_id,
            },
        )
        .await,
    )
}

async fn agent_sessions_current(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::runtime::domain::interactions::agent_sessions::agent_session_current(
            &runtime,
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
        crate::runtime::domain::interactions::agent_sessions::agent_session_list(
            &runtime,
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
        crate::runtime::domain::interactions::agent_sessions::agent_session_search(
            &runtime,
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
    result(
        crate::runtime::domain::interactions::agent_sessions::agent_session_get(
            &runtime,
            &agent_session_id,
        )
        .await,
    )
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
        crate::views::jobs::jobs(
            &runtime,
            JobsRequest {
                guild_id: query_str(&query, &["guild", "guildId"]),
                state: query_str(&query, &["state"]),
                include_ephemeral: query_bool(&query, &["ephemeral"], false),
                verbose: query_bool(&query, &["verbose"], false),
            },
        )
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
        crate::views::jobs::get_job_payload(
            &runtime,
            &job_id,
            query_bool(&query, &["verbose"], false),
        )
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

async fn dashboard_summary(State(state): State<AppState>) -> Response {
    let runtime = runtime_context!(state);
    result(crate::views::operations::dashboard_summary_payload(&runtime).await)
}

async fn dashboard_overview(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::dashboard::dashboard_overview(
            &runtime,
            DashboardOverviewRequest {
                jobs_limit: query_usize(&query, &["jobsLimit"], 120),
            },
        )
        .await,
    )
}

async fn dashboard_agents(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::dashboard::dashboard_agents(
            &runtime,
            DashboardAgentsRequest {
                limit: query_usize(&query, &["limit"], 120),
            },
        )
        .await,
    )
}

async fn dashboard_automations(State(state): State<AppState>) -> Response {
    let runtime = runtime_context!(state);
    result(crate::views::dashboard::dashboard_automations(&runtime).await)
}

async fn dashboard_health(State(state): State<AppState>) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::operations::dashboard_health_payload(
            &runtime,
            http_request_metrics_snapshot(),
            crate::app::ops::process_load_payload(),
        )
        .await,
    )
}

async fn dashboard_rooms(State(state): State<AppState>) -> Response {
    let runtime = runtime_context!(state);
    result(crate::views::operations::dashboard_rooms_payload(&runtime).await)
}

async fn dashboard_transcript(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    result(
        crate::views::dashboard::dashboard_transcript(
            &runtime,
            DashboardTranscriptRequest {
                since: query_str(&query, &["since"]),
                limit: query_usize(&query, &["limit"], 250),
                channel: query_str(&query, &["channel"]),
                search: query_str(&query, &["search"]),
            },
        )
        .await,
    )
}

async fn dashboard_timeline(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    let request = match dashboard_timeline_request(&query) {
        Ok(request) => request,
        Err(error) => return err(error),
    };
    result(crate::views::dashboard::dashboard_timeline(&runtime, request).await)
}

async fn dashboard_jobs(
    State(state): State<AppState>,
    Query(query): Query<BTreeQuery>,
) -> Response {
    let runtime = runtime_context!(state);
    let request = match dashboard_jobs_request(&query) {
        Ok(request) => request,
        Err(error) => return err(error),
    };
    result(crate::views::dashboard::dashboard_jobs(&runtime, request).await)
}

async fn dashboard_agent_detail(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Response {
    let runtime = runtime_context!(state);
    result(crate::views::dashboard::dashboard_agent_detail(&runtime, &job_id).await)
}

async fn dashboard_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn dashboard_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
}

async fn dashboard_tabulator_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        TABULATOR_CSS,
    )
}

async fn dashboard_echarts_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        ECHARTS_JS,
    )
}

async fn dashboard_tabulator_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        TABULATOR_JS,
    )
}

async fn dashboard_json_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        JSON_JS,
    )
}

async fn dashboard_charts_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        CHARTS_JS,
    )
}

async fn dashboard_tables_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        TABLES_JS,
    )
}

async fn dashboard_explorer_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        EXPLORER_JS,
    )
}

async fn dashboard_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
}

async fn dashboard_alpine_js() -> impl IntoResponse {
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

pub fn readiness_http_status(payload: &Value) -> StatusCode {
    match payload.get("status").and_then(Value::as_str) {
        Some("ok" | "degraded") => StatusCode::OK,
        Some("down" | "stale" | "unknown") | None | Some(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn health_error(error: anyhow::Error) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(health_error_payload(&error)),
    )
        .into_response()
}

pub fn health_error_payload(error: &anyhow::Error) -> Value {
    json!({
        "ok": false,
        "status": "down",
        "observedAt": Utc::now().to_rfc3339(),
        "components": [{
            "component": "runtime",
            "status": "down",
            "reason": "Health query failed",
            "details": {"error": error.to_string()},
        }],
    })
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

fn dashboard_timeline_request(query: &BTreeQuery) -> Result<DashboardTimelineRequest> {
    Ok(DashboardTimelineRequest {
        record_types: dashboard_filter(query, "recordTypes")?,
        categories: dashboard_category_filter(query)?,
        kinds: dashboard_filter(query, "kinds")?,
        event_kinds: dashboard_filter(query, "eventKinds")?,
        job_kinds: dashboard_filter(query, "jobKinds")?,
        states: dashboard_filter(query, "states")?,
        scope_kinds: dashboard_filter(query, "scopeKinds")?,
        scope_ids: dashboard_filter(query, "scopeIds")?,
        guild_ids: dashboard_filter(query, "guildIds")?,
        from: query_str(query, &["from"]),
        to: query_str(query, &["to"]),
        search: query_str(query, &["search"]),
        search_field: query_str(query, &["searchField"]),
        metadata: query_str(query, &["metadata"]),
        limit: query_usize(query, &["limit"], 120),
        cursor: query_str(query, &["cursor"]),
    })
}

fn dashboard_jobs_request(query: &BTreeQuery) -> Result<DashboardJobsRequest> {
    Ok(DashboardJobsRequest {
        categories: dashboard_category_filter(query)?,
        kinds: dashboard_filter(query, "kinds")?,
        job_kinds: dashboard_filter(query, "jobKinds")?,
        states: dashboard_filter(query, "states")?,
        scope_kinds: dashboard_filter(query, "scopeKinds")?,
        scope_ids: dashboard_filter(query, "scopeIds")?,
        guild_ids: dashboard_filter(query, "guildIds")?,
        from: query_str(query, &["from"]),
        to: query_str(query, &["to"]),
        search: query_str(query, &["search"]),
        search_field: query_str(query, &["searchField"]),
        metadata: query_str(query, &["metadata"]),
        limit: query_usize(query, &["limit"], 120),
        cursor: query_str(query, &["cursor"]),
    })
}

fn dashboard_filter(query: &BTreeQuery, key: &str) -> Result<crate::runtime::DashboardFilter> {
    parse_dashboard_filter(query.get(key).map(String::as_str), key)
}

fn dashboard_category_filter(query: &BTreeQuery) -> Result<crate::runtime::DashboardFilter> {
    let Some(raw) = query.get("categories") else {
        return Ok(default_dashboard_categories());
    };
    parse_dashboard_filter(Some(raw.as_str()), "categories")
}

fn non_empty_string(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}
