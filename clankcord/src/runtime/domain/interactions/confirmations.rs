use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::Result;
use crate::config::string_field;
use crate::errors::discord_tool_error;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::domain::interactions::requires_confirmation;
use crate::runtime::timeline::isoformat_z;
use crate::runtime::{
    BinaryPayload, CommandRequest, ConfirmationContext, DiscordTextSendPayload, Job, JobKind,
    JobOutput, JobState, TextDeliveryKind, TextTarget, TextTargetKind,
};

use crate::runtime::Runtime;
use crate::runtime::util::{first_non_empty, preview};

impl Runtime {
    pub async fn confirmation_context_for_command(
        &self,
        command: &CommandRequest,
    ) -> Result<ConfirmationContext> {
        let (start, end) = command.window_times(None);
        let guild_id = command.guild_id.clone();
        let channel_id = command.voice_channel_id.clone();
        let events = if guild_id.is_empty() || channel_id.is_empty() {
            Vec::new()
        } else {
            let kinds = BTreeSet::from(["speech_segment".to_string(), "transcript".to_string()]);
            self.timeline_store
                .load_events(
                    &guild_id,
                    &channel_id,
                    Some(start - chrono::Duration::minutes(1)),
                    Some(end + chrono::Duration::minutes(1)),
                    Some(&kinds),
                    None,
                    false,
                )
                .await
                .unwrap_or_default()
        };
        let mut source_lines = Vec::new();
        for event in events
            .iter()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let speaker = first_non_empty([
                string_field(event, "speaker_label"),
                string_field(event, "speakerLabel"),
                string_field(event, "speaker_user_id"),
                "unknown".to_string(),
            ]);
            let text = first_non_empty([
                string_field(event, "text_draft"),
                string_field(event, "text"),
            ]);
            if !text.is_empty() {
                source_lines.push(format!("- {speaker}: {}", preview(&text, 220)));
            }
        }
        let sensitive = requires_confirmation(command.command_kind.as_str());
        Ok(ConfirmationContext {
            sensitive,
            delivery: if sensitive { "dm" } else { "channel" }.to_string(),
            target_window_start: isoformat_z(Some(start)),
            target_window_end: isoformat_z(Some(end)),
            target_window_duration_seconds: (end - start).num_seconds().max(0),
            source_preview: source_lines,
            created_at: isoformat_z(None),
        })
    }

    pub(crate) async fn prepare_confirmation_required_job(&self, job: &Job) -> Result<JobDecision> {
        let command = job
            .command()
            .cloned()
            .ok_or_else(|| discord_tool_error("confirmation job has no command payload"))?;
        let sensitive = requires_confirmation(command.command_kind.as_str());
        let delivery = if sensitive { "dm" } else { "channel" };
        let requested_user_id = first_non_empty([
            command.requested_by_user_id.clone(),
            job.requested_by_user_id.clone(),
        ]);
        let target = if sensitive {
            if requested_user_id.is_empty() {
                let mut failed = job.clone();
                let confirmation = failed.metadata.confirmation_mut();
                confirmation.delivery = delivery.to_string();
                confirmation.post_error =
                    "sensitive confirmation is missing requester user id".to_string();
                let error = confirmation.post_error.clone();
                self.timeline_store.update_job(&failed).await?;
                return Ok(JobDecision::fail(error));
            }
            TextTarget {
                kind: TextTargetKind::Dm,
                channel_id: String::new(),
                user_id: requested_user_id.clone(),
            }
        } else {
            TextTarget {
                kind: TextTargetKind::Channel,
                channel_id: self.control_config.bots_channel_id.clone(),
                user_id: String::new(),
            }
        };
        if target.kind == TextTargetKind::Channel && target.channel_id.trim().is_empty() {
            let mut failed = job.clone();
            let confirmation = failed.metadata.confirmation_mut();
            confirmation.delivery = delivery.to_string();
            confirmation.post_error = "botsChannelId is not configured".to_string();
            let error = confirmation.post_error.clone();
            self.timeline_store.update_job(&failed).await?;
            return Ok(JobDecision::fail(error));
        }

        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children
            .iter()
            .find(|child| child.state != JobState::Complete)
        {
            let mut latest = self.timeline_store.get_job(&job.id).await?;
            let confirmation = latest.metadata.confirmation_mut();
            confirmation.delivery = delivery.to_string();
            confirmation.post_error = format!(
                "confirmation post dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            );
            let error = confirmation.post_error.clone();
            self.timeline_store.update_job(&latest).await?;
            return Ok(JobDecision::fail(error));
        }
        if let Some(child) = children
            .iter()
            .find(|child| child.kind == JobKind::DiscordTextSend)
        {
            let Some(JobOutput::DiscordTextSend(output)) = child.metadata.output.clone() else {
                return Ok(JobDecision::fail(format!(
                    "confirmation post child {} completed without text send output",
                    child.id
                )));
            };
            let mut latest = self.timeline_store.get_job(&job.id).await?;
            let (channel_id, message_id) = {
                let confirmation = latest.metadata.confirmation_mut();
                confirmation.delivery = delivery.to_string();
                confirmation.channel_id = output.discord_post.channel_id;
                confirmation.message_id = output
                    .discord_post
                    .messages
                    .first()
                    .map(|message| message.message_id.clone())
                    .unwrap_or_default();
                (
                    confirmation.channel_id.clone(),
                    confirmation.message_id.clone(),
                )
            };
            latest.set_state(JobState::ConfirmationPending);
            self.timeline_store.update_job(&latest).await?;
            return Ok(JobDecision::Complete(JobOutput::from_boundary_json(
                &json!({
                    "kind": "confirmation_posted",
                    "job_id": latest.id,
                    "delivery": delivery,
                    "channel_id": channel_id,
                    "message_id": message_id,
                }),
            )?));
        }

        Ok(JobDecision::WaitFor(vec![Job::discord_text_send(
            job.guild_id.clone(),
            job.voice_channel_id.clone(),
            requested_user_id.clone(),
            DiscordTextSendPayload {
                intent: TextDeliveryKind::Message,
                target,
                content: self.confirmation_card_content(job, &command),
                source_job_id: job.id.clone(),
                requested_by_user_id: String::new(),
                allowed_mentions: BinaryPayload::from_json(&json!({"parse": []}))?,
                components: BinaryPayload::from_json(&json!([{
                    "type": 1,
                    "components": [
                        {
                            "type": 2,
                            "style": 3,
                            "label": "Approve",
                            "custom_id": format!("clankcord_voice_confirm:{}", job.id),
                        },
                        {
                            "type": 2,
                            "style": 4,
                            "label": "Cancel",
                            "custom_id": format!("clankcord_voice_cancel:{}", job.id),
                        },
                    ],
                }]))?,
            },
        )]))
    }

    pub fn confirmation_card_content(&self, job: &Job, command: &CommandRequest) -> String {
        let (start, end) = command.window_times(None);
        let room = self.room_for_channel_ids(&command.guild_id, &command.voice_channel_id, None);
        let requester = first_non_empty([
            command.requested_by_speaker_label.clone(),
            command.requested_by_user_id.clone(),
            "unknown".to_string(),
        ]);
        let mut lines = vec![
            "Clanky voice confirmation required.".to_string(),
            String::new(),
            format!("Command: {}", command.command_kind.as_str()),
            format!("Room: {}", room.channel_name),
            format!("Requested by: {requester}"),
            format!(
                "Target window: {} to {}",
                isoformat_z(Some(start)),
                isoformat_z(Some(end))
            ),
            format!("Job: {}", job.id),
        ];
        if requires_confirmation(command.command_kind.as_str()) {
            lines.push(
                "Effect: forget local draft speech events and source audio in that room for the target window."
                    .to_string(),
            );
        }
        if let Some(context) = job.confirmation_context()
            && !context.source_preview.is_empty()
        {
            lines.push(String::new());
            lines.push("Source context:".to_string());
            lines.extend(context.source_preview.iter().take(6).cloned());
        }
        preview(&lines.join("\n"), 1900)
    }
    pub async fn approve_confirmation(
        &mut self,
        job_id: &str,
        actor_user_id: String,
    ) -> Result<Value> {
        let mut job = self.timeline_store.get_job(job_id).await?;
        if job.kind != JobKind::ConfirmationRequired {
            return Err(discord_tool_error(format!(
                "job {job_id} is not a pending confirmation"
            )));
        }
        if !matches!(job.state, JobState::Queued | JobState::ConfirmationPending) {
            return Err(discord_tool_error(format!(
                "job {job_id} confirmation is already {}",
                job.state
            )));
        }
        require_confirmation_actor(&job, &actor_user_id)?;
        let mut command = job
            .command()
            .cloned()
            .ok_or_else(|| discord_tool_error(format!("job {job_id} has no command payload")))?;
        command.clear_confirmation_requirement(actor_user_id.clone());
        job.set_state(JobState::Approved);
        {
            let confirmation = job.metadata.confirmation_mut();
            confirmation.approved_by_user_id = actor_user_id;
            confirmation.approved_at = isoformat_z(None);
        }
        self.timeline_store.update_job(&job).await?;
        let dispatch_result = match self.create_command_job(command.clone(), Some(&job)).await {
            Ok(result) => result,
            Err(error) => {
                let mut latest = match self.timeline_store.get_job(job_id).await {
                    Ok(latest) => latest,
                    Err(_) => job.clone(),
                };
                latest.set_state(JobState::ApprovalFailed);
                latest.metadata.confirmation_mut().approval_error = error.to_string();
                self.timeline_store.update_job(&latest).await?;
                return Err(error);
            }
        };
        let mut latest = match self.timeline_store.get_job(job_id).await {
            Ok(latest) => latest,
            Err(_) => job,
        };
        if latest.state != JobState::Waiting {
            latest.mark_complete();
            self.timeline_store.update_job(&latest).await?;
        }
        let children = self
            .timeline_store
            .list_child_jobs(job_id)
            .await?
            .into_iter()
            .map(|job| job.to_value())
            .collect::<Vec<_>>();
        Ok(json!({
            "job": latest.to_value(),
            "children": children,
            "dispatched_command": command.to_json(),
            "dispatch_result": dispatch_result
        }))
    }
    pub async fn cancel_confirmation(&self, job_id: &str, actor_user_id: String) -> Result<Value> {
        let mut job = self.timeline_store.get_job(job_id).await?;
        if job.kind != JobKind::ConfirmationRequired {
            return Err(discord_tool_error(format!(
                "job {job_id} is not a pending confirmation"
            )));
        }
        if !matches!(job.state, JobState::Queued | JobState::ConfirmationPending) {
            return Err(discord_tool_error(format!(
                "job {job_id} confirmation is already {}",
                job.state
            )));
        }
        require_confirmation_actor(&job, &actor_user_id)?;
        job.mark_cancelled();
        job.metadata.cancelled_by_user_id = actor_user_id;
        self.timeline_store.update_job(&job).await?;
        Ok(job.to_value())
    }
}

fn require_confirmation_actor(job: &Job, actor_user_id: &str) -> Result<()> {
    let expected = job.requested_by_user_id.trim();
    if !expected.is_empty() && actor_user_id.trim() != expected {
        return Err(discord_tool_error(
            "only the requesting user can approve or cancel this confirmation",
        ));
    }
    Ok(())
}
