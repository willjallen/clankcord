use serde_json::{Value, json};

use crate::Result;
use crate::config::string_field;
use crate::runtime::core::execution::RuntimeEffects;
use crate::runtime::domain::interactions::requires_confirmation;
use crate::runtime::timeline::{isoformat_z, utc_now};
use crate::runtime::{
    ForgetRequest, Job, JobKind, MaterializeTranscriptRequest, RouterCommand, RouterCommandKind,
};

use crate::runtime::Runtime;

impl Runtime {
    pub async fn create_router_command_job(
        &mut self,
        command: RouterCommand,
        parent_job: Option<&Job>,
    ) -> Result<Value> {
        self.create_router_command_job_sync(command, parent_job)
    }

    pub(crate) fn create_router_command_job_sync(
        &self,
        mut command: RouterCommand,
        parent_job: Option<&Job>,
    ) -> Result<Value> {
        let (guild_id, channel_id, _) = self.router_command_scope(&command)?;
        command.guild_id = guild_id.clone();
        command.voice_channel_id = channel_id.clone();
        if requires_confirmation(command.command_kind.as_str())
            && command.approved_by_user_id.trim().is_empty()
        {
            command.requires_confirmation = true;
        }
        if command.requires_confirmation {
            let confirmation_context = self.confirmation_context_for_command(&command)?;
            let job = Job::confirmation_required(
                &guild_id,
                &channel_id,
                command.requested_by_user_id.clone(),
                command,
                confirmation_context,
            );
            let mut job = if let Some(parent_job) = parent_job {
                self.timeline_store.create_child_job(parent_job, job)?
            } else {
                self.timeline_store.create_job(job)?
            };
            self.post_confirmation_card(&mut job)?;
            return Ok(json!({
                "kind": "confirmation_required",
                "job_ids": [job.id.clone()],
                "job": job.to_value()
            }));
        }

        let job = Job::router_command(
            &guild_id,
            &channel_id,
            command.requested_by_user_id.clone(),
            command,
        );
        let job = if let Some(parent_job) = parent_job {
            self.timeline_store.create_child_job(parent_job, job)?
        } else {
            self.timeline_store.create_job(job)?
        };
        Ok(json!({
            "kind": "router_command_created",
            "job_ids": [job.id.clone()],
            "job": job.to_value()
        }))
    }

    pub(crate) async fn execute_router_command_job(
        &mut self,
        job: &Job,
        effects: Option<&dyn RuntimeEffects>,
    ) -> Result<Value> {
        if job.kind != JobKind::RouterCommand {
            anyhow::bail!("job {} is not a router command", job.id);
        }
        let command = job.command().cloned().ok_or_else(|| {
            anyhow::anyhow!("router command job {} has no command payload", job.id)
        })?;
        self.execute_router_command(command, job, effects).await
    }

    async fn execute_router_command(
        &mut self,
        command: RouterCommand,
        parent_job: &Job,
        effects: Option<&dyn RuntimeEffects>,
    ) -> Result<Value> {
        let command_kind = command.command_kind;
        let job_kind = command_kind.job_kind();
        let (guild_id, channel_id, target_room_identifier) = self.router_command_scope(&command)?;
        match job_kind {
            "materialize_transcript" => {
                let (start, end) = command.window_times(None);
                let materialized = self.materialize_transcript(MaterializeTranscriptRequest {
                    guild_id: guild_id.clone(),
                    channel_id: channel_id.clone(),
                    from: if command.arguments.from.trim().is_empty() {
                        isoformat_z(Some(start))
                    } else {
                        command.arguments.from.clone()
                    },
                    to: if command.arguments.to.trim().is_empty() {
                        isoformat_z(Some(end))
                    } else {
                        command.arguments.to.clone()
                    },
                    publish: if command.arguments.publish.trim().is_empty() {
                        "discord".to_string()
                    } else {
                        command.arguments.publish.clone()
                    },
                    live: command_kind == RouterCommandKind::StartLiveTranscript,
                    refine: command.arguments.refine.unwrap_or(false),
                    created_by_user_id: command.requested_by_user_id.clone(),
                    parent_job_id: parent_job.id.clone(),
                    ..MaterializeTranscriptRequest::default()
                })?;
                Ok(
                    json!({"kind": "materialize_transcript", "job_ids": [], "materialized": materialized}),
                )
            }
            "make_permanent" => {
                let end = utc_now();
                let start = end - chrono::Duration::minutes(30);
                let materialized = self.materialize_transcript(MaterializeTranscriptRequest {
                    guild_id: guild_id.clone(),
                    channel_id: channel_id.clone(),
                    from: isoformat_z(Some(start)),
                    to: isoformat_z(Some(end)),
                    publish: "discord".to_string(),
                    refine: true,
                    created_by_user_id: command.requested_by_user_id.clone(),
                    parent_job_id: parent_job.id.clone(),
                    ..MaterializeTranscriptRequest::default()
                })?;
                Ok(json!({"kind": "make_permanent", "job_ids": [], "materialized": materialized}))
            }
            "pause_listening" => {
                let room = self.room_for_identifier(Some(&target_room_identifier))?;
                self.pause_room(
                    &room,
                    command.arguments.duration_seconds.unwrap_or(20 * 60),
                    &command.requested_by_user_id,
                )
                .await?;
                Ok(json!({"kind": "pause_listening", "job_ids": []}))
            }
            "resume_listening" => {
                let room = self.room_for_identifier(Some(&target_room_identifier))?;
                self.resume_room(&room, &command.requested_by_user_id)
                    .await?;
                Ok(json!({"kind": "resume_listening", "job_ids": []}))
            }
            "deafen_listening" => {
                let room = self.room_for_identifier(Some(&target_room_identifier))?;
                self.pause_room(
                    &room,
                    self.manual_leave_cooldown_seconds,
                    &command.requested_by_user_id,
                )
                .await?;
                Ok(json!({"kind": "deafen_listening", "job_ids": []}))
            }
            "leave_room" => {
                let cooldown_seconds = self.manual_leave_cooldown_seconds;
                let result = if let Some(effects) = effects {
                    self.leave_room_with_effect(
                        Some(&target_room_identifier),
                        cooldown_seconds,
                        &command.requested_by_user_id,
                        Some(parent_job),
                        effects,
                    )
                    .await?
                } else {
                    self.leave_room(
                        Some(&target_room_identifier),
                        Some(cooldown_seconds),
                        Some(&command.requested_by_user_id),
                    )
                    .await?
                };
                Ok(json!({"kind": "leave_room", "job_ids": [], "result": result}))
            }
            "join_room" => {
                let result = if let Some(effects) = effects {
                    let room = if !target_room_identifier.trim().is_empty() {
                        self.room_for_identifier(Some(&target_room_identifier))?
                    } else if !guild_id.trim().is_empty() {
                        self.resolve_room_scope(&guild_id, None)?
                    } else {
                        self.room_for_identifier(None)?
                    };
                    self.assign_room_with_effect(
                        room,
                        &command.requested_by_user_id,
                        "explicit_request",
                        effects,
                    )
                    .await?
                } else {
                    self.assign_room(
                        Some(&target_room_identifier),
                        Some(&guild_id),
                        Some(&command.requested_by_user_id),
                        Some("explicit_request"),
                    )
                    .await?
                };
                Ok(json!({"kind": "join_room", "job_ids": [], "result": result}))
            }
            "forget_window" => {
                let (start, end) = command.window_times(None);
                let result = self.forget(ForgetRequest {
                    window_id: command.arguments.window_id.clone(),
                    guild_id: guild_id.clone(),
                    channel_id: channel_id.clone(),
                    since: if command.arguments.from.trim().is_empty() {
                        isoformat_z(Some(start))
                    } else {
                        command.arguments.from.clone()
                    },
                    to: if command.arguments.to.trim().is_empty() {
                        isoformat_z(Some(end))
                    } else {
                        command.arguments.to.clone()
                    },
                    requested_by_user_id: command.requested_by_user_id.clone(),
                    unpublished_only: command.arguments.unpublished_only.unwrap_or(true),
                    ..ForgetRequest::default()
                })?;
                Ok(json!({"kind": "forget_window", "job_ids": [], "result": result}))
            }
            _ => {
                let job_kind: JobKind = job_kind.parse()?;
                if !job_kind.is_agent_task() {
                    anyhow::bail!("unsupported queued job kind: {job_kind}");
                }
                let job = Job::agent_task(
                    &guild_id,
                    &channel_id,
                    command.requested_by_user_id.clone(),
                    command,
                );
                let job = self.timeline_store.create_child_job(parent_job, job)?;
                Ok(json!({
                    "kind": "job_created",
                    "job_ids": [job.id.clone()],
                    "job": job.to_value()
                }))
            }
        }
    }

    fn router_command_scope(&self, command: &RouterCommand) -> Result<(String, String, String)> {
        let mut guild_id = command.guild_id.trim().to_string();
        let mut channel_id = command.voice_channel_id.trim().to_string();
        let target_room_identifier = command.target_room_identifier(&channel_id);
        if (guild_id.is_empty() || channel_id.is_empty()) && !command.arguments.window_id.is_empty()
        {
            let window = self
                .timeline_store
                .get_window(&command.arguments.window_id)?;
            if guild_id.is_empty() {
                guild_id = string_field(&window, "guild_id");
            }
            if channel_id.is_empty() {
                channel_id = string_field(&window, "voice_channel_id");
            }
        }
        if guild_id.is_empty() || channel_id.is_empty() {
            let room = self.resolve_room_scope(&guild_id, Some(&target_room_identifier))?;
            if guild_id.is_empty() {
                guild_id = room.guild_id;
            }
            channel_id = room.channel_id;
        }
        if guild_id.is_empty() || channel_id.is_empty() {
            anyhow::bail!("router command is missing guild or channel");
        }
        Ok((guild_id, channel_id, target_room_identifier))
    }
}
