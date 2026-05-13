use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};
use uuid::Uuid;

use crate::Result;
use crate::errors::discord_tool_error;

use super::util::{
    first_non_empty, insert_i64_if_nonzero, insert_non_empty, insert_optional_string,
};
use super::{
    AudioSegmentPayload, BinaryPayload, ConfirmationContext, ConfirmationRequiredPayload, JobKind,
    JobPayload, JobState, RefineTranscriptPayload, RoomAgentPlacementAction,
    RoomAgentPlacementPayload, RouterCommand, RouterCommandPayload, RuntimeControlAction,
    RuntimeControlPayload, VoiceAgentTaskPayload,
};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct WorkerPreflightCheck {
    pub command: String,
    pub returncode: Option<i32>,
    pub ok: bool,
    pub stdout_preview: String,
    pub stderr_preview: String,
    pub error: String,
}

impl WorkerPreflightCheck {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "command": self.command,
            "returncode": self.returncode,
            "ok": self.ok,
            "stdout_preview": self.stdout_preview,
            "stderr_preview": self.stderr_preview,
            "error": self.error,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct WorkerPreflightMetadata {
    pub ok: bool,
    pub checked_at: String,
    pub checks: Vec<WorkerPreflightCheck>,
}

impl WorkerPreflightMetadata {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "ok": self.ok,
            "checked_at": self.checked_at,
            "checks": self.checks.iter().map(WorkerPreflightCheck::to_json).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn failed_check_summary(&self) -> String {
        let summary = self
            .checks
            .iter()
            .filter(|check| !check.ok)
            .map(|check| {
                first_non_empty([
                    check.command.clone(),
                    check.error.clone(),
                    "unknown check".to_string(),
                ])
            })
            .collect::<Vec<_>>()
            .join("; ");
        if summary.is_empty() {
            "unknown check".to_string()
        } else {
            summary
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct WorkerAgentMetadata {
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub usage: BinaryPayload,
}

impl WorkerAgentMetadata {
    pub(crate) fn to_json(&self) -> Value {
        let mut object = Map::new();
        insert_non_empty(&mut object, "session_id", &self.session_id);
        insert_non_empty(&mut object, "provider", &self.provider);
        insert_non_empty(&mut object, "model", &self.model);
        if !self.usage.is_empty() {
            object.insert("usage".to_string(), self.usage.to_json());
        }
        Value::Object(object)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct DiscordPostedMessageMetadata {
    pub channel_id: String,
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct DiscordPostMetadata {
    pub channel_id: String,
    pub messages: Vec<DiscordPostedMessageMetadata>,
}

impl DiscordPostMetadata {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "channel_id": self.channel_id,
            "messages": self.messages.iter().map(|message| {
                json!({
                    "channel_id": message.channel_id,
                    "message_id": message.message_id,
                })
            }).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct WorkerJobMetadata {
    pub dispatch_attempts: i64,
    pub dispatch_error: String,
    pub dispatch_error_after_cancel: String,
    pub packet_path: String,
    pub result_path: String,
    pub dispatch_stdout_preview: String,
    pub dispatch_stderr: String,
    pub agent: WorkerAgentMetadata,
    pub preflight: Option<WorkerPreflightMetadata>,
    pub response_text: String,
    pub command: String,
    pub result_suppressed: bool,
    pub discord_post: Option<DiscordPostMetadata>,
}

impl WorkerJobMetadata {
    pub(crate) fn to_json(&self) -> Value {
        let mut object = Map::new();
        insert_i64_if_nonzero(&mut object, "dispatch_attempts", self.dispatch_attempts);
        insert_non_empty(&mut object, "dispatch_error", &self.dispatch_error);
        insert_non_empty(
            &mut object,
            "dispatch_error_after_cancel",
            &self.dispatch_error_after_cancel,
        );
        insert_non_empty(&mut object, "packet_path", &self.packet_path);
        insert_non_empty(&mut object, "result_path", &self.result_path);
        insert_non_empty(
            &mut object,
            "dispatch_stdout_preview",
            &self.dispatch_stdout_preview,
        );
        insert_non_empty(&mut object, "dispatch_stderr", &self.dispatch_stderr);
        if self.agent != WorkerAgentMetadata::default() {
            object.insert("agent".to_string(), self.agent.to_json());
        }
        if let Some(preflight) = &self.preflight {
            object.insert("preflight".to_string(), preflight.to_json());
        }
        insert_non_empty(&mut object, "response_text", &self.response_text);
        insert_non_empty(&mut object, "command", &self.command);
        if self.result_suppressed {
            object.insert("result_suppressed".to_string(), Value::Bool(true));
        }
        if let Some(post) = &self.discord_post {
            object.insert("discord_post".to_string(), post.to_json());
        }
        Value::Object(object)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct ConfirmationJobMetadata {
    pub delivery: String,
    pub channel_id: String,
    pub message_id: String,
    pub post_error: String,
    pub approved_by_user_id: String,
    pub approved_at: String,
    pub approval_error: String,
}

impl ConfirmationJobMetadata {
    pub(crate) fn to_json(&self) -> Value {
        let mut object = Map::new();
        insert_non_empty(&mut object, "delivery", &self.delivery);
        insert_non_empty(&mut object, "channel_id", &self.channel_id);
        insert_non_empty(&mut object, "message_id", &self.message_id);
        insert_non_empty(&mut object, "post_error", &self.post_error);
        insert_non_empty(
            &mut object,
            "approved_by_user_id",
            &self.approved_by_user_id,
        );
        insert_non_empty(&mut object, "approved_at", &self.approved_at);
        insert_non_empty(&mut object, "approval_error", &self.approval_error);
        Value::Object(object)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum JobMetadataDetail {
    Worker(WorkerJobMetadata),
    Confirmation(ConfirmationJobMetadata),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JobMetadata {
    pub(crate) detail: Option<Box<JobMetadataDetail>>,
    pub error: String,
    pub timed_out_at: String,
    pub cancel_requested: bool,
    pub cancelled_by_user_id: String,
    pub result: BinaryPayload,
}

impl JobMetadata {
    pub(crate) fn worker(&self) -> Option<&WorkerJobMetadata> {
        match self.detail.as_deref() {
            Some(JobMetadataDetail::Worker(worker)) => Some(worker),
            _ => None,
        }
    }

    pub(crate) fn worker_mut(&mut self) -> &mut WorkerJobMetadata {
        if !matches!(self.detail.as_deref(), Some(JobMetadataDetail::Worker(_))) {
            self.detail = Some(Box::new(JobMetadataDetail::Worker(
                WorkerJobMetadata::default(),
            )));
        }
        match self.detail.as_deref_mut() {
            Some(JobMetadataDetail::Worker(worker)) => worker,
            _ => unreachable!("worker metadata detail was just initialized"),
        }
    }

    pub(crate) fn set_worker(&mut self, worker: WorkerJobMetadata) {
        self.detail = Some(Box::new(JobMetadataDetail::Worker(worker)));
    }

    pub(crate) fn reset_worker_retry(&mut self) {
        if let Some(worker) = self.worker_mut_if_present() {
            worker.dispatch_attempts = 0;
            worker.dispatch_error.clear();
        }
    }

    pub(crate) fn confirmation(&self) -> Option<&ConfirmationJobMetadata> {
        match self.detail.as_deref() {
            Some(JobMetadataDetail::Confirmation(confirmation)) => Some(confirmation),
            _ => None,
        }
    }

    pub(crate) fn confirmation_mut(&mut self) -> &mut ConfirmationJobMetadata {
        if !matches!(
            self.detail.as_deref(),
            Some(JobMetadataDetail::Confirmation(_))
        ) {
            self.detail = Some(Box::new(JobMetadataDetail::Confirmation(
                ConfirmationJobMetadata::default(),
            )));
        }
        match self.detail.as_deref_mut() {
            Some(JobMetadataDetail::Confirmation(confirmation)) => confirmation,
            _ => unreachable!("confirmation metadata detail was just initialized"),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        match self.detail.as_deref() {
            Some(JobMetadataDetail::Worker(worker)) => {
                object.insert("worker".to_string(), worker.to_json());
            }
            Some(JobMetadataDetail::Confirmation(confirmation)) => {
                object.insert("confirmation".to_string(), confirmation.to_json());
            }
            None => {}
        }
        insert_non_empty(&mut object, "error", &self.error);
        insert_non_empty(&mut object, "timed_out_at", &self.timed_out_at);
        if self.cancel_requested {
            object.insert("cancel_requested".to_string(), Value::Bool(true));
        }
        insert_non_empty(
            &mut object,
            "cancelled_by_user_id",
            &self.cancelled_by_user_id,
        );
        if !self.result.is_empty() {
            object.insert("result".to_string(), self.result.to_json());
        }
        Value::Object(object)
    }

    fn worker_mut_if_present(&mut self) -> Option<&mut WorkerJobMetadata> {
        match self.detail.as_deref_mut() {
            Some(JobMetadataDetail::Worker(worker)) => Some(worker),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    pub guild_id: String,
    pub voice_channel_id: String,
    pub state: JobState,
    pub requested_by_user_id: String,
    pub payload: JobPayload,
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
    pub next_run_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub parent_job_id: Option<String>,
    pub root_job_id: String,
    pub lineage_depth: u8,
    pub metadata: JobMetadata,
}

impl Job {
    pub fn new(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        state: JobState,
        payload: JobPayload,
    ) -> Self {
        let now = now_string();
        let payload = payload;
        let id = format!("job_{}", Uuid::new_v4().simple());
        Self {
            id: id.clone(),
            kind: payload.kind(),
            guild_id: guild_id.into(),
            voice_channel_id: voice_channel_id.into(),
            state,
            requested_by_user_id: requested_by_user_id.into(),
            payload,
            attempts: 0,
            created_at: now.clone(),
            updated_at: now,
            next_run_at: None,
            started_at: None,
            completed_at: None,
            cancelled_at: None,
            parent_job_id: None,
            root_job_id: id,
            lineage_depth: 0,
            metadata: JobMetadata::default(),
        }
    }

    pub fn attach_to_parent(&mut self, parent: &Job) -> Result<()> {
        if parent.lineage_depth >= 2 {
            return Err(discord_tool_error(
                "job lineage is capped at parent -> child -> grandchild",
            ));
        }
        self.parent_job_id = Some(parent.id.clone());
        self.root_job_id = if parent.root_job_id.trim().is_empty() {
            parent.id.clone()
        } else {
            parent.root_job_id.clone()
        };
        self.lineage_depth = parent.lineage_depth + 1;
        Ok(())
    }

    pub fn voice_agent_task(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        command: RouterCommand,
    ) -> Self {
        Self::new(
            guild_id,
            voice_channel_id,
            requested_by_user_id,
            JobState::Queued,
            JobPayload::VoiceAgentTask(VoiceAgentTaskPayload { command }),
        )
    }

    pub fn audio_segment(payload: AudioSegmentPayload) -> Self {
        Self::new(
            payload.guild_id.clone(),
            payload.voice_channel_id.clone(),
            "discord_voice_adapter",
            JobState::Queued,
            JobPayload::AudioSegment(payload),
        )
    }

    pub fn confirmation_required(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        command: RouterCommand,
        confirmation: ConfirmationContext,
    ) -> Self {
        Self::new(
            guild_id,
            voice_channel_id,
            requested_by_user_id,
            JobState::ConfirmationPending,
            JobPayload::ConfirmationRequired(ConfirmationRequiredPayload {
                command,
                confirmation,
            }),
        )
    }

    pub fn router_command(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        command: RouterCommand,
    ) -> Self {
        Self::new(
            guild_id,
            voice_channel_id,
            requested_by_user_id,
            JobState::Queued,
            JobPayload::RouterCommand(RouterCommandPayload { command }),
        )
    }

    pub fn refine_transcript(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        window_id: impl Into<String>,
        publication_id: impl Into<String>,
    ) -> Self {
        Self::new(
            guild_id,
            voice_channel_id,
            requested_by_user_id,
            JobState::Queued,
            JobPayload::RefineTranscript(RefineTranscriptPayload {
                window_id: window_id.into(),
                publication_id: publication_id.into(),
            }),
        )
    }

    pub fn room_agent_placement(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        room_id: impl Into<String>,
        action: RoomAgentPlacementAction,
        reason: impl Into<String>,
        decision_key: impl Into<String>,
        cooldown_seconds: Option<i64>,
    ) -> Self {
        Self::new(
            guild_id,
            voice_channel_id,
            "runtime_automation",
            JobState::Queued,
            JobPayload::RoomAgentPlacement(RoomAgentPlacementPayload {
                action,
                room_id: room_id.into(),
                reason: reason.into(),
                decision_key: decision_key.into(),
                cooldown_seconds,
            }),
        )
    }

    pub fn runtime_control(
        guild_id: impl Into<String>,
        voice_channel_id: impl Into<String>,
        requested_by_user_id: impl Into<String>,
        action: RuntimeControlAction,
        target_job_id: impl Into<String>,
    ) -> Self {
        let requested_by_user_id = requested_by_user_id.into();
        Self::new(
            guild_id,
            voice_channel_id,
            requested_by_user_id.clone(),
            JobState::Queued,
            JobPayload::RuntimeControl(RuntimeControlPayload {
                action,
                target_job_id: target_job_id.into(),
                actor_user_id: requested_by_user_id,
            }),
        )
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(bincode::serialize(self)?)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(bincode::deserialize(bytes)?)
    }

    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("job_id".to_string(), Value::String(self.id.clone()));
        object.insert(
            "kind".to_string(),
            Value::String(self.kind.as_str().to_string()),
        );
        object.insert("guild_id".to_string(), Value::String(self.guild_id.clone()));
        object.insert(
            "voice_channel_id".to_string(),
            Value::String(self.voice_channel_id.clone()),
        );
        object.insert(
            "state".to_string(),
            Value::String(self.state.as_str().to_string()),
        );
        object.insert(
            "requested_by_user_id".to_string(),
            Value::String(self.requested_by_user_id.clone()),
        );
        object.insert("payload".to_string(), self.payload.to_json());
        object.insert(
            "attempts".to_string(),
            Value::Number(Number::from(self.attempts)),
        );
        object.insert(
            "created_at".to_string(),
            Value::String(self.created_at.clone()),
        );
        object.insert(
            "updated_at".to_string(),
            Value::String(self.updated_at.clone()),
        );
        insert_optional_string(&mut object, "next_run_at", &self.next_run_at);
        insert_optional_string(&mut object, "started_at", &self.started_at);
        insert_optional_string(&mut object, "completed_at", &self.completed_at);
        insert_optional_string(&mut object, "cancelled_at", &self.cancelled_at);
        insert_optional_string(&mut object, "parent_job_id", &self.parent_job_id);
        insert_non_empty(&mut object, "root_job_id", &self.root_job_id);
        object.insert(
            "lineage_depth".to_string(),
            Value::Number(Number::from(self.lineage_depth)),
        );
        let metadata = self.metadata.to_json();
        if metadata
            .as_object()
            .is_some_and(|metadata| !metadata.is_empty())
        {
            object.insert("metadata".to_string(), metadata);
        }
        Value::Object(object)
    }

    pub fn payload_value(&self) -> Value {
        self.payload.to_json()
    }

    pub fn command(&self) -> Option<&RouterCommand> {
        self.payload.command()
    }

    pub fn command_mut(&mut self) -> Option<&mut RouterCommand> {
        self.payload.command_mut()
    }

    pub fn command_value(&self) -> Option<Value> {
        self.command().map(RouterCommand::to_json)
    }

    pub fn command_kind(&self) -> String {
        self.payload.command_kind()
    }

    pub fn confirmation_context(&self) -> Option<&ConfirmationContext> {
        match &self.payload {
            JobPayload::ConfirmationRequired(payload) => Some(&payload.confirmation),
            _ => None,
        }
    }

    pub fn refinement_payload(&self) -> Option<&RefineTranscriptPayload> {
        match &self.payload {
            JobPayload::RefineTranscript(payload) => Some(payload),
            _ => None,
        }
    }

    pub fn audio_segment_payload(&self) -> Option<&AudioSegmentPayload> {
        match &self.payload {
            JobPayload::AudioSegment(payload) => Some(payload),
            _ => None,
        }
    }

    pub fn room_agent_placement_payload(&self) -> Option<&RoomAgentPlacementPayload> {
        match &self.payload {
            JobPayload::RoomAgentPlacement(payload) => Some(payload),
            _ => None,
        }
    }

    pub fn runtime_control_payload(&self) -> Option<&RuntimeControlPayload> {
        match &self.payload {
            JobPayload::RuntimeControl(payload) => Some(payload),
            _ => None,
        }
    }

    pub fn string_field(&self, key: &str) -> String {
        match key {
            "job_id" => self.id.clone(),
            "kind" => self.kind.as_str().to_string(),
            "guild_id" => self.guild_id.clone(),
            "voice_channel_id" => self.voice_channel_id.clone(),
            "state" => self.state.as_str().to_string(),
            "requested_by_user_id" => self.requested_by_user_id.clone(),
            "created_at" => self.created_at.clone(),
            "updated_at" => self.updated_at.clone(),
            "next_run_at" => self.next_run_at.clone().unwrap_or_default(),
            "started_at" => self.started_at.clone().unwrap_or_default(),
            "completed_at" => self.completed_at.clone().unwrap_or_default(),
            "cancelled_at" => self.cancelled_at.clone().unwrap_or_default(),
            "parent_job_id" => self.parent_job_id.clone().unwrap_or_default(),
            "root_job_id" => self.root_job_id.clone(),
            "lineage_depth" => self.lineage_depth.to_string(),
            "worker_dispatch_stdout_preview" | "worker.dispatch_stdout_preview" => self
                .metadata
                .worker()
                .map(|worker| worker.dispatch_stdout_preview.clone())
                .unwrap_or_default(),
            "response_text" | "worker.response_text" => self
                .metadata
                .worker()
                .map(|worker| worker.response_text.clone())
                .unwrap_or_default(),
            "worker_dispatch_error" | "worker.dispatch_error" => self
                .metadata
                .worker()
                .map(|worker| worker.dispatch_error.clone())
                .unwrap_or_default(),
            "error" => self.metadata.error.clone(),
            "confirmation_delivery" | "confirmation.delivery" => self
                .metadata
                .confirmation()
                .map(|confirmation| confirmation.delivery.clone())
                .unwrap_or_default(),
            "confirmation_post_error" | "confirmation.post_error" => self
                .metadata
                .confirmation()
                .map(|confirmation| confirmation.post_error.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    pub fn mark_running(&mut self) {
        self.state = JobState::Running;
        if self
            .started_at
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
        {
            self.started_at = Some(now_string());
        }
    }

    pub fn mark_waiting(&mut self) {
        self.state = JobState::Waiting;
    }

    pub fn mark_complete(&mut self) {
        self.state = JobState::Complete;
        self.completed_at = Some(now_string());
    }

    pub fn mark_cancelled(&mut self) {
        self.state = JobState::Cancelled;
        if self
            .cancelled_at
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
        {
            self.cancelled_at = Some(now_string());
        }
    }

    pub fn mark_cancel_requested(&mut self) {
        self.state = JobState::CancelRequested;
        self.metadata.cancel_requested = true;
    }

    pub fn cancel_requested(&self) -> bool {
        self.metadata.cancel_requested
            || matches!(self.state, JobState::CancelRequested | JobState::Cancelled)
    }

    pub fn set_state(&mut self, state: JobState) {
        self.state = state;
    }

    pub fn touch(&mut self) {
        self.updated_at = now_string();
    }

    pub fn touched(&self) -> Self {
        let mut job = self.clone();
        job.touch();
        job
    }
}

fn now_string() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
