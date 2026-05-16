use crate::config::{ControlConfig, GuildConfig, PoolConfig};
use crate::runtime::RoomConfig;

use super::*;

const RUNTIME_POOL_CONFIG_KEY: &str = "runtime_pool";
const CONTROL_CONFIG_KEY: &str = "control";
const GUILDS_CONFIG_KEY: &str = "guilds";
const ROOMS_CONFIG_KEY: &str = "rooms";

impl TimelineStore {
    pub async fn write_runtime_config_snapshot(
        &self,
        pool: &PoolConfig,
        control: &ControlConfig,
        guilds: &[GuildConfig],
        rooms: &[RoomConfig],
    ) -> Result<()> {
        self.upsert_runtime_config_value(RUNTIME_POOL_CONFIG_KEY, &serde_json::to_value(pool)?)
            .await?;
        self.upsert_runtime_config_value(CONTROL_CONFIG_KEY, &serde_json::to_value(control)?)
            .await?;
        self.upsert_runtime_config_value(GUILDS_CONFIG_KEY, &serde_json::to_value(guilds)?)
            .await?;
        self.upsert_runtime_config_value(ROOMS_CONFIG_KEY, &serde_json::to_value(rooms)?)
            .await?;
        Ok(())
    }

    pub async fn runtime_pool_config(&self) -> Result<PoolConfig> {
        self.get_runtime_config_value(RUNTIME_POOL_CONFIG_KEY).await
    }

    pub async fn control_config(&self) -> Result<ControlConfig> {
        self.get_runtime_config_value(CONTROL_CONFIG_KEY).await
    }

    pub async fn list_guild_configs(&self) -> Result<Vec<GuildConfig>> {
        self.get_runtime_config_value(GUILDS_CONFIG_KEY).await
    }

    pub async fn list_room_configs(&self) -> Result<Vec<RoomConfig>> {
        self.get_runtime_config_value(ROOMS_CONFIG_KEY).await
    }

    async fn upsert_runtime_config_value(&self, key: &str, payload: &Value) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO runtime_config(config_key, updated_at_ms, payload_json)
            VALUES ($1, $2, $3)
            ON CONFLICT (config_key) DO UPDATE SET
              updated_at_ms = EXCLUDED.updated_at_ms,
              payload_json = EXCLUDED.payload_json
            "#,
        )
        .bind(key)
        .bind(instant_ms_dt(utc_now()))
        .bind(payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_runtime_config_value<T>(&self, key: &str) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let row = sqlx::query("SELECT payload_json FROM runtime_config WHERE config_key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            anyhow::bail!("runtime config `{key}` is not stored in postgres");
        };
        serde_json::from_value(json_value(&row, "payload_json")?).map_err(Into::into)
    }
}
