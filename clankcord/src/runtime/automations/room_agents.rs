use crate::Result;
use crate::runtime::automations::{
    Automation, AutomationContext, AutomationOutput, AutomationVoiceState,
};
use crate::runtime::timeline::{parse_instant, utc_now};
use crate::runtime::util::first_value_string;
use crate::runtime::{
    DiscordVoiceLeavePayload, Job, JobKind, RoomAgentPlacementAction, RoomConfig,
    VoiceCaptureSessionStatus,
};
use serde_json::Value;

pub(crate) struct RoomAgentPlacementAutomation;

impl Automation for RoomAgentPlacementAutomation {
    fn name(&self) -> &'static str {
        "room_agent_placement"
    }

    fn evaluate(&self, context: &AutomationContext<'_>) -> Result<AutomationOutput> {
        if !room_placement_automation_enabled() {
            return Ok(AutomationOutput::empty());
        }
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

fn room_placement_automation_enabled() -> bool {
    true
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
        let occupants = room_occupants(context.voice_state(), room);
        let manual_hold =
            context.room_control_datetime_active(&room.channel_id, "manual_hold_until");
        let manual_leave = manual_leave_active(context, room);
        let auto_suppressed =
            context.room_control_datetime_active(&room.channel_id, "auto_join_suppressed_until");

        if voice_bot_present
            && room_empty_past_grace(
                context,
                room,
                context.pool_config().auto_leave_empty_seconds,
            )
        {
            return Self {
                action: Some(RoomAgentPlacementAction::Leave),
                reason: "auto_policy_empty",
                cooldown_seconds: Some(context.pool_config().auto_rejoin_cooldown_seconds),
            };
        }

        if manual_leave {
            if voice_bot_present {
                return Self {
                    action: Some(RoomAgentPlacementAction::Leave),
                    reason: "manual_leave",
                    cooldown_seconds: Some(context.pool_config().manual_override_seconds),
                };
            }
            return Self {
                action: None,
                reason: "manual_leave",
                cooldown_seconds: None,
            };
        }

        if manual_hold {
            if !voice_bot_present && available_bot && !occupants.is_empty() {
                return Self {
                    action: Some(RoomAgentPlacementAction::Join),
                    reason: "manual_hold",
                    cooldown_seconds: None,
                };
            }
            return Self {
                action: None,
                reason: "manual_hold",
                cooldown_seconds: None,
            };
        }

        if voice_bot_present {
            if single_deafened_participant_past_grace(
                occupants,
                context.pool_config().auto_leave_single_deafened_seconds,
            ) {
                return Self {
                    action: Some(RoomAgentPlacementAction::Leave),
                    reason: "auto_policy_single_deafened",
                    cooldown_seconds: Some(context.pool_config().auto_rejoin_cooldown_seconds),
                };
            }
            return Self {
                action: None,
                reason: "present",
                cooldown_seconds: None,
            };
        }

        if context.pool_config().auto_join_enabled
            && room.auto_join
            && !auto_suppressed
            && occupants.len() >= context.pool_config().auto_join_min_participants
            && available_bot
        {
            return Self {
                action: Some(RoomAgentPlacementAction::Join),
                reason: "auto_join",
                cooldown_seconds: None,
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

fn room_occupants<'a>(voice_state: &'a AutomationVoiceState, room: &RoomConfig) -> &'a [Value] {
    voice_state
        .room_occupants
        .get(&room.channel_id)
        .expect("automation voice state is missing room occupants")
}

fn manual_leave_active(context: &AutomationContext<'_>, room: &RoomConfig) -> bool {
    if !context.room_control_datetime_active(&room.channel_id, "auto_join_suppressed_until") {
        return false;
    }
    let Some(control) = context.room_control(&room.channel_id) else {
        return false;
    };
    matches!(
        control.auto_join_suppression_reason.as_deref(),
        Some("manual_leave" | "manual_leave_all")
    )
}

fn room_empty_past_grace(
    context: &AutomationContext<'_>,
    room: &RoomConfig,
    grace_seconds: i64,
) -> bool {
    if !room_occupants(context.voice_state(), room).is_empty() {
        return false;
    }
    let Some(empty_since) = context.voice_state().room_empty_since.get(&room.channel_id) else {
        return false;
    };
    utc_now().signed_duration_since(*empty_since) > chrono::Duration::seconds(grace_seconds)
}

fn single_deafened_participant_past_grace(occupants: &[Value], grace_seconds: i64) -> bool {
    let [occupant] = occupants else {
        return false;
    };
    participant_deafened(occupant)
        && voice_state_updated_at(occupant).is_some_and(|updated_at| {
            utc_now().signed_duration_since(updated_at) >= chrono::Duration::seconds(grace_seconds)
        })
}

fn participant_deafened(occupant: &Value) -> bool {
    voice_state_bool(occupant, "deaf") || voice_state_bool(occupant, "self_deaf")
}

fn voice_state_bool(occupant: &Value, key: &str) -> bool {
    occupant.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn voice_state_updated_at(occupant: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    parse_instant(&first_value_string(occupant, &["updated_at", "updatedAt"]))
}

fn has_available_voice_bot(voice_state: &AutomationVoiceState) -> bool {
    voice_state.bots.iter().any(|bot| {
        bot.ready
            && bot.current_channel_id.trim().is_empty()
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
        job.scope_id == room.channel_id
    })
}

fn placement_targets_room(job: &Job, room: &RoomConfig) -> bool {
    room_identifier_matches(&job.scope_id, room)
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
