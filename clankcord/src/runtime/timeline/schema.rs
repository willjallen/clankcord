use super::store::*;

impl TimelineStore {
    pub async fn initialize(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        self.ensure_schema_migration_table().await?;
        self.create_tables().await?;
        self.run_pending_schema_migrations().await?;
        self.create_indexes().await?;
        self.assert_schema_invariants().await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct TableSchema {
    name: &'static str,
    columns: &'static [ColumnSchema],
}

#[derive(Debug, Clone, Copy)]
struct ColumnSchema {
    name: &'static str,
    data_type: &'static str,
    nullable: bool,
}

#[derive(Debug, Clone)]
struct ActualColumnSchema {
    data_type: String,
    nullable: bool,
}

const fn table(name: &'static str, columns: &'static [ColumnSchema]) -> TableSchema {
    TableSchema { name, columns }
}

const fn column(name: &'static str, data_type: &'static str, nullable: bool) -> ColumnSchema {
    ColumnSchema {
        name,
        data_type,
        nullable,
    }
}

const EXPECTED_TABLE_SCHEMAS: &[TableSchema] = &[
    table(
        "voice_rooms",
        &[
            column("guild_id", "text", false),
            column("voice_channel_id", "text", false),
            column("guild_slug", "text", false),
            column("voice_channel_name", "text", false),
            column("voice_channel_slug", "text", false),
            column("updated_at_ms", "bigint", false),
        ],
    ),
    table(
        "room_controls",
        &[
            column("guild_id", "text", false),
            column("voice_channel_id", "text", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "runtime_status",
        &[
            column("status_key", "text", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "bot_states",
        &[
            column("bot_id", "text", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "capture_sessions",
        &[
            column("session_id", "text", false),
            column("assignment_id", "text", false),
            column("guild_id", "text", false),
            column("voice_channel_id", "text", false),
            column("bot_id", "text", false),
            column("capture_run_id", "text", false),
            column("active", "boolean", false),
            column("started_at_ms", "bigint", true),
            column("ended_at_ms", "bigint", true),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "assignments",
        &[
            column("assignment_id", "text", false),
            column("guild_id", "text", false),
            column("voice_channel_id", "text", false),
            column("voice_bot_id", "text", false),
            column("capture_run_id", "text", false),
            column("state", "text", false),
            column("assigned_at_ms", "bigint", true),
            column("released_at_ms", "bigint", true),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "occupancy",
        &[
            column("guild_id", "text", false),
            column("voice_channel_id", "text", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "voice_states",
        &[
            column("guild_id", "text", false),
            column("user_id", "text", false),
            column("voice_channel_id", "text", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "discord_member_cache_refreshes",
        &[
            column("guild_id", "text", false),
            column("refreshed_at_ms", "bigint", false),
        ],
    ),
    table(
        "discord_members",
        &[
            column("guild_id", "text", false),
            column("user_id", "text", false),
            column("username", "text", false),
            column("global_name", "text", false),
            column("nick", "text", false),
            column("display_name", "text", false),
            column("normalized_search", "text", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "capture_runs",
        &[
            column("capture_run_id", "text", false),
            column("guild_id", "text", false),
            column("voice_channel_id", "text", false),
            column("voice_bot_id", "text", false),
            column("started_at_ms", "bigint", true),
            column("ended_at_ms", "bigint", true),
            column("state", "text", false),
            column("mode", "text", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "timeline_events",
        &[
            column("sequence", "bigint", false),
            column("event_id", "text", false),
            column("scope_kind", "text", false),
            column("guild_id", "text", false),
            column("scope_id", "text", false),
            column("event_kind", "text", false),
            column("started_at_ms", "bigint", false),
            column("ended_at_ms", "bigint", false),
            column("created_at_ms", "bigint", false),
            column("capture_run_id", "text", false),
            column("conversation_id", "text", false),
            column("speaker_user_id", "text", false),
            column("speaker_label", "text", false),
            column("text", "text", false),
            column("forgotten", "boolean", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "conversations",
        &[
            column("conversation_id", "text", false),
            column("scope_kind", "text", false),
            column("guild_id", "text", false),
            column("scope_id", "text", false),
            column("start_ms", "bigint", true),
            column("end_ms", "bigint", true),
            column("last_speech_at_ms", "bigint", true),
            column("state", "text", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "transcription_slots",
        &[
            column("slot_id", "text", false),
            column("source_job_id", "text", false),
            column("mux_job_id", "text", false),
            column("state", "text", false),
            column("guild_id", "text", false),
            column("voice_channel_id", "text", false),
            column("capture_run_id", "text", false),
            column("voice_bot_id", "text", false),
            column("voice_bot_discord_user_id", "text", false),
            column("speaker_user_id", "text", false),
            column("speaker_label", "text", false),
            column("speaker_username", "text", false),
            column("segment_index", "bigint", false),
            column("segment_start_ms", "bigint", false),
            column("segment_end_ms", "bigint", false),
            column("duration_ms", "bigint", false),
            column("source_audio_path", "text", false),
            column("audio_checksum", "text", false),
            column("audio_bytes", "bigint", false),
            column("audio_format", "text", false),
            column("sample_rate_hz", "bigint", false),
            column("channels", "bigint", false),
            column("sample_width_bits", "bigint", false),
            column("post_processing", "text", false),
            column("transcription_source_id", "text", false),
            column("provider", "text", false),
            column("model", "text", false),
            column("priority", "bigint", false),
            column("mux_stream_id", "text", false),
            column("mux_start_ms", "bigint", true),
            column("mux_end_ms", "bigint", true),
            column("guard_before_ms", "bigint", false),
            column("guard_after_ms", "bigint", false),
            column("created_at_ms", "bigint", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "windows",
        &[
            column("window_id", "text", false),
            column("scope_kind", "text", false),
            column("guild_id", "text", false),
            column("scope_id", "text", false),
            column("start_ms", "bigint", true),
            column("end_ms", "bigint", true),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "publications",
        &[
            column("publication_id", "text", false),
            column("scope_kind", "text", false),
            column("guild_id", "text", false),
            column("scope_id", "text", false),
            column("window_id", "text", false),
            column("state", "text", false),
            column("created_at_ms", "bigint", true),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "runtime_metadata",
        &[
            column("key", "text", false),
            column("value", "text", false),
            column("updated_at_ms", "bigint", false),
        ],
    ),
    table(
        "runtime_config",
        &[
            column("config_key", "text", false),
            column("updated_at_ms", "bigint", false),
            column("payload_json", "jsonb", false),
        ],
    ),
    table(
        "clankcord_schema_migrations",
        &[
            column("version", "text", false),
            column("name", "text", false),
            column("applied_at_ms", "bigint", false),
            column("clankcord_version", "text", false),
        ],
    ),
    table(
        "jobs",
        &[
            column("job_id", "text", false),
            column("scope_kind", "text", false),
            column("guild_id", "text", false),
            column("scope_id", "text", false),
            column("kind", "text", false),
            column("state", "text", false),
            column("terminal", "boolean", false),
            column("failed", "boolean", false),
            column("ephemeral", "boolean", false),
            column("cancellable", "boolean", false),
            column("lane", "text", false),
            column("ordering_key", "text", false),
            column("ready_at_ms", "bigint", false),
            column("created_at_ms", "bigint", false),
            column("updated_at_ms", "bigint", false),
            column("started_at_ms", "bigint", true),
            column("completed_at_ms", "bigint", true),
            column("gc_after_ms", "bigint", true),
            column("root_job_id", "text", false),
            column("parent_job_id", "text", true),
            column("lineage_depth", "bigint", false),
            column("requested_by_user_id", "text", false),
            column("command_kind", "text", false),
            column("source_job_id", "text", false),
            column("stream_id", "text", false),
            column("target_job_id", "text", false),
            column("speaker_user_id", "text", false),
            column("segment_end_ms", "bigint", true),
        ],
    ),
    table(
        "job_payloads",
        &[
            column("job_id", "text", false),
            column("payload_blob", "bytea", false),
        ],
    ),
    table(
        "job_dependencies",
        &[
            column("parent_job_id", "text", false),
            column("child_job_id", "text", false),
            column("dependency_kind", "text", false),
            column("created_at_ms", "bigint", false),
            column("resolution_policy", "text", false),
        ],
    ),
    table(
        "automations",
        &[
            column("automation_id", "text", false),
            column("scope_kind", "text", false),
            column("guild_id", "text", false),
            column("scope_id", "text", false),
            column("state", "text", false),
            column("idempotency_key", "text", false),
            column("created_at_ms", "bigint", true),
            column("updated_at_ms", "bigint", false),
            column("expires_at_ms", "bigint", true),
            column("fire_count", "bigint", false),
            column("max_fires", "bigint", true),
            column("payload_blob", "bytea", false),
        ],
    ),
    table(
        "agent_sessions",
        &[
            column("agent_session_id", "text", false),
            column("codex_session_id", "text", false),
            column("route_kind", "text", false),
            column("route_key", "text", false),
            column("guild_id", "text", false),
            column("scope_id", "text", false),
            column("dm_user_id", "text", false),
            column("voice_capture_session_id", "text", false),
            column("discord_thread_id", "text", false),
            column("discord_parent_channel_id", "text", false),
            column("text_target_kind", "text", false),
            column("text_channel_id", "text", false),
            column("text_user_id", "text", false),
            column("state", "text", false),
            column("created_at_ms", "bigint", false),
            column("last_activity_at_ms", "bigint", false),
            column("max_active_until_ms", "bigint", false),
            column("retired_at_ms", "bigint", true),
            column("retirement_reason", "text", false),
            column("retired_by_user_id", "text", false),
            column("resumed_from_agent_session_id", "text", false),
            column("payload_blob", "bytea", false),
        ],
    ),
];

const EXPECTED_INDEXES: &[(&str, &[&str])] = &[
    (
        "agent_sessions",
        &[
            "agent_sessions_pkey",
            "idx_agent_sessions_codex",
            "idx_agent_sessions_route",
            "idx_agent_sessions_thread",
        ],
    ),
    ("assignments", &["assignments_pkey"]),
    (
        "automations",
        &[
            "automations_pkey",
            "idx_automations_idempotency",
            "idx_automations_scope_state",
        ],
    ),
    ("bot_states", &["bot_states_pkey"]),
    (
        "capture_runs",
        &["capture_runs_pkey", "idx_capture_runs_room_time"],
    ),
    (
        "conversations",
        &["conversations_pkey", "idx_conversations_room_time"],
    ),
    (
        "discord_member_cache_refreshes",
        &["discord_member_cache_refreshes_pkey"],
    ),
    (
        "discord_members",
        &[
            "discord_members_pkey",
            "idx_discord_members_guild_normalized",
        ],
    ),
    (
        "job_dependencies",
        &["idx_job_dependencies_child", "job_dependencies_pkey"],
    ),
    ("job_payloads", &["job_payloads_pkey"]),
    (
        "jobs",
        &[
            "idx_jobs_active_ordering",
            "idx_jobs_active_visible_scope",
            "idx_jobs_agent_task_requester_recent",
            "idx_jobs_agent_task_scope_recent",
            "idx_jobs_audio_segment_pending_speaker",
            "idx_jobs_cancellable_scope_recent",
            "idx_jobs_due_kind",
            "idx_jobs_ephemeral_gc",
            "idx_jobs_failed_visible",
            "idx_jobs_kind_updated",
            "idx_jobs_queued_ready",
            "idx_jobs_recent_visible",
            "idx_jobs_scope_kind_updated",
            "idx_jobs_scope_state_kind_updated",
            "idx_jobs_state_updated",
            "idx_jobs_text_delivery_source",
            "idx_jobs_wake_stream_queued",
            "jobs_pkey",
        ],
    ),
    (
        "clankcord_schema_migrations",
        &["clankcord_schema_migrations_pkey"],
    ),
    ("occupancy", &["occupancy_pkey"]),
    (
        "publications",
        &["idx_publications_room_state", "publications_pkey"],
    ),
    (
        "room_controls",
        &["idx_room_controls_updated", "room_controls_pkey"],
    ),
    ("runtime_metadata", &["runtime_metadata_pkey"]),
    ("runtime_config", &["runtime_config_pkey"]),
    ("runtime_status", &["runtime_status_pkey"]),
    (
        "capture_sessions",
        &["capture_sessions_pkey", "idx_capture_sessions_active_room"],
    ),
    (
        "timeline_events",
        &[
            "idx_timeline_capture_run_time",
            "idx_timeline_conversation_time",
            "idx_timeline_kind_time",
            "idx_timeline_room_kind_time",
            "idx_timeline_room_time",
            "idx_timeline_speaker_time",
            "timeline_events_event_id_key",
            "timeline_events_pkey",
        ],
    ),
    (
        "transcription_slots",
        &[
            "idx_transcription_slots_mux_job",
            "idx_transcription_slots_scope_speaker_time",
            "idx_transcription_slots_source_mux_state",
            "idx_transcription_slots_source_state_created",
            "idx_transcription_slots_state_priority",
            "transcription_slots_pkey",
            "transcription_slots_source_job_id_key",
        ],
    ),
    ("voice_rooms", &["voice_rooms_pkey"]),
    (
        "voice_states",
        &["idx_voice_states_room_updated", "voice_states_pkey"],
    ),
    ("windows", &["windows_pkey"]),
];

impl TimelineStore {
    async fn create_tables(&self) -> Result<()> {
        sqlx::raw_sql(
            r#"
            CREATE TABLE IF NOT EXISTS voice_rooms (
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              guild_slug TEXT NOT NULL DEFAULT '',
              voice_channel_name TEXT NOT NULL DEFAULT '',
              voice_channel_slug TEXT NOT NULL DEFAULT '',
              updated_at_ms BIGINT NOT NULL,
              PRIMARY KEY (guild_id, voice_channel_id)
            );

            CREATE TABLE IF NOT EXISTS room_controls (
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL,
              PRIMARY KEY (guild_id, voice_channel_id)
            );

            CREATE TABLE IF NOT EXISTS runtime_status (
              status_key TEXT PRIMARY KEY,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS bot_states (
              bot_id TEXT PRIMARY KEY,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS capture_sessions (
              session_id TEXT PRIMARY KEY,
              assignment_id TEXT NOT NULL DEFAULT '',
              guild_id TEXT NOT NULL DEFAULT '',
              voice_channel_id TEXT NOT NULL DEFAULT '',
              bot_id TEXT NOT NULL DEFAULT '',
              capture_run_id TEXT NOT NULL DEFAULT '',
              active BOOLEAN NOT NULL DEFAULT FALSE,
              started_at_ms BIGINT,
              ended_at_ms BIGINT,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS assignments (
              assignment_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL DEFAULT '',
              voice_channel_id TEXT NOT NULL DEFAULT '',
              voice_bot_id TEXT NOT NULL DEFAULT '',
              capture_run_id TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              assigned_at_ms BIGINT,
              released_at_ms BIGINT,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS occupancy (
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL,
              PRIMARY KEY (guild_id, voice_channel_id)
            );

            CREATE TABLE IF NOT EXISTS voice_states (
              guild_id TEXT NOT NULL,
              user_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL,
              PRIMARY KEY (guild_id, user_id)
            );

            CREATE TABLE IF NOT EXISTS discord_member_cache_refreshes (
              guild_id TEXT PRIMARY KEY,
              refreshed_at_ms BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS discord_members (
              guild_id TEXT NOT NULL,
              user_id TEXT NOT NULL,
              username TEXT NOT NULL DEFAULT '',
              global_name TEXT NOT NULL DEFAULT '',
              nick TEXT NOT NULL DEFAULT '',
              display_name TEXT NOT NULL DEFAULT '',
              normalized_search TEXT NOT NULL DEFAULT '',
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL,
              PRIMARY KEY (guild_id, user_id)
            );

            CREATE TABLE IF NOT EXISTS capture_runs (
              capture_run_id TEXT PRIMARY KEY,
              guild_id TEXT NOT NULL,
              voice_channel_id TEXT NOT NULL,
              voice_bot_id TEXT NOT NULL DEFAULT '',
              started_at_ms BIGINT,
              ended_at_ms BIGINT,
              state TEXT NOT NULL DEFAULT '',
              mode TEXT NOT NULL DEFAULT '',
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS timeline_events (
              sequence BIGSERIAL PRIMARY KEY,
              event_id TEXT NOT NULL UNIQUE,
              scope_kind TEXT NOT NULL DEFAULT 'voice_channel',
              guild_id TEXT NOT NULL,
              scope_id TEXT NOT NULL,
              event_kind TEXT NOT NULL,
              started_at_ms BIGINT NOT NULL,
              ended_at_ms BIGINT NOT NULL,
              created_at_ms BIGINT NOT NULL,
              capture_run_id TEXT NOT NULL DEFAULT '',
              conversation_id TEXT NOT NULL DEFAULT '',
              speaker_user_id TEXT NOT NULL DEFAULT '',
              speaker_label TEXT NOT NULL DEFAULT '',
              text TEXT NOT NULL DEFAULT '',
              forgotten BOOLEAN NOT NULL DEFAULT FALSE,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS conversations (
              conversation_id TEXT PRIMARY KEY,
              scope_kind TEXT NOT NULL DEFAULT 'voice_channel',
              guild_id TEXT NOT NULL,
              scope_id TEXT NOT NULL,
              start_ms BIGINT,
              end_ms BIGINT,
              last_speech_at_ms BIGINT,
              state TEXT NOT NULL DEFAULT '',
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS transcription_slots (
              slot_id TEXT PRIMARY KEY,
              source_job_id TEXT NOT NULL UNIQUE,
              mux_job_id TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT 'queued',
              guild_id TEXT NOT NULL DEFAULT '',
              voice_channel_id TEXT NOT NULL DEFAULT '',
              capture_run_id TEXT NOT NULL DEFAULT '',
              voice_bot_id TEXT NOT NULL DEFAULT '',
              voice_bot_discord_user_id TEXT NOT NULL DEFAULT '',
              speaker_user_id TEXT NOT NULL DEFAULT '',
              speaker_label TEXT NOT NULL DEFAULT '',
              speaker_username TEXT NOT NULL DEFAULT '',
              segment_index BIGINT NOT NULL DEFAULT 0,
              segment_start_ms BIGINT NOT NULL DEFAULT 0,
              segment_end_ms BIGINT NOT NULL DEFAULT 0,
              duration_ms BIGINT NOT NULL DEFAULT 0,
              source_audio_path TEXT NOT NULL DEFAULT '',
              audio_checksum TEXT NOT NULL DEFAULT '',
              audio_bytes BIGINT NOT NULL DEFAULT 0,
              audio_format TEXT NOT NULL DEFAULT '',
              sample_rate_hz BIGINT NOT NULL DEFAULT 0,
              channels BIGINT NOT NULL DEFAULT 0,
              sample_width_bits BIGINT NOT NULL DEFAULT 0,
              post_processing TEXT NOT NULL DEFAULT '',
              transcription_source_id TEXT NOT NULL DEFAULT '',
              provider TEXT NOT NULL DEFAULT '',
              model TEXT NOT NULL DEFAULT '',
              priority BIGINT NOT NULL DEFAULT 0,
              mux_stream_id TEXT NOT NULL DEFAULT '',
              mux_start_ms BIGINT,
              mux_end_ms BIGINT,
              guard_before_ms BIGINT NOT NULL DEFAULT 0,
              guard_after_ms BIGINT NOT NULL DEFAULT 0,
              created_at_ms BIGINT NOT NULL,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS windows (
              window_id TEXT PRIMARY KEY,
              scope_kind TEXT NOT NULL DEFAULT 'voice_channel',
              guild_id TEXT NOT NULL,
              scope_id TEXT NOT NULL,
              start_ms BIGINT,
              end_ms BIGINT,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS publications (
              publication_id TEXT PRIMARY KEY,
              scope_kind TEXT NOT NULL DEFAULT 'voice_channel',
              guild_id TEXT NOT NULL,
              scope_id TEXT NOT NULL,
              window_id TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              created_at_ms BIGINT,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runtime_metadata (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL,
              updated_at_ms BIGINT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runtime_config (
              config_key TEXT PRIMARY KEY,
              updated_at_ms BIGINT NOT NULL,
              payload_json JSONB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS jobs (
              job_id TEXT PRIMARY KEY,
              scope_kind TEXT NOT NULL DEFAULT '',
              guild_id TEXT NOT NULL,
              scope_id TEXT NOT NULL,
              kind TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              terminal BOOLEAN NOT NULL DEFAULT FALSE,
              failed BOOLEAN NOT NULL DEFAULT FALSE,
              ephemeral BOOLEAN NOT NULL DEFAULT FALSE,
              cancellable BOOLEAN NOT NULL DEFAULT FALSE,
              lane TEXT NOT NULL DEFAULT '',
              ordering_key TEXT NOT NULL DEFAULT '',
              ready_at_ms BIGINT NOT NULL,
              created_at_ms BIGINT NOT NULL,
              updated_at_ms BIGINT NOT NULL,
              started_at_ms BIGINT,
              completed_at_ms BIGINT,
              gc_after_ms BIGINT,
              root_job_id TEXT NOT NULL DEFAULT '',
              parent_job_id TEXT,
              lineage_depth BIGINT NOT NULL DEFAULT 0,
              requested_by_user_id TEXT NOT NULL DEFAULT '',
              command_kind TEXT NOT NULL DEFAULT '',
              source_job_id TEXT NOT NULL DEFAULT '',
              stream_id TEXT NOT NULL DEFAULT '',
              target_job_id TEXT NOT NULL DEFAULT '',
              speaker_user_id TEXT NOT NULL DEFAULT '',
              segment_end_ms BIGINT
            );

            CREATE TABLE IF NOT EXISTS job_payloads (
              job_id TEXT PRIMARY KEY REFERENCES jobs(job_id) ON DELETE CASCADE,
              payload_blob BYTEA NOT NULL
            );

            CREATE TABLE IF NOT EXISTS job_dependencies (
              parent_job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
              child_job_id TEXT NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
              dependency_kind TEXT NOT NULL DEFAULT 'required',
              created_at_ms BIGINT NOT NULL,
              resolution_policy TEXT NOT NULL DEFAULT 'parent_resumes',
              PRIMARY KEY (parent_job_id, child_job_id)
            );

            CREATE TABLE IF NOT EXISTS automations (
              automation_id TEXT PRIMARY KEY,
              scope_kind TEXT NOT NULL DEFAULT 'voice_channel',
              guild_id TEXT NOT NULL,
              scope_id TEXT NOT NULL,
              state TEXT NOT NULL DEFAULT '',
              idempotency_key TEXT NOT NULL DEFAULT '',
              created_at_ms BIGINT,
              updated_at_ms BIGINT NOT NULL,
              expires_at_ms BIGINT,
              fire_count BIGINT NOT NULL DEFAULT 0,
              max_fires BIGINT,
              payload_blob BYTEA NOT NULL
            );

            CREATE TABLE IF NOT EXISTS agent_sessions (
              agent_session_id TEXT PRIMARY KEY,
              codex_session_id TEXT NOT NULL DEFAULT '',
              route_kind TEXT NOT NULL DEFAULT '',
              route_key TEXT NOT NULL DEFAULT '',
              guild_id TEXT NOT NULL DEFAULT '',
              scope_id TEXT NOT NULL DEFAULT '',
              dm_user_id TEXT NOT NULL DEFAULT '',
              voice_capture_session_id TEXT NOT NULL DEFAULT '',
              discord_thread_id TEXT NOT NULL DEFAULT '',
              discord_parent_channel_id TEXT NOT NULL DEFAULT '',
              text_target_kind TEXT NOT NULL DEFAULT '',
              text_channel_id TEXT NOT NULL DEFAULT '',
              text_user_id TEXT NOT NULL DEFAULT '',
              state TEXT NOT NULL DEFAULT '',
              created_at_ms BIGINT NOT NULL,
              last_activity_at_ms BIGINT NOT NULL,
              max_active_until_ms BIGINT NOT NULL,
              retired_at_ms BIGINT,
              retirement_reason TEXT NOT NULL DEFAULT '',
              retired_by_user_id TEXT NOT NULL DEFAULT '',
              resumed_from_agent_session_id TEXT NOT NULL DEFAULT '',
              payload_blob BYTEA NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn assert_schema_invariants(&self) -> Result<()> {
        let rows = sqlx::query(
            r#"
            SELECT table_name, column_name, data_type, is_nullable
            FROM information_schema.columns
            WHERE table_schema = current_schema()
            ORDER BY table_name, ordinal_position
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let expected_tables = EXPECTED_TABLE_SCHEMAS
            .iter()
            .map(|schema| schema.name)
            .collect::<BTreeSet<_>>();
        let mut actual_tables: BTreeMap<String, BTreeMap<String, ActualColumnSchema>> =
            BTreeMap::new();
        for row in rows {
            let table_name = row.get::<String, _>("table_name");
            if !expected_tables.contains(table_name.as_str()) {
                continue;
            }
            actual_tables.entry(table_name).or_default().insert(
                row.get::<String, _>("column_name"),
                ActualColumnSchema {
                    data_type: row.get::<String, _>("data_type"),
                    nullable: row.get::<String, _>("is_nullable") == "YES",
                },
            );
        }

        let mut problems = Vec::new();
        for expected_table in EXPECTED_TABLE_SCHEMAS {
            let Some(actual_columns) = actual_tables.get(expected_table.name) else {
                problems.push(format!("missing table {}", expected_table.name));
                continue;
            };
            let expected_columns = expected_table
                .columns
                .iter()
                .map(|column| (column.name, *column))
                .collect::<BTreeMap<_, _>>();
            for expected_column in expected_table.columns {
                let Some(actual_column) = actual_columns.get(expected_column.name) else {
                    problems.push(format!(
                        "{} missing column {}",
                        expected_table.name, expected_column.name
                    ));
                    continue;
                };
                if actual_column.data_type != expected_column.data_type
                    || actual_column.nullable != expected_column.nullable
                {
                    problems.push(format!(
                        "{}.{} expected {} nullable={} got {} nullable={}",
                        expected_table.name,
                        expected_column.name,
                        expected_column.data_type,
                        expected_column.nullable,
                        actual_column.data_type,
                        actual_column.nullable
                    ));
                }
            }
            for actual_column in actual_columns.keys() {
                if !expected_columns.contains_key(actual_column.as_str()) {
                    problems.push(format!(
                        "{} has stale column {}",
                        expected_table.name, actual_column
                    ));
                }
            }
        }

        let index_rows = sqlx::query(
            r#"
            SELECT tablename, indexname
            FROM pg_indexes
            WHERE schemaname = current_schema()
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let expected_indexes = EXPECTED_INDEXES
            .iter()
            .map(|(table, indexes)| {
                (
                    *table,
                    indexes.iter().copied().collect::<BTreeSet<&'static str>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut actual_indexes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for row in index_rows {
            let table_name = row.get::<String, _>("tablename");
            if !expected_tables.contains(table_name.as_str()) {
                continue;
            }
            actual_indexes
                .entry(table_name)
                .or_default()
                .insert(row.get::<String, _>("indexname"));
        }
        for (table, expected) in expected_indexes {
            let actual = actual_indexes.get(table).cloned().unwrap_or_default();
            for expected_index in &expected {
                if !actual.contains(*expected_index) {
                    problems.push(format!("{table} missing index {expected_index}"));
                }
            }
            for actual_index in actual {
                if !expected.contains(actual_index.as_str()) {
                    problems.push(format!("{table} has stale index {actual_index}"));
                }
            }
        }

        if !problems.is_empty() {
            anyhow::bail!(
                "timeline database schema does not match the hard-cut runtime contract: {}",
                problems.join("; ")
            );
        }
        Ok(())
    }

    async fn create_indexes(&self) -> Result<()> {
        sqlx::raw_sql(
            r#"
            CREATE INDEX IF NOT EXISTS idx_timeline_room_time
              ON timeline_events(scope_kind, scope_id, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_timeline_room_kind_time
              ON timeline_events(scope_kind, scope_id, event_kind, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_timeline_capture_run_time
              ON timeline_events(capture_run_id, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_timeline_conversation_time
              ON timeline_events(conversation_id, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_timeline_speaker_time
              ON timeline_events(speaker_user_id, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_voice_states_room_updated
              ON voice_states(guild_id, voice_channel_id, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_room_controls_updated
              ON room_controls(updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_discord_members_guild_normalized
              ON discord_members(guild_id, normalized_search);
            CREATE INDEX IF NOT EXISTS idx_timeline_kind_time
              ON timeline_events(event_kind, started_at_ms, sequence);
            CREATE INDEX IF NOT EXISTS idx_transcription_slots_state_priority
              ON transcription_slots(state, priority DESC, created_at_ms, slot_id);
            CREATE INDEX IF NOT EXISTS idx_transcription_slots_scope_speaker_time
              ON transcription_slots(guild_id, voice_channel_id, speaker_user_id, segment_start_ms, segment_end_ms);
            CREATE INDEX IF NOT EXISTS idx_transcription_slots_mux_job
              ON transcription_slots(mux_job_id, slot_id)
              WHERE mux_job_id <> '';
            CREATE INDEX IF NOT EXISTS idx_transcription_slots_source_state_created
              ON transcription_slots(transcription_source_id, state, priority DESC, created_at_ms, slot_id);
            CREATE INDEX IF NOT EXISTS idx_transcription_slots_source_mux_state
              ON transcription_slots(transcription_source_id, mux_job_id, state)
              WHERE mux_job_id <> '';
            CREATE INDEX IF NOT EXISTS idx_capture_runs_room_time
              ON capture_runs(guild_id, voice_channel_id, started_at_ms, ended_at_ms);
            CREATE INDEX IF NOT EXISTS idx_conversations_room_time
              ON conversations(scope_kind, scope_id, start_ms, end_ms);
            CREATE INDEX IF NOT EXISTS idx_publications_room_state
              ON publications(scope_kind, scope_id, state, created_at_ms);
            CREATE INDEX IF NOT EXISTS idx_capture_sessions_active_room
              ON capture_sessions(guild_id, voice_channel_id, active, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_due_kind
              ON jobs(kind, ready_at_ms, created_at_ms, job_id)
              WHERE state = 'queued';
            CREATE INDEX IF NOT EXISTS idx_jobs_queued_ready
              ON jobs(ready_at_ms, created_at_ms, job_id, kind)
              WHERE state = 'queued';
            CREATE INDEX IF NOT EXISTS idx_jobs_active_ordering
              ON jobs(ordering_key)
              WHERE terminal = FALSE AND ordering_key <> '';
            CREATE INDEX IF NOT EXISTS idx_jobs_active_visible_scope
              ON jobs(scope_kind, scope_id, updated_at_ms DESC, job_id)
              WHERE terminal = FALSE AND ephemeral = FALSE;
            CREATE INDEX IF NOT EXISTS idx_jobs_cancellable_scope_recent
              ON jobs(guild_id, scope_kind, scope_id, updated_at_ms DESC, created_at_ms DESC, job_id DESC)
              WHERE terminal = FALSE AND ephemeral = FALSE AND cancellable = TRUE;
            CREATE INDEX IF NOT EXISTS idx_jobs_agent_task_scope_recent
              ON jobs(guild_id, scope_kind, scope_id, updated_at_ms DESC, created_at_ms DESC, job_id DESC)
              WHERE kind = 'agent_task'
                AND ephemeral = FALSE
                AND state IN ('queued', 'running', 'waiting', 'cancel_requested', 'complete', 'failed', 'failed_timeout');
            CREATE INDEX IF NOT EXISTS idx_jobs_agent_task_requester_recent
              ON jobs(guild_id, scope_kind, scope_id, requested_by_user_id, updated_at_ms DESC, created_at_ms DESC, job_id DESC)
              WHERE kind = 'agent_task'
                AND ephemeral = FALSE
                AND state IN ('queued', 'running', 'waiting', 'cancel_requested', 'complete', 'failed', 'failed_timeout');
            CREATE INDEX IF NOT EXISTS idx_jobs_recent_visible
              ON jobs(updated_at_ms DESC, job_id)
              WHERE ephemeral = FALSE;
            CREATE INDEX IF NOT EXISTS idx_jobs_failed_visible
              ON jobs(updated_at_ms DESC, job_id)
              WHERE failed = TRUE AND ephemeral = FALSE;
            CREATE INDEX IF NOT EXISTS idx_jobs_scope_state_kind_updated
              ON jobs(scope_kind, scope_id, state, kind, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_scope_kind_updated
              ON jobs(scope_kind, scope_id, kind, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_kind_updated
              ON jobs(kind, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_state_updated
              ON jobs(state, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_jobs_ephemeral_gc
              ON jobs(gc_after_ms, job_id)
              WHERE ephemeral = TRUE AND terminal = TRUE;
            CREATE INDEX IF NOT EXISTS idx_jobs_wake_stream_queued
              ON jobs(stream_id, ready_at_ms, created_at_ms, job_id)
              WHERE kind = 'wake_probe' AND state = 'queued';
            CREATE INDEX IF NOT EXISTS idx_jobs_text_delivery_source
              ON jobs(source_job_id, updated_at_ms DESC, job_id)
              WHERE kind = 'text_delivery';
            CREATE INDEX IF NOT EXISTS idx_jobs_audio_segment_pending_speaker
              ON jobs(guild_id, scope_id, speaker_user_id, segment_end_ms, job_id)
              WHERE scope_kind = 'voice_channel' AND kind = 'audio_segment' AND terminal = FALSE;
            CREATE INDEX IF NOT EXISTS idx_job_dependencies_child
              ON job_dependencies(child_job_id, parent_job_id);
            CREATE INDEX IF NOT EXISTS idx_automations_scope_state
              ON automations(scope_kind, scope_id, state, expires_at_ms);
            CREATE INDEX IF NOT EXISTS idx_automations_idempotency
              ON automations(scope_kind, scope_id, idempotency_key, state);
            CREATE INDEX IF NOT EXISTS idx_agent_sessions_route
              ON agent_sessions(route_key, state, max_active_until_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_agent_sessions_thread
              ON agent_sessions(discord_thread_id, state, max_active_until_ms DESC)
              WHERE discord_thread_id <> '';
            CREATE INDEX IF NOT EXISTS idx_agent_sessions_codex
              ON agent_sessions(codex_session_id)
              WHERE codex_session_id <> '';
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
