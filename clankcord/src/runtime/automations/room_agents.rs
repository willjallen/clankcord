use crate::Result;
use crate::runtime::automations::{
    Automation, AutomationContext, AutomationOutput, AutomationVoiceState,
};
use crate::runtime::{
    DiscordVoiceLeavePayload, Job, JobKind, RoomAgentPlacementAction, RoomConfig,
    VoiceCaptureSessionStatus,
};

pub(crate) struct RoomAgentPlacementAutomation;

impl Automation for RoomAgentPlacementAutomation {
    fn name(&self) -> &'static str {
        "room_agent_placement"
    }

    fn evaluate(&self, context: &AutomationContext<'_>) -> Result<AutomationOutput> {
        let voice_state = context.voice_state();
        let available_bot = has_available_voice_bot(voice_state);
        let mut output = AutomationOutput::empty();
        for room in context.room_configs() {
            for duplicate in duplicate_voice_bot_sessions_for_room(voice_state, &room) {
                if !has_active_session_leave_job(context, &duplicate) {
                    output.emit(duplicate_session_leave_job(&duplicate));
                }
            }
            let decision = RoomAgentPlacementDecision::evaluate(context, room, available_bot);
            if let Some(action) = decision.action {
                if !has_active_placement_job(context, room, action) {
                    output.emit(placement_job(
                        room,
                        action,
                        decision.reason,
                        decision.cooldown_seconds,
                    ));
                }
            }
        }
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy)]
struct RoomAgentPlacementDecision {
    action: Option<RoomAgentPlacementAction>,
    reason: &'static str,
    cooldown_seconds: Option<i64>,
}

impl RoomAgentPlacementDecision {
    fn evaluate(context: &AutomationContext<'_>, room: &RoomConfig, available_bot: bool) -> Self {
        let voice_bot_present = room_has_voice_bot_presence(context.voice_state(), room);
        let auto_suppressed =
            context.room_control_datetime_active(&room.channel_id, "auto_join_suppressed_until");
        let manual_hold =
            context.room_control_datetime_active(&room.channel_id, "manual_hold_until");
        let listening_paused =
            context.room_control_datetime_active(&room.channel_id, "listening_paused_until");
        let auto_desired = context.pool_config().auto_join_enabled && room.auto_join;
        let should_be_present =
            !auto_suppressed && !listening_paused && (manual_hold || auto_desired);

        if should_be_present && !voice_bot_present && available_bot {
            return Self {
                action: Some(RoomAgentPlacementAction::Join),
                reason: if manual_hold {
                    "manual_hold"
                } else {
                    "auto_join"
                },
                cooldown_seconds: None,
            };
        }

        if voice_bot_present && !should_be_present {
            return Self {
                action: Some(RoomAgentPlacementAction::Leave),
                reason: leave_reason(auto_suppressed, listening_paused),
                cooldown_seconds: Some(context.pool_config().manual_leave_cooldown_seconds),
            };
        }

        Self {
            action: None,
            reason: "no_change",
            cooldown_seconds: None,
        }
    }
}

fn placement_job(
    room: &RoomConfig,
    action: RoomAgentPlacementAction,
    reason: &str,
    cooldown_seconds: Option<i64>,
) -> Job {
    Job::room_agent_placement(
        room.guild_id.clone(),
        room.channel_id.clone(),
        room.room_id.clone(),
        action,
        reason.to_string(),
        format!(
            "room_agent_placement:{}:{}:{}",
            action.as_str(),
            room.guild_id,
            room.channel_id
        ),
        cooldown_seconds,
    )
}

fn leave_reason(auto_suppressed: bool, listening_paused: bool) -> &'static str {
    if listening_paused {
        "listening_paused"
    } else if auto_suppressed {
        "auto_join_suppressed"
    } else {
        "auto_join_not_desired"
    }
}

fn has_available_voice_bot(voice_state: &AutomationVoiceState) -> bool {
    voice_state.bots.iter().any(|bot| {
        bot.ready
            && !voice_state
                .assignments
                .iter()
                .any(|assignment| assignment.is_active() && assignment.voice_bot_id == bot.bot_id)
    })
}

fn duplicate_voice_bot_sessions_for_room(
    voice_state: &AutomationVoiceState,
    room: &RoomConfig,
) -> Vec<VoiceCaptureSessionStatus> {
    active_sessions_for_room(voice_state, room)
        .into_iter()
        .skip(1)
        .collect()
}

fn room_has_voice_bot_presence(voice_state: &AutomationVoiceState, room: &RoomConfig) -> bool {
    voice_state.assignments.iter().any(|assignment| {
        assignment.is_active()
            && assignment.guild_id == room.guild_id
            && assignment.voice_channel_id == room.channel_id
    }) || !active_sessions_for_room(voice_state, room).is_empty()
        || voice_state.bots.iter().any(|status| {
            status.ready
                && status.current_guild_id == room.guild_id
                && status.current_channel_id == room.channel_id
        })
}

fn active_sessions_for_room(
    voice_state: &AutomationVoiceState,
    room: &RoomConfig,
) -> Vec<VoiceCaptureSessionStatus> {
    let mut sessions = voice_state
        .sessions
        .iter()
        .filter(|session| {
            session.active
                && session.ended_at.trim().is_empty()
                && session.guild_id == room.guild_id
                && session.voice_channel_id == room.channel_id
        })
        .cloned()
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    sessions
}

fn has_active_placement_job(
    context: &AutomationContext<'_>,
    room: &RoomConfig,
    action: RoomAgentPlacementAction,
) -> bool {
    context.has_active_job_in_guild(JobKind::RoomAgentPlacement, &room.guild_id, |job| {
        job.room_agent_placement_payload()
            .is_some_and(|payload| payload.action == action && placement_targets_room(job, room))
    }) || context.has_active_job_in_guild(JobKind::DiscordVoiceJoin, &room.guild_id, |job| {
        job.voice_channel_id == room.channel_id
    })
}

fn placement_targets_room(job: &Job, room: &RoomConfig) -> bool {
    room_identifier_matches(&job.voice_channel_id, room)
        || job
            .room_agent_placement_payload()
            .is_some_and(|payload| room_identifier_matches(&payload.room_id, room))
}

fn room_identifier_matches(value: &str, room: &RoomConfig) -> bool {
    let value = value.trim();
    !value.is_empty()
        && [
            room.room_id.as_str(),
            room.channel_id.as_str(),
            room.channel_slug.as_str(),
            room.channel_name.as_str(),
        ]
        .contains(&value)
}

fn has_active_session_leave_job(
    context: &AutomationContext<'_>,
    session: &VoiceCaptureSessionStatus,
) -> bool {
    context.has_active_job_in_guild(JobKind::DiscordVoiceLeave, &session.guild_id, |job| {
        job.discord_voice_leave_payload()
            .is_some_and(|payload| payload.session_id == session.session_id)
    })
}

fn duplicate_session_leave_job(session: &VoiceCaptureSessionStatus) -> Job {
    Job::discord_voice_leave(
        session.guild_id.clone(),
        session.voice_channel_id.clone(),
        "runtime_automation",
        DiscordVoiceLeavePayload {
            session_id: session.session_id.clone(),
            reason: "duplicate_voice_bot_in_channel".to_string(),
        },
    )
}
