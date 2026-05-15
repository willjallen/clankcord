use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::Result;
use anyhow::Context;
use sqlx::Row;

use crate::runtime::timeline::{TimelineStore, instant_ms_str, isoformat_z, new_id, parse_instant};
use crate::runtime::{JobKind, JobState, Runtime};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationSpec {
    #[serde(default = "default_schema")]
    pub schema: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, alias = "idempotencyKey")]
    pub idempotency_key: String,
    #[serde(default)]
    pub owner: AutomationOwner,
    #[serde(default)]
    pub scope: AutomationScope,
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default)]
    pub condition: AutomationCondition,
    #[serde(default)]
    pub expiry: AutomationExpiry,
    #[serde(default)]
    pub actions: Vec<AutomationAction>,
}

impl AutomationSpec {
    pub fn from_json(value: &Value) -> Result<Self> {
        validate_boundary_automation_value(value)?;
        let mut spec: Self = serde_json::from_value(normalize_boundary_automation_value(value))
            .context("failed to parse normalized automation spec")?;
        spec.normalize();
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }

    pub fn normalize(&mut self) {
        if self.schema.trim().is_empty() {
            self.schema = default_schema();
        }
        if self.expiry.max_fires.is_none() {
            self.expiry.max_fires = Some(1);
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != default_schema() {
            anyhow::bail!("unsupported automation schema: {}", self.schema);
        }
        if self.name.trim().is_empty() {
            anyhow::bail!("automation name is required");
        }
        self.owner.validate()?;
        self.scope.validate()?;
        self.trigger.validate()?;
        self.condition.validate()?;
        self.expiry.validate()?;
        if self.actions.is_empty() {
            anyhow::bail!("automation must define at least one action");
        }
        for action in &self.actions {
            action.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AutomationOwner {
    Agent {
        #[serde(default, alias = "userId")]
        user_id: String,
        #[serde(default, alias = "sourceJobId")]
        source_job_id: String,
    },
    User {
        #[serde(default, alias = "userId")]
        user_id: String,
    },
    System,
}

impl Default for AutomationOwner {
    fn default() -> Self {
        Self::System
    }
}

impl AutomationOwner {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Agent { source_job_id, .. } if source_job_id.trim().is_empty() => {
                anyhow::bail!("agent-owned automations require source_job_id")
            }
            Self::User { user_id } if user_id.trim().is_empty() => {
                anyhow::bail!("user-owned automations require user_id")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationScope {
    #[serde(default, alias = "guildId")]
    pub guild_id: String,
    #[serde(
        default,
        alias = "voiceChannelId",
        alias = "channel_id",
        alias = "channelId"
    )]
    pub voice_channel_id: String,
}

impl AutomationScope {
    fn validate(&self) -> Result<()> {
        if self.guild_id.trim().is_empty() || self.voice_channel_id.trim().is_empty() {
            anyhow::bail!("automation scope requires guild_id and voice_channel_id");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AutomationTrigger {
    Tick {
        #[serde(default, alias = "intervalSeconds")]
        interval_seconds: u64,
    },
    Event {
        #[serde(default, alias = "eventKinds")]
        event_kinds: Vec<String>,
    },
    Job {
        #[serde(
            default,
            alias = "jobKinds",
            deserialize_with = "deserialize_job_kinds",
            serialize_with = "serialize_job_kinds"
        )]
        job_kinds: Vec<JobKind>,
        #[serde(
            default,
            deserialize_with = "deserialize_job_states",
            serialize_with = "serialize_job_states"
        )]
        states: Vec<JobState>,
    },
    RoomStateChanged,
}

impl Default for AutomationTrigger {
    fn default() -> Self {
        Self::Event {
            event_kinds: Vec::new(),
        }
    }
}

impl AutomationTrigger {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Tick { interval_seconds } if *interval_seconds == 0 => {
                anyhow::bail!("tick trigger requires interval_seconds > 0")
            }
            Self::Event { event_kinds } if event_kinds.is_empty() => {
                anyhow::bail!("event trigger requires event_kinds")
            }
            Self::Job { job_kinds, states } if job_kinds.is_empty() || states.is_empty() => {
                anyhow::bail!("job trigger requires job_kinds and states")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AutomationCondition {
    True,
    All {
        #[serde(default)]
        conditions: Vec<AutomationCondition>,
    },
    Any {
        #[serde(default)]
        conditions: Vec<AutomationCondition>,
    },
    Not {
        condition: Box<AutomationCondition>,
    },
    Predicate {
        path: String,
        op: AutomationConditionOp,
        #[serde(default)]
        value: Option<AutomationScalar>,
    },
}

impl Default for AutomationCondition {
    fn default() -> Self {
        Self::True
    }
}

impl AutomationCondition {
    fn validate(&self) -> Result<()> {
        match self {
            Self::True => Ok(()),
            Self::All { conditions } | Self::Any { conditions } => {
                if conditions.is_empty() {
                    anyhow::bail!("compound automation condition requires child conditions");
                }
                for condition in conditions {
                    condition.validate()?;
                }
                Ok(())
            }
            Self::Not { condition } => condition.validate(),
            Self::Predicate { path, op, value } => {
                if path.trim().is_empty() {
                    anyhow::bail!("predicate condition requires path");
                }
                if op.requires_value() && value.is_none() {
                    anyhow::bail!("predicate condition op {op:?} requires value");
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationConditionOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    Present,
    Empty,
    Matches,
}

impl AutomationConditionOp {
    fn requires_value(self) -> bool {
        !matches!(self, Self::Present | Self::Empty)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AutomationScalar {
    String(String),
    Number(f64),
    Bool(bool),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationExpiry {
    #[serde(default, alias = "maxFires")]
    pub max_fires: Option<u64>,
    #[serde(default, alias = "expiresAt")]
    pub expires_at: Option<String>,
}

impl AutomationExpiry {
    fn validate(&self) -> Result<()> {
        if self.max_fires == Some(0) {
            anyhow::bail!("automation expiry max_fires must be greater than 0");
        }
        if let Some(expires_at) = self.expires_at.as_deref() {
            if parse_instant(expires_at).is_none() {
                anyhow::bail!("automation expiry expires_at must be an RFC3339 timestamp");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AutomationAction {
    ResponseSend {
        sink: AutomationResponseSink,
        content: String,
    },
    AgentTaskStart {
        prompt: String,
        #[serde(default, alias = "responseSink")]
        response_sink: Option<AutomationResponseSink>,
    },
    SoundPlay {
        name: String,
    },
    TranscriptStartLive {
        #[serde(default)]
        title: String,
    },
}

impl AutomationAction {
    fn validate(&self) -> Result<()> {
        match self {
            Self::ResponseSend { sink, content } => {
                sink.validate()?;
                if content.trim().is_empty() {
                    anyhow::bail!("response.send action requires content");
                }
                Ok(())
            }
            Self::AgentTaskStart {
                prompt,
                response_sink,
            } => {
                if prompt.trim().is_empty() {
                    anyhow::bail!("agent_task.start action requires prompt");
                }
                if let Some(sink) = response_sink {
                    sink.validate()?;
                }
                Ok(())
            }
            Self::SoundPlay { name } if name.trim().is_empty() => {
                anyhow::bail!("sound.play action requires name")
            }
            Self::TranscriptStartLive { .. } => Ok(()),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutomationResponseSink {
    pub kind: AutomationResponseSinkKind,
    #[serde(
        default,
        alias = "channel_id",
        alias = "channelId",
        alias = "user_id",
        alias = "userId"
    )]
    pub id: String,
}

impl AutomationResponseSink {
    fn validate(&self) -> Result<()> {
        if matches!(
            self.kind,
            AutomationResponseSinkKind::Channel | AutomationResponseSinkKind::Dm
        ) && self.id.trim().is_empty()
        {
            anyhow::bail!("channel and dm response sinks require id");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationResponseSinkKind {
    AgentChat,
    Channel,
    Dm,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRecord {
    pub automation_id: String,
    pub state: AutomationState,
    pub created_at: String,
    pub updated_at: String,
    pub last_evaluated_at: String,
    pub last_fired_at: String,
    pub fire_count: u64,
    pub spec: AutomationSpec,
}

impl AutomationRecord {
    pub fn new(spec: AutomationSpec) -> Self {
        let now = isoformat_z(None);
        Self {
            automation_id: new_id("aut"),
            state: AutomationState::Active,
            created_at: now.clone(),
            updated_at: now,
            last_evaluated_at: String::new(),
            last_fired_at: String::new(),
            fire_count: 0,
            spec,
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }

    fn encode(&self) -> Result<Vec<u8>> {
        Ok(bincode::serialize(self)?)
    }

    fn decode(payload: &[u8]) -> Result<Self> {
        Ok(bincode::deserialize(payload)?)
    }

    pub(crate) fn mark_evaluated(&mut self) {
        self.updated_at = isoformat_z(None);
        self.last_evaluated_at = self.updated_at.clone();
    }

    pub(crate) fn mark_fired(&mut self) {
        self.updated_at = isoformat_z(None);
        self.last_evaluated_at = self.updated_at.clone();
        self.last_fired_at = self.updated_at.clone();
        self.fire_count += 1;
        if self
            .spec
            .expiry
            .max_fires
            .is_some_and(|max_fires| self.fire_count >= max_fires)
        {
            self.state = AutomationState::Expired;
        }
    }

    pub(crate) fn cursor_at(&self) -> String {
        if self.last_evaluated_at.trim().is_empty() {
            self.created_at.clone()
        } else {
            self.last_evaluated_at.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationState {
    Active,
    Cancelled,
    Expired,
}

impl AutomationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

impl std::str::FromStr for AutomationState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "active" => Ok(Self::Active),
            "cancelled" => Ok(Self::Cancelled),
            "expired" => Ok(Self::Expired),
            other => anyhow::bail!("unknown automation state: {other}"),
        }
    }
}

impl TimelineStore {
    pub async fn create_automation(&self, spec: AutomationSpec) -> Result<AutomationRecord> {
        spec.validate()?;
        let mut transaction = self.pool.begin().await?;
        if let Some(existing) =
            find_active_automation_by_idempotency_key_in_tx(&mut transaction, &spec).await?
        {
            transaction.commit().await?;
            return Ok(existing);
        }
        if let Some(source_job_id) = agent_owner_source_job_id(&spec.owner) {
            lock_agent_automation_source(&mut transaction, source_job_id).await?;
            if let Some(existing) =
                find_active_agent_automation_by_source_in_tx(&mut transaction, &spec).await?
            {
                transaction.commit().await?;
                return Ok(existing);
            }
        }
        let record = AutomationRecord::new(spec);
        upsert_automation_record_in_tx(&mut transaction, &record).await?;
        transaction.commit().await?;
        self.append_event(
            &record.spec.scope.guild_id,
            &record.spec.scope.voice_channel_id,
            json!({
                "event_kind": "automation_created",
                "kind": "automation_created",
                "automation_id": record.automation_id,
                "name": record.spec.name,
            }),
        )
        .await?;
        Ok(record)
    }

    pub(crate) async fn save_automation_record(&self, record: &AutomationRecord) -> Result<()> {
        self.upsert_automation_record(record).await
    }

    pub async fn get_automation(&self, automation_id: &str) -> Result<AutomationRecord> {
        let row = sqlx::query("SELECT payload_blob FROM automations WHERE automation_id = $1")
            .bind(automation_id)
            .fetch_one(&self.pool)
            .await?;
        let payload: Vec<u8> = row.try_get("payload_blob")?;
        AutomationRecord::decode(&payload)
    }

    pub async fn list_automations(
        &self,
        guild_id: Option<&str>,
        voice_channel_id: Option<&str>,
        state: Option<AutomationState>,
    ) -> Result<Vec<AutomationRecord>> {
        let records = self
            .automation_rows()
            .await?
            .into_iter()
            .filter(|record| {
                guild_id
                    .filter(|value| !value.trim().is_empty())
                    .is_none_or(|value| record.spec.scope.guild_id == value)
            })
            .filter(|record| {
                voice_channel_id
                    .filter(|value| !value.trim().is_empty())
                    .is_none_or(|value| record.spec.scope.voice_channel_id == value)
            })
            .filter(|record| state.is_none_or(|value| record.state == value))
            .collect::<Vec<_>>();
        Ok(records)
    }

    pub async fn cancel_automation(&self, automation_id: &str) -> Result<AutomationRecord> {
        let mut record = self.get_automation(automation_id).await?;
        record.state = AutomationState::Cancelled;
        record.updated_at = isoformat_z(None);
        self.upsert_automation_record(&record).await?;
        self.append_event(
            &record.spec.scope.guild_id,
            &record.spec.scope.voice_channel_id,
            json!({
                "event_kind": "automation_cancelled",
                "kind": "automation_cancelled",
                "automation_id": record.automation_id,
                "name": record.spec.name,
            }),
        )
        .await?;
        Ok(record)
    }

    async fn upsert_automation_record(&self, record: &AutomationRecord) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        upsert_automation_record_in_tx(&mut transaction, record).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn automation_rows(&self) -> Result<Vec<AutomationRecord>> {
        let rows = sqlx::query("SELECT payload_blob FROM automations ORDER BY created_at_ms DESC")
            .fetch_all(&self.pool)
            .await?;
        let mut records = Vec::new();
        for row in rows {
            let payload: Vec<u8> = row.try_get("payload_blob")?;
            records.push(AutomationRecord::decode(&payload)?);
        }
        Ok(records)
    }
}

async fn find_active_automation_by_idempotency_key_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    spec: &AutomationSpec,
) -> Result<Option<AutomationRecord>> {
    if spec.idempotency_key.trim().is_empty() {
        return Ok(None);
    }
    let row = sqlx::query(
        "SELECT payload_blob FROM automations WHERE guild_id = $1 AND voice_channel_id = $2 AND idempotency_key = $3 AND state = 'active' ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&spec.scope.guild_id)
    .bind(&spec.scope.voice_channel_id)
    .bind(&spec.idempotency_key)
    .fetch_optional(transaction.as_mut())
    .await?;
    row.map(|row| -> Result<AutomationRecord> {
        let payload: Vec<u8> = row.try_get("payload_blob")?;
        AutomationRecord::decode(&payload)
    })
    .transpose()
}

async fn lock_agent_automation_source(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_job_id: &str,
) -> Result<()> {
    let row = sqlx::query("SELECT kind FROM jobs WHERE job_id = $1 FOR UPDATE")
        .bind(source_job_id)
        .fetch_optional(transaction.as_mut())
        .await?;
    let Some(row) = row else {
        anyhow::bail!("agent-owned automation source job does not exist: {source_job_id}");
    };
    let kind: String = row.try_get("kind")?;
    if kind != JobKind::AgentTask.as_str() {
        anyhow::bail!(
            "agent-owned automation source job {source_job_id} is {kind}, not agent_task"
        );
    }
    Ok(())
}

async fn find_active_agent_automation_by_source_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    spec: &AutomationSpec,
) -> Result<Option<AutomationRecord>> {
    let Some(source_job_id) = agent_owner_source_job_id(&spec.owner) else {
        return Ok(None);
    };
    let rows = sqlx::query(
        r#"
        SELECT payload_blob
        FROM automations
        WHERE guild_id = $1
          AND voice_channel_id = $2
          AND state = 'active'
        ORDER BY created_at_ms, automation_id
        "#,
    )
    .bind(&spec.scope.guild_id)
    .bind(&spec.scope.voice_channel_id)
    .fetch_all(transaction.as_mut())
    .await?;
    for row in rows {
        let payload: Vec<u8> = row.try_get("payload_blob")?;
        let record = AutomationRecord::decode(&payload)?;
        if agent_owner_source_job_id(&record.spec.owner) == Some(source_job_id) {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

fn agent_owner_source_job_id(owner: &AutomationOwner) -> Option<&str> {
    match owner {
        AutomationOwner::Agent { source_job_id, .. } => Some(source_job_id.as_str()),
        AutomationOwner::User { .. } | AutomationOwner::System => None,
    }
    .filter(|source_job_id| !source_job_id.trim().is_empty())
}

async fn upsert_automation_record_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    record: &AutomationRecord,
) -> Result<()> {
    let created_ms = instant_ms_str(Some(&record.created_at)).unwrap_or(0);
    let updated_ms = instant_ms_str(Some(&record.updated_at)).unwrap_or(created_ms);
    let expires_at_ms = record.spec.expiry.expires_at.as_deref().and_then(|value| {
        parse_instant(value)
            .and_then(|expires_at| instant_ms_str(Some(&isoformat_z(Some(expires_at)))))
    });
    sqlx::query(
        r#"
            INSERT INTO automations(
              automation_id,
              guild_id,
              voice_channel_id,
              state,
              idempotency_key,
              created_at_ms,
              updated_at_ms,
              expires_at_ms,
              fire_count,
              max_fires,
              payload_blob
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT(automation_id) DO UPDATE SET
              guild_id = EXCLUDED.guild_id,
              voice_channel_id = EXCLUDED.voice_channel_id,
              state = EXCLUDED.state,
              idempotency_key = EXCLUDED.idempotency_key,
              updated_at_ms = EXCLUDED.updated_at_ms,
              expires_at_ms = EXCLUDED.expires_at_ms,
              fire_count = EXCLUDED.fire_count,
              max_fires = EXCLUDED.max_fires,
              payload_blob = EXCLUDED.payload_blob
            "#,
    )
    .bind(&record.automation_id)
    .bind(&record.spec.scope.guild_id)
    .bind(&record.spec.scope.voice_channel_id)
    .bind(record.state.as_str())
    .bind(&record.spec.idempotency_key)
    .bind(created_ms)
    .bind(updated_ms)
    .bind(expires_at_ms)
    .bind(record.fire_count as i64)
    .bind(record.spec.expiry.max_fires.map(|value| value as i64))
    .bind(record.encode()?)
    .execute(transaction.as_mut())
    .await?;
    Ok(())
}

impl Runtime {
    pub async fn load_automation_registry(&mut self) -> Result<()> {
        self.automations = self
            .timeline_store
            .list_automations(None, None, Some(AutomationState::Active))
            .await?
            .into_iter()
            .map(|record| (record.automation_id.clone(), record))
            .collect();
        Ok(())
    }

    pub fn validate_automation_from_value(&self, value: &Value) -> Result<Value> {
        let spec = AutomationSpec::from_json(value)?;
        Ok(json!({"valid": true, "automation": spec.to_json()}))
    }

    pub async fn create_automation_from_value(&mut self, value: &Value) -> Result<Value> {
        let spec = AutomationSpec::from_json(value)?;
        let record = self.timeline_store.create_automation(spec).await?;
        if record.state == AutomationState::Active {
            self.automations
                .insert(record.automation_id.clone(), record.clone());
        }
        Ok(json!({"created": true, "automation": record.to_json()}))
    }

    pub async fn list_automation_records(
        &self,
        guild_id: Option<&str>,
        voice_channel_id: Option<&str>,
        state: Option<AutomationState>,
    ) -> Result<Value> {
        let records = self
            .timeline_store
            .list_automations(guild_id, voice_channel_id, state)
            .await?;
        Ok(json!({
            "automations": records.iter().map(AutomationRecord::to_json).collect::<Vec<_>>(),
        }))
    }

    pub async fn get_automation_record(&self, automation_id: &str) -> Result<Value> {
        Ok(self
            .timeline_store
            .get_automation(automation_id)
            .await?
            .to_json())
    }

    pub async fn cancel_automation_record(&mut self, automation_id: &str) -> Result<Value> {
        let record = self.timeline_store.cancel_automation(automation_id).await?;
        self.automations.remove(automation_id);
        Ok(record.to_json())
    }
}

fn default_schema() -> String {
    "clankcord.automation.v0".to_string()
}

fn validate_boundary_automation_value(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("automation spec must be a JSON object"))?;
    if let Some(scope) = object.get("scope") {
        validate_scope_boundary(scope)?;
    }
    if let Some(owner) = object.get("owner") {
        validate_kind(
            "$.owner.kind",
            owner,
            &["agent", "user", "system"],
            "automation owner",
        )?;
    }
    if let Some(trigger) = object.get("trigger") {
        validate_trigger_boundary(trigger)?;
    }
    if let Some(condition) = object.get("condition") {
        validate_condition_boundary("$.condition", condition)?;
    }
    if let Some(expiry) = object.get("expiry") {
        validate_expiry_boundary(expiry)?;
    }
    if let Some(actions) = object.get("actions") {
        let Some(actions) = actions.as_array() else {
            anyhow::bail!("$.actions must be an array");
        };
        for (index, action) in actions.iter().enumerate() {
            validate_action_boundary(index, action)?;
        }
    }
    Ok(())
}

fn validate_scope_boundary(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("$.scope must be an object"))?;
    if !has_any_key(object, &["guild_id", "guildId"]) {
        if object.contains_key("guild") {
            anyhow::bail!("$.scope requires guild_id; use guild_id instead of guild");
        }
        anyhow::bail!("$.scope requires guild_id");
    }
    if !has_any_key(
        object,
        &[
            "voice_channel_id",
            "voiceChannelId",
            "channel_id",
            "channelId",
        ],
    ) {
        if object.contains_key("channel") {
            anyhow::bail!(
                "$.scope requires voice_channel_id; use voice_channel_id instead of channel"
            );
        }
        anyhow::bail!("$.scope requires voice_channel_id");
    }
    Ok(())
}

fn validate_trigger_boundary(value: &Value) -> Result<()> {
    let kind = validate_kind(
        "$.trigger.kind",
        value,
        &["tick", "event", "job", "room_state_changed"],
        "automation trigger",
    )?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("$.trigger must be an object"))?;
    match kind.as_str() {
        "tick" => {
            if object
                .get("interval_seconds")
                .or_else(|| object.get("intervalSeconds"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                == 0
            {
                anyhow::bail!("$.trigger.interval_seconds must be a positive integer");
            }
        }
        "event" => {
            if object.contains_key("event_kind") || object.contains_key("eventKind") {
                anyhow::bail!(
                    "$.trigger requires event_kinds as a non-empty array, not event_kind"
                );
            }
            require_non_empty_string_array(
                object
                    .get("event_kinds")
                    .or_else(|| object.get("eventKinds")),
                "$.trigger.event_kinds",
            )?;
        }
        "job" => {
            if object.contains_key("job_kind") || object.contains_key("jobKind") {
                anyhow::bail!("$.trigger requires job_kinds as a non-empty array, not job_kind");
            }
            if object.contains_key("state") {
                anyhow::bail!("$.trigger requires states as a non-empty array, not state");
            }
            let job_kinds = require_non_empty_string_array(
                object.get("job_kinds").or_else(|| object.get("jobKinds")),
                "$.trigger.job_kinds",
            )?;
            for kind in job_kinds {
                kind.parse::<JobKind>().with_context(|| {
                    format!("$.trigger.job_kinds contains unknown job kind `{kind}`")
                })?;
            }
            let states = require_non_empty_string_array(object.get("states"), "$.trigger.states")?;
            for state in states {
                state.parse::<JobState>().with_context(|| {
                    format!("$.trigger.states contains unknown job state `{state}`")
                })?;
            }
        }
        "room_state_changed" => {}
        _ => unreachable!(),
    }
    Ok(())
}

fn validate_condition_boundary(path: &str, value: &Value) -> Result<()> {
    let kind = validate_kind(
        &format!("{path}.kind"),
        value,
        &["true", "all", "any", "not", "predicate"],
        "automation condition",
    )?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{path} must be an object"))?;
    match kind.as_str() {
        "true" => Ok(()),
        "all" | "any" => {
            let Some(conditions) = object.get("conditions") else {
                anyhow::bail!("{path}.conditions must be a non-empty array");
            };
            let Some(conditions) = conditions.as_array() else {
                anyhow::bail!("{path}.conditions must be an array");
            };
            if conditions.is_empty() {
                anyhow::bail!("{path}.conditions must be a non-empty array");
            }
            for (index, condition) in conditions.iter().enumerate() {
                validate_condition_boundary(&format!("{path}.conditions[{index}]"), condition)?;
            }
            Ok(())
        }
        "not" => {
            let Some(condition) = object.get("condition") else {
                anyhow::bail!("{path}.condition is required for not conditions");
            };
            validate_condition_boundary(&format!("{path}.condition"), condition)
        }
        "predicate" => validate_predicate_condition_boundary(path, object),
        _ => unreachable!(),
    }
}

fn validate_predicate_condition_boundary(
    path: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<()> {
    if object
        .get("path")
        .and_then(Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
    {
        anyhow::bail!("{path}.path is required for predicate conditions");
    }
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{path}.op is required for predicate conditions"))?;
    let requires_value = match op {
        "eq" | "ne" | "gt" | "gte" | "lt" | "lte" | "contains" | "matches" => true,
        "present" | "empty" => false,
        _ => anyhow::bail!(
            "{path}.op `{op}` is not supported; expected one of eq, ne, gt, gte, lt, lte, contains, present, empty, matches"
        ),
    };
    if requires_value && !object.contains_key("value") {
        anyhow::bail!("{path}.value is required for predicate op `{op}`");
    }
    if let Some(value) = object.get("value") {
        validate_scalar_boundary(&format!("{path}.value"), value)?;
    }
    Ok(())
}

fn validate_scalar_boundary(path: &str, value: &Value) -> Result<()> {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => Ok(()),
        Value::Object(object) => {
            let kind = object
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("{path} object scalar requires kind"))?;
            if !matches!(kind, "string" | "number" | "bool") {
                anyhow::bail!(
                    "{path}.kind `{kind}` is not supported; expected string, number, or bool"
                );
            }
            if !object.contains_key("value") {
                anyhow::bail!("{path}.value is required for tagged scalar values");
            }
            Ok(())
        }
        _ => anyhow::bail!("{path} must be a string, number, bool, or tagged scalar object"),
    }
}

fn validate_expiry_boundary(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("$.expiry must be an object"))?;
    if object
        .get("max_fires")
        .or_else(|| object.get("maxFires"))
        .and_then(Value::as_u64)
        == Some(0)
    {
        anyhow::bail!("$.expiry.max_fires must be greater than 0");
    }
    if let Some(expires_at) = object
        .get("expires_at")
        .or_else(|| object.get("expiresAt"))
        .and_then(Value::as_str)
    {
        if parse_instant(expires_at).is_none() {
            anyhow::bail!("$.expiry.expires_at must be an RFC3339 timestamp");
        }
    }
    Ok(())
}

fn validate_action_boundary(index: usize, value: &Value) -> Result<()> {
    let path = format!("$.actions[{index}]");
    let kind = validate_kind(
        &format!("{path}.kind"),
        value,
        &[
            "response.send",
            "response_send",
            "agent_task.start",
            "agent_task_start",
            "sound.play",
            "sound_play",
            "transcript.start_live",
            "transcript_start_live",
        ],
        "automation action",
    )?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{path} must be an object"))?;
    match kind.as_str() {
        "response.send" | "response_send" => {
            if object
                .get("content")
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                anyhow::bail!("{path}.content is required for response.send");
            }
            let Some(sink) = object.get("sink") else {
                anyhow::bail!("{path}.sink is required for response.send");
            };
            validate_response_sink_boundary(&format!("{path}.sink"), sink)
        }
        "agent_task.start" | "agent_task_start" => {
            if object
                .get("prompt")
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                anyhow::bail!("{path}.prompt is required for agent_task.start");
            }
            if let Some(sink) = object
                .get("response_sink")
                .or_else(|| object.get("responseSink"))
            {
                validate_response_sink_boundary(&format!("{path}.response_sink"), sink)?;
            }
            Ok(())
        }
        "sound.play" | "sound_play" => {
            if object
                .get("name")
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                anyhow::bail!("{path}.name is required for sound.play");
            }
            Ok(())
        }
        "transcript.start_live" | "transcript_start_live" => Ok(()),
        _ => unreachable!(),
    }
}

fn validate_response_sink_boundary(path: &str, value: &Value) -> Result<()> {
    let kind = validate_kind(
        &format!("{path}.kind"),
        value,
        &["agent_chat", "channel", "dm"],
        "response sink",
    )?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{path} must be an object"))?;
    if matches!(kind.as_str(), "channel" | "dm")
        && !has_any_key(
            object,
            &["id", "channel_id", "channelId", "user_id", "userId"],
        )
    {
        anyhow::bail!("{path}.id is required for channel and dm sinks");
    }
    Ok(())
}

fn validate_kind(path: &str, value: &Value, allowed: &[&str], label: &str) -> Result<String> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{path} parent must be an object"))?;
    let Some(kind) = object.get("kind").and_then(Value::as_str) else {
        anyhow::bail!("{path} is required");
    };
    if !allowed.contains(&kind) {
        anyhow::bail!(
            "{path} `{kind}` is not a supported {label}; expected one of {}",
            allowed.join(", ")
        );
    }
    Ok(kind.to_string())
}

fn require_non_empty_string_array<'a>(
    value: Option<&'a Value>,
    path: &str,
) -> Result<Vec<&'a str>> {
    let Some(value) = value else {
        anyhow::bail!("{path} must be a non-empty array");
    };
    let Some(values) = value.as_array() else {
        anyhow::bail!("{path} must be an array");
    };
    if values.is_empty() {
        anyhow::bail!("{path} must be a non-empty array");
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("{path}[{index}] must be a non-empty string"))
        })
        .collect()
}

fn has_any_key(object: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| object.contains_key(*key))
}

fn normalize_boundary_automation_value(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        if let Some(owner) = object.get("owner") {
            object.insert("owner".to_string(), normalize_owner(owner));
        }
        if let Some(trigger) = object.get("trigger") {
            object.insert("trigger".to_string(), normalize_trigger(trigger));
        }
        if let Some(condition) = object.get("condition") {
            object.insert("condition".to_string(), normalize_condition(condition));
        }
        if let Some(actions) = object.get("actions").and_then(Value::as_array) {
            object.insert(
                "actions".to_string(),
                Value::Array(actions.iter().map(normalize_action).collect()),
            );
        }
    }
    value
}

fn normalize_owner(value: &Value) -> Value {
    let kind = value_kind(value);
    let payload = object_without_kind(value);
    match kind.as_str() {
        "agent" => json!({"Agent": payload}),
        "user" => json!({"User": payload}),
        "system" | "" => json!("System"),
        _ => value.clone(),
    }
}

fn normalize_trigger(value: &Value) -> Value {
    let kind = value_kind(value);
    let payload = object_without_kind(value);
    match kind.as_str() {
        "tick" => json!({"Tick": payload}),
        "event" => json!({"Event": payload}),
        "job" => json!({"Job": payload}),
        "room_state_changed" => json!("RoomStateChanged"),
        _ => value.clone(),
    }
}

fn normalize_condition(value: &Value) -> Value {
    let kind = value_kind(value);
    let mut payload = object_without_kind(value);
    match kind.as_str() {
        "true" | "" => json!("True"),
        "all" => {
            let conditions = payload
                .remove("conditions")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .map(normalize_condition)
                .collect::<Vec<_>>();
            json!({"All": {"conditions": conditions}})
        }
        "any" => {
            let conditions = payload
                .remove("conditions")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .map(normalize_condition)
                .collect::<Vec<_>>();
            json!({"Any": {"conditions": conditions}})
        }
        "not" => {
            let condition = payload
                .remove("condition")
                .map(|condition| normalize_condition(&condition))
                .unwrap_or_else(|| json!("True"));
            json!({"Not": {"condition": condition}})
        }
        "predicate" => {
            if let Some(value) = payload.remove("value") {
                payload.insert("value".to_string(), normalize_scalar(&value));
            }
            json!({"Predicate": payload})
        }
        _ => value.clone(),
    }
}

fn normalize_action(value: &Value) -> Value {
    let kind = value_kind(value);
    let payload = object_without_kind(value);
    match kind.as_str() {
        "response.send" | "response_send" => json!({"ResponseSend": payload}),
        "agent_task.start" | "agent_task_start" => json!({"AgentTaskStart": payload}),
        "sound.play" | "sound_play" => json!({"SoundPlay": payload}),
        "transcript.start_live" | "transcript_start_live" => {
            json!({"TranscriptStartLive": payload})
        }
        _ => value.clone(),
    }
}

fn normalize_scalar(value: &Value) -> Value {
    if let Some(kind) = value.get("kind").and_then(Value::as_str) {
        let raw = value.get("value").cloned().unwrap_or(Value::Null);
        return match kind {
            "string" => json!({"String": raw}),
            "number" => json!({"Number": raw}),
            "bool" => json!({"Bool": raw}),
            _ => value.clone(),
        };
    }
    match value {
        Value::String(raw) => json!({"String": raw}),
        Value::Number(raw) => raw
            .as_f64()
            .map(|number| json!({"Number": number}))
            .unwrap_or(Value::Null),
        Value::Bool(raw) => json!({"Bool": raw}),
        _ => value.clone(),
    }
}

fn value_kind(value: &Value) -> String {
    value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn object_without_kind(value: &Value) -> serde_json::Map<String, Value> {
    value
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter(|(key, _)| key.as_str() != "kind")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn deserialize_job_kinds<'de, D>(deserializer: D) -> std::result::Result<Vec<JobKind>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|value| value.parse::<JobKind>().map_err(serde::de::Error::custom))
        .collect()
}

fn serialize_job_kinds<S>(
    values: &Vec<JobKind>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    values
        .iter()
        .map(|value| value.as_str().to_string())
        .collect::<Vec<_>>()
        .serialize(serializer)
}

fn deserialize_job_states<'de, D>(deserializer: D) -> std::result::Result<Vec<JobState>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|value| value.parse::<JobState>().map_err(serde::de::Error::custom))
        .collect()
}

fn serialize_job_states<S>(
    values: &Vec<JobState>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    values
        .iter()
        .map(|value| value.as_str().to_string())
        .collect::<Vec<_>>()
        .serialize(serializer)
}
