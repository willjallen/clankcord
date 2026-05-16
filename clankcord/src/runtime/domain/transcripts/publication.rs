use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::timeline::isoformat_z;
use crate::runtime::util::{
    MESSAGE_CHUNK_LIMIT, first_non_empty, preview, split_message_chunks, string_field,
};
use crate::runtime::{
    BinaryPayload, DiscordForumThreadCreatePayload, DiscordTextSendPayload, Job, JobKind,
    JobOutput, JobState, RoomConfig, Runtime, TextDeliveryKind, TextTarget, TextTargetKind,
    TranscriptPublicationOutput, TranscriptPublicationPayload,
};

const DISCORD_THREAD_NAME_LIMIT: usize = 100;

impl Runtime {
    pub(crate) async fn publish_materialized_transcript(
        &self,
        result: &mut Value,
        live: bool,
        refined_queued: bool,
    ) -> Result<()> {
        let mut publication = result
            .get("publication")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !publication.is_object() {
            return Ok(());
        }
        let guild_id = string_field(&publication, "guild_id");
        let channel_id = string_field(&publication, "voice_channel_id");
        let created_by_user_id = string_field(&publication, "created_by_user_id");
        let job = Job::transcript_publication(
            guild_id.clone(),
            channel_id.clone(),
            created_by_user_id,
            TranscriptPublicationPayload {
                publication_id: string_field(&publication, "publication_id"),
                live,
                refined_queued,
            },
        );
        let job = if let Some(parent_job_id) = string_field(&publication, "parent_job_id")
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
        {
            let parent = self.timeline_store.get_job(parent_job_id).await?;
            self.timeline_store.create_child_job(&parent, job).await?
        } else {
            self.timeline_store.create_job(job).await?
        };
        update_object_fields(
            &mut publication,
            [
                ("publish_job_id", json!(job.id.clone())),
                ("state", json!("discord_publish_queued")),
                ("updated_at", json!(isoformat_z(None))),
            ],
        )?;
        self.timeline_store.update_publication(&publication).await?;
        if let Some(map) = result.as_object_mut() {
            map.insert("publication".to_string(), publication);
            map.insert("publish_job".to_string(), job.to_value());
        }
        Ok(())
    }

    pub(crate) async fn prepare_transcript_publication_job(
        &mut self,
        job: &Job,
        payload: &TranscriptPublicationPayload,
    ) -> Result<JobDecision> {
        let mut publication = self
            .timeline_store
            .get_publication(&payload.publication_id)
            .await?;
        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children
            .iter()
            .find(|child| child.state != JobState::Complete)
        {
            let message = format!(
                "publication dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            );
            update_object_fields(
                &mut publication,
                [
                    ("discord_publish_error", json!(message.clone())),
                    ("updated_at", json!(isoformat_z(None))),
                ],
            )?;
            self.timeline_store.update_publication(&publication).await?;
            return Ok(JobDecision::fail(message));
        }

        let thread_job = children
            .iter()
            .find(|child| child.kind == JobKind::DiscordForumThreadCreate);
        if let Some(thread_job) = thread_job {
            let Some(JobOutput::DiscordForumThreadCreate(thread_output)) =
                thread_job.metadata.output.clone()
            else {
                return Ok(JobDecision::fail(format!(
                    "publication thread child {} completed without thread output",
                    thread_job.id
                )));
            };
            let text_children = children
                .iter()
                .filter(|child| child.kind == JobKind::DiscordTextSend)
                .collect::<Vec<_>>();
            if text_children.is_empty() {
                let draft_path = PathBuf::from(string_field(&publication, "draft_artifact_path"));
                let content = fs::read_to_string(&draft_path).unwrap_or_default();
                let chunks = split_message_chunks(&content, MESSAGE_CHUNK_LIMIT);
                let jobs = chunks
                    .into_iter()
                    .map(|chunk| {
                        Job::discord_text_send(
                            job.guild_id.clone(),
                            job.voice_channel_id.clone(),
                            job.requested_by_user_id.clone(),
                            DiscordTextSendPayload {
                                intent: TextDeliveryKind::Message,
                                target: TextTarget {
                                    kind: TextTargetKind::Channel,
                                    channel_id: thread_output.thread_id.clone(),
                                    user_id: String::new(),
                                },
                                content: chunk,
                                source_job_id: job.id.clone(),
                                requested_by_user_id: String::new(),
                                allowed_mentions: BinaryPayload::empty(),
                                components: BinaryPayload::empty(),
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                if jobs.is_empty() {
                    return self
                        .complete_transcript_publication(
                            job,
                            payload,
                            &mut publication,
                            &thread_output.thread_id,
                            Vec::new(),
                        )
                        .await;
                }
                return Ok(JobDecision::WaitFor(jobs));
            }
            let mut message_ids = Vec::new();
            for child in text_children {
                let Some(JobOutput::DiscordTextSend(output)) = child.metadata.output.clone() else {
                    return Ok(JobDecision::fail(format!(
                        "publication message child {} completed without text output",
                        child.id
                    )));
                };
                message_ids.extend(
                    output
                        .discord_post
                        .messages
                        .into_iter()
                        .map(|message| message.message_id),
                );
            }
            return self
                .complete_transcript_publication(
                    job,
                    payload,
                    &mut publication,
                    &thread_output.thread_id,
                    message_ids,
                )
                .await;
        }

        let (_room, name, header, auto_archive) = self
            .publication_thread_request(&publication, payload)
            .await?;
        let forum_id = self.control_config.transcripts_forum_id.clone();
        if forum_id.trim().is_empty() {
            anyhow::bail!("transcriptsForumId is not configured");
        }
        Ok(JobDecision::WaitFor(vec![
            Job::discord_forum_thread_create(
                job.guild_id.clone(),
                job.voice_channel_id.clone(),
                job.requested_by_user_id.clone(),
                DiscordForumThreadCreatePayload {
                    parent_channel_id: forum_id,
                    name,
                    content: header,
                    auto_archive_minutes: auto_archive,
                    source_job_id: job.id.clone(),
                },
            ),
        ]))
    }

    async fn complete_transcript_publication(
        &self,
        _job: &Job,
        payload: &TranscriptPublicationPayload,
        publication: &mut Value,
        thread_id: &str,
        message_ids: Vec<String>,
    ) -> Result<JobDecision> {
        let state = if payload.live {
            "live_draft_published"
        } else {
            "draft_published"
        };
        update_object_fields(
            publication,
            [
                ("discord_thread_id", json!(thread_id)),
                ("discord_message_ids", json!(message_ids.clone())),
                ("state", json!(state)),
                ("updated_at", json!(isoformat_z(None))),
            ],
        )?;
        self.timeline_store.update_publication(publication).await?;
        Ok(JobDecision::Complete(JobOutput::TranscriptPublication(
            TranscriptPublicationOutput {
                publication_id: payload.publication_id.clone(),
                state: state.to_string(),
                discord_thread_id: thread_id.to_string(),
                discord_message_ids: message_ids,
            },
        )))
    }

    async fn publication_thread_request(
        &self,
        publication: &Value,
        payload: &TranscriptPublicationPayload,
    ) -> Result<(RoomConfig, String, String, i64)> {
        let guild_id = string_field(publication, "guild_id");
        let channel_id = string_field(publication, "voice_channel_id");
        let room = self.room_for_channel_ids(&guild_id, &channel_id, None);
        let window = self
            .timeline_store
            .get_window(&string_field(publication, "window_id"))
            .await?;
        let start = string_field(&window, "start_time");
        let end = string_field(&window, "end_time");
        let name_base = first_non_empty([
            room.channel_name.clone(),
            room.channel_slug.clone(),
            room.channel_id.clone(),
        ]);
        let name = preview(
            &format!(
                "{} transcript {}",
                name_base,
                start.chars().take(16).collect::<String>()
            ),
            DISCORD_THREAD_NAME_LIMIT,
        );
        let mut header_lines = vec![
            if payload.live {
                "# Draft Live Transcript".to_string()
            } else {
                "# Draft Transcript".to_string()
            },
            String::new(),
            format!("- Source: {}", room.channel_name),
            format!("- Window: {start} to {end}"),
            "- Status: draft local STT".to_string(),
        ];
        if payload.refined_queued {
            header_lines.push("- High-quality refinement queued.".to_string());
        }
        header_lines.push(String::new());
        header_lines.push(
            "This transcript may contain local STT errors until refinement completes.".to_string(),
        );
        let auto_archive = {
            let configured = self.control_config.thread_auto_archive_minutes;
            if configured > 0 { configured } else { 1440 }
        };
        Ok((room, name, header_lines.join("\n"), auto_archive))
    }
}

fn update_object_fields<const N: usize>(
    value: &mut Value,
    fields: [(&str, Value); N],
) -> Result<()> {
    let Some(map) = value.as_object_mut() else {
        anyhow::bail!("payload is not an object");
    };
    for (key, field_value) in fields {
        map.insert(key.to_string(), field_value);
    }
    Ok(())
}
