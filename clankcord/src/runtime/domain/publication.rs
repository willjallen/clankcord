use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Result;
use crate::adapters::discord::api::{
    delete_message, discord_request, edit_message, maybe_unarchive_thread, send_message,
};
use crate::config::{MESSAGE_CHUNK_LIMIT, split_message_chunks, string_field};
use crate::runtime::timeline::isoformat_z;

use crate::runtime::util::{first_non_empty, preview, update_object_fields};
use crate::runtime::{RoomConfig, Runtime};

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
        let room = self.room_for_channel_ids(&guild_id, &channel_id, None);
        let draft_path = PathBuf::from(string_field(&publication, "draft_artifact_path"));
        let content = fs::read_to_string(&draft_path).unwrap_or_default();
        match self
            .create_publication_thread(&room, &publication, &content, live, refined_queued)
            .await
        {
            Ok((thread_id, message_ids)) => {
                update_object_fields(
                    &mut publication,
                    [
                        ("discord_thread_id", json!(thread_id)),
                        ("discord_message_ids", json!(message_ids)),
                        (
                            "state",
                            json!(if live {
                                "live_draft_published"
                            } else {
                                "draft_published"
                            }),
                        ),
                        ("updated_at", json!(isoformat_z(None))),
                    ],
                )?;
            }
            Err(error) => {
                update_object_fields(
                    &mut publication,
                    [
                        ("discord_publish_error", json!(error.to_string())),
                        ("updated_at", json!(isoformat_z(None))),
                    ],
                )?;
            }
        }
        self.timeline_store.update_publication(&publication).await?;
        if let Some(map) = result.as_object_mut() {
            map.insert("publication".to_string(), publication);
        }
        Ok(())
    }

    pub async fn create_publication_thread(
        &self,
        room: &RoomConfig,
        publication: &Value,
        content: &str,
        live: bool,
        refined_queued: bool,
    ) -> Result<(String, Vec<String>)> {
        let forum_id = self.control_config.transcripts_forum_id.clone();
        if forum_id.is_empty() {
            anyhow::bail!("transcriptsForumId is not configured");
        }
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
            if live {
                "# Draft Live Transcript".to_string()
            } else {
                "# Draft Transcript".to_string()
            },
            String::new(),
            format!("- Source: {}", room.channel_name),
            format!("- Window: {start} to {end}"),
            "- Status: draft local STT".to_string(),
        ];
        if refined_queued {
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
        let body = json!({
            "name": name,
            "auto_archive_duration": auto_archive,
            "message": {"content": header_lines.join("\n")},
        });
        let payload = discord_request(
            "POST",
            &format!("/channels/{forum_id}/threads"),
            Some(&body),
            None,
            None,
            30,
        )?;
        let thread_id = string_field(&payload, "id");
        if thread_id.is_empty() {
            anyhow::bail!("failed to create publication thread for {}", room.room_id);
        }
        let chunks = split_message_chunks(content, MESSAGE_CHUNK_LIMIT);
        let message_ids = self.sync_message_chunks(&thread_id, &chunks, &[])?;
        Ok((thread_id, message_ids))
    }

    pub fn sync_message_chunks(
        &self,
        channel_id: &str,
        chunks: &[String],
        existing_ids: &[String],
    ) -> Result<Vec<String>> {
        maybe_unarchive_thread(channel_id)?;
        let mut message_ids = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            if let Some(existing_id) = existing_ids.get(index).filter(|id| !id.trim().is_empty()) {
                edit_message(channel_id, existing_id, chunk)?;
                message_ids.push(existing_id.clone());
            } else {
                let payload = send_message(channel_id, chunk)?;
                let message_id = string_field(&payload, "id");
                if !message_id.is_empty() {
                    message_ids.push(message_id);
                }
            }
        }
        for stale_id in existing_ids.iter().skip(chunks.len()) {
            if !stale_id.trim().is_empty() {
                let _ = delete_message(channel_id, stale_id);
            }
        }
        Ok(message_ids)
    }
}
