use crate::Result;
use crate::runtime::automations::{Automation, AutomationContext, AutomationOutput};
use crate::runtime::{
    Job, JobKind, RoomAgentPlacementAction, RoomConfig, Runtime, RuntimeBotStatus,
};

pub(crate) struct RoomAgentPlacementAutomation;

impl Automation for RoomAgentPlacementAutomation {
    fn name(&self) -> &'static str {
        "room_agent_placement"
    }

    fn evaluate(&self, context: &AutomationContext<'_>) -> Result<AutomationOutput> {
        let runtime = context.runtime();
        let available_bot = has_available_voice_bot(runtime);
        let mut output = AutomationOutput::empty();
        for room in runtime.known_rooms() {
            let decision = RoomAgentPlacementDecision::evaluate(runtime, &room, available_bot);
            if let Some(action) = decision.action {
                if !has_active_placement_job(context, &room, action) {
                    output.emit(placement_job(
                        &room,
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
    fn evaluate(runtime: &Runtime, room: &RoomConfig, available_bot: bool) -> Self {
        let active_session = runtime.active_session_id_for_room(room).is_some();
        let auto_suppressed =
            runtime.room_control_datetime_active(&room.channel_id, "auto_join_suppressed_until");
        let manual_hold =
            runtime.room_control_datetime_active(&room.channel_id, "manual_hold_until");
        let listening_paused =
            runtime.room_control_datetime_active(&room.channel_id, "listening_paused_until");
        let auto_desired = runtime.auto_join_enabled && room.auto_join;
        let should_be_present =
            !auto_suppressed && !listening_paused && (manual_hold || auto_desired);

        if should_be_present && !active_session && available_bot {
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

        if active_session && !should_be_present {
            return Self {
                action: Some(RoomAgentPlacementAction::Leave),
                reason: leave_reason(auto_suppressed, listening_paused),
                cooldown_seconds: Some(runtime.manual_leave_cooldown_seconds),
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

fn has_available_voice_bot(runtime: &Runtime) -> bool {
    runtime.bots.values().any(voice_bot_available)
}

fn voice_bot_available(bot: &RuntimeBotStatus) -> bool {
    bot.ready && bot.joining_session_id.is_empty() && bot.assigned_session_id.is_empty()
}

fn has_active_placement_job(
    context: &AutomationContext<'_>,
    room: &RoomConfig,
    action: RoomAgentPlacementAction,
) -> bool {
    context.has_active_job(
        JobKind::RoomAgentPlacement,
        &room.guild_id,
        &room.channel_id,
        |job| {
            job.room_agent_placement_payload()
                .is_some_and(|payload| payload.action == action)
        },
    )
}
