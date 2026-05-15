use crate::Result;
use crate::runtime::core::execution::JobDecision;
use crate::runtime::{
    DiscordVoiceMutePayload, DiscordVoicePlayAudioPayload, DiscordVoicePlaybackCue,
    DiscordVoicePlaybackOutput, DiscordVoicePlaybackPayload, Job, JobKind, JobOutput, JobState,
    RoomConfig, Runtime, RuntimeSessionStatus,
};

impl Runtime {
    pub(crate) async fn prepare_voice_playback_job(
        &mut self,
        job: &Job,
        payload: &DiscordVoicePlaybackPayload,
    ) -> Result<JobDecision> {
        let children = self.timeline_store.list_child_jobs(&job.id).await?;
        if children.iter().any(|child| !child.state.is_terminal()) {
            return Ok(JobDecision::Wait);
        }
        if let Some(failed) = children
            .iter()
            .find(|child| child.state != JobState::Complete)
        {
            return Ok(JobDecision::fail(format!(
                "voice playback dependency {} ended as {}: {}",
                failed.id, failed.state, failed.metadata.error
            )));
        }
        if !children
            .iter()
            .any(|child| child.kind == JobKind::DiscordVoiceMute)
        {
            return Ok(JobDecision::WaitFor(vec![Job::discord_voice_mute(
                job.guild_id.clone(),
                job.voice_channel_id.clone(),
                job.requested_by_user_id.clone(),
                DiscordVoiceMutePayload {
                    session_id: payload.session_id.clone(),
                    muted: false,
                    source_job_id: job.id.clone(),
                    reason: format!("before_{}", payload.reason),
                },
            )]));
        }
        if !children
            .iter()
            .any(|child| child.kind == JobKind::DiscordVoicePlayAudio)
        {
            return Ok(JobDecision::WaitFor(vec![Job::discord_voice_play_audio(
                job.guild_id.clone(),
                job.voice_channel_id.clone(),
                job.requested_by_user_id.clone(),
                DiscordVoicePlayAudioPayload {
                    session_id: payload.session_id.clone(),
                    cue: payload.cue,
                    source_job_id: job.id.clone(),
                    reason: payload.reason.clone(),
                },
            )]));
        }

        let play_child = single_child_of_kind(&children, JobKind::DiscordVoicePlayAudio)?;
        match play_child.metadata.output.clone() {
            Some(JobOutput::DiscordVoicePlayAudio(output)) => Ok(JobDecision::Complete(
                JobOutput::DiscordVoicePlayback(DiscordVoicePlaybackOutput {
                    session_id: output.session_id,
                    cue: output.cue,
                    status: output.status,
                    guild_id: output.guild_id,
                    voice_channel_id: output.voice_channel_id,
                    audio_path: output.audio_path,
                    duration_ms: output.duration_ms,
                    message: output.message,
                }),
            )),
            Some(other) => Ok(JobDecision::fail(format!(
                "play audio child {} completed with wrong output kind: {:?}",
                play_child.id, other
            ))),
            None => Ok(JobDecision::fail(format!(
                "play audio child {} completed without output",
                play_child.id
            ))),
        }
    }

    pub(crate) fn voice_playback_job_for_session(
        &self,
        session: &RuntimeSessionStatus,
        requested_by_user_id: &str,
        cue: DiscordVoicePlaybackCue,
        reason: &str,
        source_job_id: &str,
    ) -> Job {
        Job::discord_voice_playback(
            session.guild_id.clone(),
            session.voice_channel_id.clone(),
            requested_by_user_id.to_string(),
            DiscordVoicePlaybackPayload {
                session_id: session.session_id.clone(),
                cue,
                source_job_id: source_job_id.to_string(),
                reason: reason.to_string(),
            },
        )
    }

    pub(crate) async fn create_voice_playback_job_for_channel(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
        requested_by_user_id: &str,
        cue: DiscordVoicePlaybackCue,
        reason: &str,
        source_job_id: &str,
    ) -> Result<Option<Job>> {
        let Some(session) = self.active_session_for_channel(guild_id, voice_channel_id) else {
            return Ok(None);
        };
        let job = self.voice_playback_job_for_session(
            &session,
            requested_by_user_id,
            cue,
            reason,
            source_job_id,
        );
        Ok(Some(self.timeline_store.create_job(job).await?))
    }

    pub(crate) async fn create_voice_playback_job_for_room(
        &self,
        room: &RoomConfig,
        requested_by_user_id: &str,
        cue: DiscordVoicePlaybackCue,
        reason: &str,
        source_job_id: &str,
    ) -> Result<Option<Job>> {
        self.create_voice_playback_job_for_channel(
            &room.guild_id,
            &room.channel_id,
            requested_by_user_id,
            cue,
            reason,
            source_job_id,
        )
        .await
    }

    pub(crate) fn active_session_for_channel(
        &self,
        guild_id: &str,
        voice_channel_id: &str,
    ) -> Option<RuntimeSessionStatus> {
        self.sessions.values().find_map(|session| {
            (session.guild_id == guild_id
                && session.voice_channel_id == voice_channel_id
                && session.ended_at.trim().is_empty())
            .then_some(session.clone())
        })
    }
}

fn single_child_of_kind(children: &[Job], kind: JobKind) -> Result<&Job> {
    let matches = children
        .iter()
        .filter(|child| child.kind == kind)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        anyhow::bail!("expected exactly one {kind} child, found {}", matches.len());
    }
    Ok(matches[0])
}
