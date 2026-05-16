use serde_json::json;

use super::*;
use crate::runtime::{RoomConfig, VoiceAssignment, VoiceBotStatus, VoiceCaptureSessionStatus};

const ACTIVE_ASSIGNMENT_STATES: &[&str] = &["joining", "capturing", "leaving"];

impl TimelineStore {
    pub async fn upsert_voice_bot_state(&self, status: &VoiceBotStatus) -> Result<()> {
        let updated_ms = instant_ms_dt(utc_now());
        sqlx::query(
            r#"
            INSERT INTO bot_states(bot_id, updated_at_ms, payload_json)
            VALUES ($1, $2, $3)
            ON CONFLICT(bot_id) DO UPDATE SET
              updated_at_ms = EXCLUDED.updated_at_ms,
              payload_json = EXCLUDED.payload_json
            "#,
        )
        .bind(&status.bot_id)
        .bind(updated_ms)
        .bind(status.to_json())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_voice_bot_states(&self, statuses: &[VoiceBotStatus]) -> Result<()> {
        for status in statuses {
            self.upsert_voice_bot_state(status).await?;
        }
        Ok(())
    }

    pub async fn list_voice_bot_states(&self) -> Result<Vec<VoiceBotStatus>> {
        let rows = sqlx::query("SELECT payload_json FROM bot_states ORDER BY bot_id")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                serde_json::from_value(json_value(&row, "payload_json")?).map_err(Into::into)
            })
            .collect()
    }

    pub async fn upsert_capture_session_status(
        &self,
        session: &VoiceCaptureSessionStatus,
    ) -> Result<()> {
        let payload = session.to_json();
        let active = session.active && session.ended_at.trim().is_empty();
        let started_ms = instant_ms_str(Some(&session.started_at));
        let ended_ms = instant_ms_str(Some(&session.ended_at));
        let updated_ms = instant_ms_dt(utc_now());
        sqlx::query(
            r#"
            INSERT INTO capture_sessions(
              session_id, assignment_id, capture_run_id, guild_id, voice_channel_id,
              bot_id, active, started_at_ms, ended_at_ms, updated_at_ms, payload_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT(session_id) DO UPDATE SET
              assignment_id = EXCLUDED.assignment_id,
              capture_run_id = EXCLUDED.capture_run_id,
              guild_id = EXCLUDED.guild_id,
              voice_channel_id = EXCLUDED.voice_channel_id,
              bot_id = EXCLUDED.bot_id,
              active = EXCLUDED.active,
              started_at_ms = EXCLUDED.started_at_ms,
              ended_at_ms = EXCLUDED.ended_at_ms,
              updated_at_ms = EXCLUDED.updated_at_ms,
              payload_json = EXCLUDED.payload_json
            "#,
        )
        .bind(&session.session_id)
        .bind(&session.assignment_id)
        .bind(&session.capture_run_id)
        .bind(&session.guild_id)
        .bind(&session.voice_channel_id)
        .bind(&session.bot_id)
        .bind(active)
        .bind(started_ms)
        .bind(ended_ms)
        .bind(updated_ms)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_capture_session_statuses(
        &self,
        sessions: &[VoiceCaptureSessionStatus],
    ) -> Result<()> {
        for session in sessions {
            self.upsert_capture_session_status(session).await?;
        }
        Ok(())
    }

    pub async fn list_active_capture_sessions(&self) -> Result<Vec<VoiceCaptureSessionStatus>> {
        self.list_capture_sessions(true).await
    }

    pub async fn list_active_capture_sessions_for_room(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
    ) -> Result<Vec<VoiceCaptureSessionStatus>> {
        let rows = sqlx::query(
            r#"
            SELECT payload_json
            FROM capture_sessions
            WHERE active = TRUE AND guild_id = $1 AND voice_channel_id = $2
            ORDER BY started_at_ms, session_id
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                serde_json::from_value(json_value(&row, "payload_json")?).map_err(Into::into)
            })
            .collect()
    }

    pub async fn get_capture_session_status(
        &self,
        session_id: &str,
    ) -> Result<Option<VoiceCaptureSessionStatus>> {
        let row = sqlx::query("SELECT payload_json FROM capture_sessions WHERE session_id = $1")
            .bind(session_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| serde_json::from_value(json_value(&row, "payload_json")?).map_err(Into::into))
            .transpose()
    }

    pub async fn list_capture_sessions(
        &self,
        active_only: bool,
    ) -> Result<Vec<VoiceCaptureSessionStatus>> {
        let rows = if active_only {
            sqlx::query(
                "SELECT payload_json FROM capture_sessions WHERE active = TRUE ORDER BY started_at_ms, session_id",
            )
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query("SELECT payload_json FROM capture_sessions ORDER BY updated_at_ms DESC")
                .fetch_all(&self.pool)
                .await?
        };
        rows.into_iter()
            .map(|row| {
                serde_json::from_value(json_value(&row, "payload_json")?).map_err(Into::into)
            })
            .collect()
    }

    pub async fn list_active_voice_assignments(&self) -> Result<Vec<VoiceAssignment>> {
        self.list_voice_assignments_by_states(ACTIVE_ASSIGNMENT_STATES)
            .await
    }

    pub async fn list_active_voice_assignments_for_room(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
    ) -> Result<Vec<VoiceAssignment>> {
        let rows = sqlx::query(
            r#"
            SELECT payload_json
            FROM assignments
            WHERE guild_id = $1 AND voice_channel_id = $2 AND state = ANY($3)
            ORDER BY COALESCE(assigned_at_ms, updated_at_ms), assignment_id
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .bind(ACTIVE_ASSIGNMENT_STATES)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_voice_assignment(json_value(&row, "payload_json")?))
            .collect()
    }

    pub async fn list_voice_assignments_by_states(
        &self,
        states: &[&str],
    ) -> Result<Vec<VoiceAssignment>> {
        let rows = sqlx::query(
            r#"
            SELECT payload_json
            FROM assignments
            WHERE cardinality($1::text[]) = 0 OR state = ANY($1)
            ORDER BY COALESCE(assigned_at_ms, updated_at_ms), assignment_id
            "#,
        )
        .bind(states)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| decode_voice_assignment(json_value(&row, "payload_json")?))
            .collect()
    }

    pub async fn get_voice_assignment(
        &self,
        assignment_id: &str,
    ) -> Result<Option<VoiceAssignment>> {
        let row = sqlx::query("SELECT payload_json FROM assignments WHERE assignment_id = $1")
            .bind(assignment_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| decode_voice_assignment(json_value(&row, "payload_json")?))
            .transpose()
    }

    pub async fn get_voice_assignment_by_capture_run(
        &self,
        capture_run_id: &str,
    ) -> Result<Option<VoiceAssignment>> {
        let row = sqlx::query(
            "SELECT payload_json FROM assignments WHERE capture_run_id = $1 ORDER BY updated_at_ms DESC LIMIT 1",
        )
        .bind(capture_run_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| decode_voice_assignment(json_value(&row, "payload_json")?))
            .transpose()
    }

    pub async fn claim_voice_assignment_for_room(
        &self,
        room: &RoomConfig,
        reason: &str,
    ) -> Result<Option<VoiceAssignment>> {
        self.ensure_room(
            &room.guild_id,
            &room.channel_id,
            &room.guild_slug,
            &room.channel_name,
            &room.channel_slug,
        )
        .await?;
        let started = utc_now();
        let started_ms = instant_ms_dt(started);
        let capture_run_id = new_id("cap");
        let assignment_id = new_id("assign");
        let mut transaction = self.pool.begin().await?;
        let bot_row = sqlx::query(
            r#"
            SELECT bot_id, payload_json
            FROM bot_states bot
            WHERE (bot.payload_json->>'ready')::boolean = TRUE
              AND NOT EXISTS (
                SELECT 1
                FROM assignments assignment
                WHERE assignment.voice_bot_id = bot.bot_id
                  AND assignment.state = ANY($1)
              )
            ORDER BY bot.bot_id
            LIMIT 1
            FOR UPDATE OF bot SKIP LOCKED
            "#,
        )
        .bind(ACTIVE_ASSIGNMENT_STATES)
        .fetch_optional(transaction.as_mut())
        .await?;
        let Some(bot_row) = bot_row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let bot: VoiceBotStatus = serde_json::from_value(json_value(&bot_row, "payload_json")?)?;
        let assignment = VoiceAssignment {
            assignment_id: assignment_id.clone(),
            guild_id: room.guild_id.clone(),
            voice_channel_id: room.channel_id.clone(),
            voice_channel_name: room.channel_name.clone(),
            voice_bot_id: bot.bot_id.clone(),
            voice_bot_discord_user_id: bot.user_id.clone(),
            capture_run_id: capture_run_id.clone(),
            state: "joining".to_string(),
            mode: "local_buffering".to_string(),
            assigned_at: isoformat_z(Some(started)),
            released_at: String::new(),
            assignment_reason: reason.to_string(),
            release_reason: String::new(),
        };
        let retention_policy = json!({
            "draft_transcript_events": "7d",
            "source_audio": "7d",
            "job_metadata": "30d"
        });
        let run = json!({
            "capture_run_id": capture_run_id,
            "captureRunId": capture_run_id,
            "assignment_id": assignment_id,
            "assignmentId": assignment_id,
            "guild_id": room.guild_id,
            "guildId": room.guild_id,
            "guild_slug": room.guild_slug,
            "guildSlug": room.guild_slug,
            "voice_channel_id": room.channel_id,
            "channelId": room.channel_id,
            "voice_channel_name": room.channel_name,
            "channelName": room.channel_name,
            "voice_channel_slug": room.channel_slug,
            "channelSlug": room.channel_slug,
            "voice_bot_id": bot.bot_id,
            "botId": bot.bot_id,
            "voice_bot_discord_user_id": bot.user_id,
            "botUserId": bot.user_id,
            "started_at": isoformat_z(Some(started)),
            "startedAt": isoformat_z(Some(started)),
            "ended_at": Value::Null,
            "endedAt": "",
            "state": "joining",
            "mode": "local_buffering",
            "retention_policy": retention_policy,
            "retentionPolicy": retention_policy
        });
        sqlx::query(
            r#"
            INSERT INTO capture_runs(
              capture_run_id, guild_id, voice_channel_id, voice_bot_id, started_at_ms,
              ended_at_ms, state, mode, updated_at_ms, payload_json
            )
            VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, $8, $9)
            "#,
        )
        .bind(&assignment.capture_run_id)
        .bind(&assignment.guild_id)
        .bind(&assignment.voice_channel_id)
        .bind(&assignment.voice_bot_id)
        .bind(started_ms)
        .bind(&assignment.state)
        .bind(&assignment.mode)
        .bind(started_ms)
        .bind(&run)
        .execute(transaction.as_mut())
        .await?;
        upsert_voice_assignment_in_tx(transaction.as_mut(), &assignment, None).await?;
        transaction.commit().await?;
        self.append_event(
            &assignment.guild_id,
            &assignment.voice_channel_id,
            json!({
                "event_kind": "voice_bot_assigned",
                "kind": "voice_bot_assigned",
                "assignment_id": assignment.assignment_id,
                "capture_run_id": assignment.capture_run_id,
                "voice_bot_id": assignment.voice_bot_id,
                "voice_bot_discord_user_id": assignment.voice_bot_discord_user_id,
                "voice_channel_name": assignment.voice_channel_name,
                "assigned_at": assignment.assigned_at,
                "state": assignment.state,
                "mode": assignment.mode,
                "assignment_reason": assignment.assignment_reason
            }),
        )
        .await?;
        Ok(Some(assignment))
    }

    pub async fn mark_voice_assignment_capturing(
        &self,
        assignment_id: &str,
    ) -> Result<Option<VoiceAssignment>> {
        self.mark_voice_assignment_state(assignment_id, "capturing", None, "")
            .await
    }

    pub async fn mark_voice_assignment_leaving(
        &self,
        assignment_id: &str,
        reason: &str,
    ) -> Result<Option<VoiceAssignment>> {
        self.mark_voice_assignment_state(assignment_id, "leaving", None, reason)
            .await
    }

    pub async fn mark_voice_assignment_failed(
        &self,
        assignment_id: &str,
        reason: &str,
    ) -> Result<Option<VoiceAssignment>> {
        self.mark_voice_assignment_state(assignment_id, "failed", Some(utc_now()), reason)
            .await
    }

    pub async fn mark_capture_session_ended(
        &self,
        session_id: &str,
        ended_at: DateTime<Utc>,
    ) -> Result<()> {
        let row = sqlx::query("SELECT payload_json FROM capture_sessions WHERE session_id = $1")
            .bind(session_id)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(());
        };
        let mut session: VoiceCaptureSessionStatus =
            serde_json::from_value(json_value(&row, "payload_json")?)?;
        session.mark_ended(isoformat_z(Some(ended_at)));
        self.upsert_capture_session_status(&session).await
    }

    async fn mark_voice_assignment_state(
        &self,
        assignment_id: &str,
        state: &str,
        released_at: Option<DateTime<Utc>>,
        reason: &str,
    ) -> Result<Option<VoiceAssignment>> {
        let Some(mut assignment) = self.get_voice_assignment(assignment_id).await? else {
            return Ok(None);
        };
        assignment.state = state.to_string();
        if let Some(released_at) = released_at {
            assignment.released_at = isoformat_z(Some(released_at));
        }
        if !reason.trim().is_empty() {
            assignment.release_reason = reason.to_string();
        }
        let updated_ms = released_at
            .map(instant_ms_dt)
            .unwrap_or_else(|| instant_ms_dt(utc_now()));
        let mut transaction = self.pool.begin().await?;
        upsert_voice_assignment_in_tx(transaction.as_mut(), &assignment, Some(updated_ms)).await?;
        if state == "capturing" {
            sqlx::query(
                r#"
                UPDATE capture_runs
                SET state = 'active', updated_at_ms = $1,
                    payload_json = jsonb_set(payload_json, '{state}', '"active"', true)
                WHERE capture_run_id = $2
                "#,
            )
            .bind(updated_ms)
            .bind(&assignment.capture_run_id)
            .execute(transaction.as_mut())
            .await?;
        }
        transaction.commit().await?;
        Ok(Some(assignment))
    }
}

async fn upsert_voice_assignment_in_tx(
    transaction: &mut sqlx::PgConnection,
    assignment: &VoiceAssignment,
    updated_ms: Option<i64>,
) -> Result<()> {
    let assigned_ms = instant_ms_str(Some(&assignment.assigned_at));
    let released_ms = instant_ms_str(Some(&assignment.released_at));
    let updated_ms = updated_ms
        .or(released_ms)
        .or(assigned_ms)
        .unwrap_or_else(|| instant_ms_dt(utc_now()));
    sqlx::query(
        r#"
        INSERT INTO assignments(
          assignment_id, guild_id, voice_channel_id, voice_bot_id, capture_run_id,
          state, assigned_at_ms, released_at_ms, updated_at_ms, payload_json
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ON CONFLICT(assignment_id) DO UPDATE SET
          guild_id = EXCLUDED.guild_id,
          voice_channel_id = EXCLUDED.voice_channel_id,
          voice_bot_id = EXCLUDED.voice_bot_id,
          capture_run_id = EXCLUDED.capture_run_id,
          state = EXCLUDED.state,
          assigned_at_ms = EXCLUDED.assigned_at_ms,
          released_at_ms = EXCLUDED.released_at_ms,
          updated_at_ms = EXCLUDED.updated_at_ms,
          payload_json = EXCLUDED.payload_json
        "#,
    )
    .bind(&assignment.assignment_id)
    .bind(&assignment.guild_id)
    .bind(&assignment.voice_channel_id)
    .bind(&assignment.voice_bot_id)
    .bind(&assignment.capture_run_id)
    .bind(&assignment.state)
    .bind(assigned_ms)
    .bind(released_ms)
    .bind(updated_ms)
    .bind(assignment.to_json())
    .execute(transaction)
    .await?;
    Ok(())
}

fn decode_voice_assignment(value: Value) -> Result<VoiceAssignment> {
    Ok(VoiceAssignment {
        assignment_id: first_value_string(&value, &["assignmentId", "assignment_id"]),
        guild_id: first_value_string(&value, &["guildId", "guild_id"]),
        voice_channel_id: first_value_string(&value, &["voiceChannelId", "voice_channel_id"]),
        voice_channel_name: first_value_string(&value, &["voiceChannelName", "voice_channel_name"]),
        voice_bot_id: first_value_string(&value, &["voiceBotId", "voice_bot_id", "botId"]),
        voice_bot_discord_user_id: first_value_string(
            &value,
            &[
                "voiceBotDiscordUserId",
                "voice_bot_discord_user_id",
                "botUserId",
            ],
        ),
        capture_run_id: first_value_string(&value, &["captureRunId", "capture_run_id"]),
        state: first_value_string(&value, &["state"]),
        mode: first_value_string(&value, &["mode"]),
        assigned_at: first_value_string(&value, &["assignedAt", "assigned_at"]),
        released_at: first_value_string(&value, &["releasedAt", "released_at"]),
        assignment_reason: first_value_string(&value, &["assignmentReason", "assignment_reason"]),
        release_reason: first_value_string(&value, &["releaseReason", "release_reason"]),
    })
}
