use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Result;
use crate::runtime::jobs::{
    AgentSessionResumePayload, AgentSessionRetirementPayload, AgentSessionStartPayload,
    AgentSessionSunsetPayload, AgentTaskPayload, AgentThreadTitleRefreshPayload,
    AudioSegmentPayload, AutomationEvaluationPayload, BinaryPayload, CommandPayload,
    ConfirmationRequiredPayload, DiscordForumThreadCreatePayload, DiscordForumThreadRenamePayload,
    DiscordSlashCommandPayload, DiscordTextMessagePayload, DiscordTextSendPayload,
    DiscordTypingIndicatorPayload, DiscordVoiceDeafenPayload, DiscordVoiceJoinPayload,
    DiscordVoiceLeavePayload, DiscordVoiceMutePayload, DiscordVoicePlayAudioPayload,
    DiscordVoicePlaybackPayload, DiscordVoiceStatusSnapshotPayload, EphemeralJobGcPayload,
    RefineTranscriptPayload, RoomAgentPlacementPayload, RuntimeControlPayload,
    RuntimeMaintenancePayload, StaleRunningJobSweepPayload, StaleWakeProbeSweepPayload,
    TextDeliveryPayload, TranscriptPublicationPayload, VoiceStatusSyncPayload,
    WakeActivationPayload, WakeProbePayload,
};
use crate::runtime::{JobPayload, TextDeliveryKind, TextTarget};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PreV0_7_0TextDeliveryPayload {
    intent: TextDeliveryKind,
    target: TextTarget,
    content: String,
    source_job_id: String,
    requested_by_user_id: String,
    expects_reply: bool,
    opaque: BinaryPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PreV0_7_0DiscordTextSendPayload {
    intent: TextDeliveryKind,
    target: TextTarget,
    content: String,
    source_job_id: String,
    requested_by_user_id: String,
    allowed_mentions: BinaryPayload,
    components: BinaryPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum PreV0_7_0JobPayload {
    AudioSegment(AudioSegmentPayload),
    WakeActivation(WakeActivationPayload),
    AgentTask(AgentTaskPayload),
    DiscordTextMessage(DiscordTextMessagePayload),
    DiscordSlashCommand(DiscordSlashCommandPayload),
    TextDelivery(PreV0_7_0TextDeliveryPayload),
    DiscordTextSend(PreV0_7_0DiscordTextSendPayload),
    DiscordForumThreadCreate(DiscordForumThreadCreatePayload),
    DiscordForumThreadRename(DiscordForumThreadRenamePayload),
    AgentSessionStart(AgentSessionStartPayload),
    AgentSessionSunset(AgentSessionSunsetPayload),
    AgentSessionResume(AgentSessionResumePayload),
    AgentSessionRetirement(AgentSessionRetirementPayload),
    AgentThreadTitleRefresh(AgentThreadTitleRefreshPayload),
    TranscriptPublication(TranscriptPublicationPayload),
    RefineTranscript(RefineTranscriptPayload),
    ConfirmationRequired(ConfirmationRequiredPayload),
    Command(CommandPayload),
    RoomAgentPlacement(RoomAgentPlacementPayload),
    DiscordVoiceJoin(DiscordVoiceJoinPayload),
    DiscordVoiceLeave(DiscordVoiceLeavePayload),
    DiscordVoicePlayback(DiscordVoicePlaybackPayload),
    DiscordVoiceMute(DiscordVoiceMutePayload),
    DiscordVoicePlayAudio(DiscordVoicePlayAudioPayload),
    RuntimeControl(RuntimeControlPayload),
    WakeProbe(WakeProbePayload),
    RuntimeMaintenance(RuntimeMaintenancePayload),
    VoiceStatusSync(VoiceStatusSyncPayload),
    DiscordVoiceStatusSnapshot(DiscordVoiceStatusSnapshotPayload),
    AutomationEvaluation(AutomationEvaluationPayload),
    StaleWakeProbeSweep(StaleWakeProbeSweepPayload),
    StaleRunningJobSweep(StaleRunningJobSweepPayload),
    EphemeralJobGc(EphemeralJobGcPayload),
    DiscordVoiceDeafen(DiscordVoiceDeafenPayload),
    DiscordTypingIndicator(DiscordTypingIndicatorPayload),
}

impl PreV0_7_0JobPayload {
    pub(super) fn into_current(self) -> Result<JobPayload> {
        Ok(match self {
            Self::AudioSegment(payload) => JobPayload::AudioSegment(payload),
            Self::WakeActivation(payload) => JobPayload::WakeActivation(payload),
            Self::AgentTask(payload) => JobPayload::AgentTask(payload),
            Self::DiscordTextMessage(payload) => JobPayload::DiscordTextMessage(payload),
            Self::DiscordSlashCommand(payload) => JobPayload::DiscordSlashCommand(payload),
            Self::TextDelivery(payload) => JobPayload::TextDelivery(payload.into_current()?),
            Self::DiscordTextSend(payload) => JobPayload::DiscordTextSend(payload.into_current()),
            Self::DiscordForumThreadCreate(payload) => {
                JobPayload::DiscordForumThreadCreate(payload)
            }
            Self::DiscordForumThreadRename(payload) => {
                JobPayload::DiscordForumThreadRename(payload)
            }
            Self::AgentSessionStart(payload) => JobPayload::AgentSessionStart(payload),
            Self::AgentSessionSunset(payload) => JobPayload::AgentSessionSunset(payload),
            Self::AgentSessionResume(payload) => JobPayload::AgentSessionResume(payload),
            Self::AgentSessionRetirement(payload) => JobPayload::AgentSessionRetirement(payload),
            Self::AgentThreadTitleRefresh(payload) => JobPayload::AgentThreadTitleRefresh(payload),
            Self::TranscriptPublication(payload) => JobPayload::TranscriptPublication(payload),
            Self::RefineTranscript(payload) => JobPayload::RefineTranscript(payload),
            Self::ConfirmationRequired(payload) => JobPayload::ConfirmationRequired(payload),
            Self::Command(payload) => JobPayload::Command(payload),
            Self::RoomAgentPlacement(payload) => JobPayload::RoomAgentPlacement(payload),
            Self::DiscordVoiceJoin(payload) => JobPayload::DiscordVoiceJoin(payload),
            Self::DiscordVoiceLeave(payload) => JobPayload::DiscordVoiceLeave(payload),
            Self::DiscordVoicePlayback(payload) => JobPayload::DiscordVoicePlayback(payload),
            Self::DiscordVoiceMute(payload) => JobPayload::DiscordVoiceMute(payload),
            Self::DiscordVoicePlayAudio(payload) => JobPayload::DiscordVoicePlayAudio(payload),
            Self::RuntimeControl(payload) => JobPayload::RuntimeControl(payload),
            Self::WakeProbe(payload) => JobPayload::WakeProbe(payload),
            Self::RuntimeMaintenance(payload) => JobPayload::RuntimeMaintenance(payload),
            Self::VoiceStatusSync(payload) => JobPayload::VoiceStatusSync(payload),
            Self::DiscordVoiceStatusSnapshot(payload) => {
                JobPayload::DiscordVoiceStatusSnapshot(payload)
            }
            Self::AutomationEvaluation(payload) => JobPayload::AutomationEvaluation(payload),
            Self::StaleWakeProbeSweep(payload) => JobPayload::StaleWakeProbeSweep(payload),
            Self::StaleRunningJobSweep(payload) => JobPayload::StaleRunningJobSweep(payload),
            Self::EphemeralJobGc(payload) => JobPayload::EphemeralJobGc(payload),
            Self::DiscordVoiceDeafen(payload) => JobPayload::DiscordVoiceDeafen(payload),
            Self::DiscordTypingIndicator(payload) => JobPayload::DiscordTypingIndicator(payload),
        })
    }
}

impl PreV0_7_0TextDeliveryPayload {
    fn into_current(self) -> Result<TextDeliveryPayload> {
        let mut map = self
            .opaque
            .to_json()
            .as_object()
            .cloned()
            .unwrap_or_else(Map::new);
        map.insert(
            "intent".to_string(),
            Value::String(self.intent.as_str().to_string()),
        );
        map.insert("target".to_string(), self.target.to_json());
        map.insert("content".to_string(), Value::String(self.content));
        map.insert(
            "source_job_id".to_string(),
            Value::String(self.source_job_id),
        );
        map.insert(
            "requested_by_user_id".to_string(),
            Value::String(self.requested_by_user_id),
        );
        if self.expects_reply {
            map.insert("expects_reply".to_string(), Value::Bool(true));
        } else {
            map.remove("expects_reply");
        }
        map.remove("attachments");
        TextDeliveryPayload::from_json(&Value::Object(map))
    }
}

impl PreV0_7_0DiscordTextSendPayload {
    fn into_current(self) -> DiscordTextSendPayload {
        DiscordTextSendPayload {
            intent: self.intent,
            target: self.target,
            content: self.content,
            source_job_id: self.source_job_id,
            requested_by_user_id: self.requested_by_user_id,
            allowed_mentions: self.allowed_mentions,
            components: self.components,
            attachments: Vec::new(),
        }
    }
}
