use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::api::discord_request;
use crate::config::string_field;
use crate::errors::discord_tool_error;
use crate::runtime::domain::interactions::requires_confirmation;
use crate::runtime::timeline::isoformat_z;
use crate::runtime::{CommandRequest, ConfirmationContext, Job, JobKind, JobState};

use crate::runtime::Runtime;
use crate::runtime::util::{first_non_empty, preview, require_confirmation_actor};

impl Runtime {
    pub fn confirmation_context_for_command(
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

    pub fn post_confirmation_card(&self, job: &mut Job) -> Result<()> {
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
        let channel_id = if sensitive {
            if requested_user_id.is_empty() {
                let confirmation = job.metadata.confirmation_mut();
                confirmation.delivery = delivery.to_string();
                confirmation.post_error =
                    "sensitive confirmation is missing requester user id".to_string();
                self.timeline_store.update_job(job)?;
                return Ok(());
            }
            let dm = discord_request(
                "POST",
                "/users/@me/channels",
                Some(&json!({"recipient_id": requested_user_id})),
                None,
                None,
                30,
            );
            match dm {
                Ok(payload) => string_field(&payload, "id"),
                Err(error) => {
                    let confirmation = job.metadata.confirmation_mut();
                    confirmation.delivery = delivery.to_string();
                    confirmation.post_error = error.to_string();
                    self.timeline_store.update_job(job)?;
                    return Ok(());
                }
            }
        } else {
            self.control_config.bots_channel_id.clone()
        };
        if channel_id.is_empty() {
            let confirmation = job.metadata.confirmation_mut();
            confirmation.delivery = delivery.to_string();
            confirmation.post_error = "botsChannelId is not configured".to_string();
            self.timeline_store.update_job(job)?;
            return Ok(());
        }
        let content = self.confirmation_card_content(job, &command);
        let body = json!({
            "content": content,
            "allowed_mentions": {"parse": []},
            "components": [{
                "type": 1,
                "components": [
                    {
                        "type": 2,
                        "style": 3,
                        "label": "Approve",
                        "custom_id": format!("clawcord_voice_confirm:{}", job.id),
                    },
                    {
                        "type": 2,
                        "style": 4,
                        "label": "Cancel",
                        "custom_id": format!("clawcord_voice_cancel:{}", job.id),
                    },
                ],
            }],
        });
        match discord_request(
            "POST",
            &format!("/channels/{channel_id}/messages"),
            Some(&body),
            None,
            None,
            30,
        ) {
            Ok(response) => {
                let confirmation = job.metadata.confirmation_mut();
                confirmation.delivery = delivery.to_string();
                confirmation.channel_id = channel_id;
                confirmation.message_id = string_field(&response, "id");
            }
            Err(error) => {
                let confirmation = job.metadata.confirmation_mut();
                confirmation.delivery = delivery.to_string();
                confirmation.channel_id = channel_id;
                confirmation.post_error = error.to_string();
            }
        }
        self.timeline_store.update_job(job)?;
        Ok(())
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
        let mut job = self.timeline_store.get_job(job_id)?;
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
        self.timeline_store.update_job(&job)?;
        let dispatch_result = match self.create_command_job(command.clone(), Some(&job)).await {
            Ok(result) => result,
            Err(error) => {
                let mut latest = self.timeline_store.get_job(job_id).unwrap_or(job.clone());
                latest.set_state(JobState::ApprovalFailed);
                latest.metadata.confirmation_mut().approval_error = error.to_string();
                self.timeline_store.update_job(&latest)?;
                return Err(error);
            }
        };
        let mut latest = self.timeline_store.get_job(job_id).unwrap_or(job);
        if latest.state != JobState::Waiting {
            latest.mark_complete();
            self.timeline_store.update_job(&latest)?;
        }
        let children = self
            .timeline_store
            .list_child_jobs(job_id)?
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
    pub fn cancel_confirmation(&self, job_id: &str, actor_user_id: String) -> Result<Value> {
        let mut job = self.timeline_store.get_job(job_id)?;
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
        self.timeline_store.update_job(&job)?;
        Ok(job.to_value())
    }
}
