use super::*;

use crate::runtime::RoomControl;

impl TimelineStore {
    pub async fn list_room_controls(&self) -> Result<BTreeMap<String, RoomControl>> {
        let rows = sqlx::query(
            r#"
            SELECT payload_json
            FROM room_controls
            ORDER BY voice_channel_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut controls = BTreeMap::new();
        for row in rows {
            let control = decode_room_control(json_value(&row, "payload_json")?)?;
            controls.insert(control.voice_channel_id.clone(), control);
        }
        Ok(controls)
    }

    pub async fn get_room_control(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
    ) -> Result<Option<RoomControl>> {
        let row = sqlx::query(
            r#"
            SELECT payload_json
            FROM room_controls
            WHERE guild_id = $1 AND voice_channel_id = $2
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| decode_room_control(json_value(&row, "payload_json")?))
            .transpose()
    }

    pub async fn upsert_room_control(&self, control: &RoomControl) -> Result<()> {
        let updated_at_ms = instant_ms_str(Some(&control.updated_at)).ok_or_else(|| {
            anyhow::anyhow!(
                "room control {}:{} has invalid updated_at `{}`",
                control.guild_id,
                control.voice_channel_id,
                control.updated_at
            )
        })?;
        let payload = serde_json::to_value(control)?;
        sqlx::query(
            r#"
            INSERT INTO room_controls(guild_id, voice_channel_id, updated_at_ms, payload_json)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT(guild_id, voice_channel_id) DO UPDATE SET
              updated_at_ms = EXCLUDED.updated_at_ms,
              payload_json = EXCLUDED.payload_json
            "#,
        )
        .bind(&control.guild_id)
        .bind(&control.voice_channel_id)
        .bind(updated_at_ms)
        .bind(&payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_room_control(&self, guild_id: &str, voice_channel_id: &str) -> Result<()> {
        sqlx::query(
            r#"
            DELETE FROM room_controls
            WHERE guild_id = $1 AND voice_channel_id = $2
            "#,
        )
        .bind(guild_id)
        .bind(voice_channel_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn prune_expired_room_controls(&self) -> Result<usize> {
        let rows = sqlx::query(
            r#"
            SELECT payload_json
            FROM room_controls
            ORDER BY voice_channel_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let now = utc_now();
        let mut changed = 0usize;
        for row in rows {
            let mut control = decode_room_control(json_value(&row, "payload_json")?)?;
            let before = control.clone();
            clear_expired_marker(&mut control, "auto_join_suppressed_until", now);
            clear_expired_marker(&mut control, "manual_hold_until", now);
            clear_expired_marker(&mut control, "listening_paused_until", now);
            if control == before {
                continue;
            }
            changed += 1;
            if control.has_active_marker() {
                control.updated_at = isoformat_z(Some(now));
                self.upsert_room_control(&control).await?;
            } else {
                self.delete_room_control(&control.guild_id, &control.voice_channel_id)
                    .await?;
            }
        }
        Ok(changed)
    }
}

fn decode_room_control(payload: Value) -> Result<RoomControl> {
    Ok(serde_json::from_value(payload)?)
}

fn clear_expired_marker(control: &mut RoomControl, key: &str, now: DateTime<Utc>) {
    if room_control_datetime(control, key).is_some_and(|value| value <= now) {
        control.clear_key(key);
    }
}

fn room_control_datetime(control: &RoomControl, key: &str) -> Option<DateTime<Utc>> {
    let value = match key {
        "auto_join_suppressed_until" => control.auto_join_suppressed_until.as_deref(),
        "manual_hold_until" => control.manual_hold_until.as_deref(),
        "listening_paused_until" => control.listening_paused_until.as_deref(),
        _ => None,
    }?;
    parse_instant(value)
}
