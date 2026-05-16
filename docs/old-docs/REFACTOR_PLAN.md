# Discord Adapter And Runtime Domain Refactor Plan

Status: active implementation plan

## Goal

Make Discord ingress, runtime job policy, and Discord side effects line up with the job system.

The runtime should decide by creating and resolving durable jobs. Discord adapters should translate
Discord protocol events into jobs and execute Discord IO jobs. They should not own runtime policy.

## Hard Rules

- Discord gateway events become durable ingress jobs.
- Runtime domain code lowers ingress jobs into command, feedback, agent, voice, and Discord IO jobs.
- Discord REST and gateway mechanics stay in `adapters/discord`.
- Runtime domain modules do not call Discord REST helpers directly.
- Voice adapter code owns live voice clients, live capture buffers, and audio playback only.
- Voice room placement, leave/join policy, playback orchestration, and command lowering are runtime
  domain concerns.
- No runtime locks for routing policy. Concurrency is handled by job ordering keys and scheduler lanes.
- Hard cuts only. Do not add compatibility shims or duplicate old and new concepts.

## Target Module Shape

```text
adapters/discord/
  gateway/
    mod.rs
    text.rs
    slash.rs
    components.rs
    registration.rs
  rest/
    messages.rs
    interactions.rs
    threads.rs
  voice/
    live.rs
    client_connection.rs
    capture.rs
    session.rs

runtime/
  domain/
    ingress/
      discord_text.rs
      discord_slash.rs
    interactions/
      agent_sessions.rs
      commands.rs
      confirmations.rs
      policy.rs
      tasks.rs
    voice/
      room_placement.rs
      playback.rs
      status.rs
    voice_capture/
      segments.rs
      wake_probes.rs
      wake_activations.rs
    messaging/
      text_delivery.rs
    transcripts/
      publication.rs
    feedback.rs
  agents/
  jobs/
```

## Immediate Hard Cuts

- Delete `runtime/sessions/`.
  - Move `runtime/sessions/join.rs` to `runtime/domain/voice/room_placement.rs`.
  - Move `runtime/sessions/playback.rs` to `runtime/domain/voice/playback.rs`.
  - Move `runtime/sessions/types.rs` to `runtime/voice/status.rs`.
- Delete standalone `runtime/bots.rs`.
  - Move `RuntimeBotStatus` to `runtime/voice/status.rs`.
- Rename overloaded runtime status types.
  - `RuntimeBotStatus` -> `VoiceBotStatus`.
  - `RuntimeSessionStatus` -> `VoiceCaptureSessionStatus`.
  - Adapter-local `VoiceSession` -> `LiveVoiceSession`.

## Discord Gateway Ingress

Create a gateway-focused adapter layer:

```text
adapters/discord/gateway/text.rs
adapters/discord/gateway/slash.rs
adapters/discord/gateway/components.rs
adapters/discord/gateway/registration.rs
```

Gateway modules should:

- Receive Discord protocol events.
- Acknowledge or defer interactions when Discord requires it.
- Submit durable runtime jobs through `RuntimeJobSink`.
- Avoid runtime policy and avoid direct room/session mutation.

## Slash Commands

Add first-class slash command ingress:

```text
JobKind::DiscordSlashCommand
DiscordSlashCommandPayload {
  interaction_id
  interaction_token
  application_id
  guild_id
  channel_id
  user_id
  username
  command_name
  options
  created_at
  response_visibility
}
```

Expected lowering:

```text
/join
  DiscordSlashCommand -> Command(join_room) -> RoomAgentPlacement(join) -> DiscordVoiceJoin

/leave
  DiscordSlashCommand -> Command(leave_room) -> RoomAgentPlacement(leave) -> DiscordVoicePlayback -> DiscordVoiceLeave

/feedback
  DiscordSlashCommand -> Feedback -> Discord interaction followup or channel text job
```

Slash command jobs use the target ingress route ordering key. Join/leave for the same room must
serialize with wake activations, dashboard commands, and text ingress for that room.

## Components

Move confirmation button handling out of `adapters/discord/voice/live.rs`.

Target flow:

```text
Discord component interaction
  -> DiscordComponentInteraction job
  -> RuntimeControl child job
  -> DiscordInteractionEditOriginal child job
```

The voice adapter should not know about confirmation buttons.

## Discord REST Side Effects

Runtime domain code should not call REST helpers such as:

```text
send_message
create_dm_channel
create_forum_thread
discord_request
```

Use Discord IO jobs instead:

```text
DiscordTextSend
DiscordForumThreadCreate
DiscordInteractionEditOriginal
DiscordInteractionFollowup
```

Adapters execute those jobs and return typed outputs.

## Response Concept

Drop `Response` as an internal domain concept.

It remains useful only at user-facing tool/CLI/agent surfaces as "send a response to the right
place for this source job/session." Internally it lowers through runtime text routing into Discord
text IO jobs:

```text
agent/tool surface Response request
  -> TextDelivery
  -> resolve target from source job or agent session
  -> DiscordTextSend
```

`JobKind::Response` is removed. The runtime tracks `TextDelivery` routing jobs and concrete
`DiscordTextSend` jobs.

## Implementation Order

1. Move voice status and voice domain files out of `runtime/sessions` and `runtime/bots`.
2. Rename runtime voice status types and adapter live session type.
3. Move Discord text adapter under `adapters/discord/gateway`.
4. Move component interaction handling out of voice live adapter into gateway components.
5. Add slash command registration and `DiscordSlashCommand` ingress jobs.
6. Add Discord interaction reply/followup IO jobs.
7. Convert confirmations, publications, and agent thread creation from direct REST calls into
   Discord IO child jobs.
8. Keep the `clankcord responses ...` CLI as the tool surface, backed by `TextDelivery`.

## Verification

- `cargo fmt`
- `cargo check`
- `cargo test`
- Live `/debug` endpoint still loads.
- `/join` and `/leave` create room placement jobs.
- `/feedback` creates a durable feedback record/job and returns a Discord acknowledgement.
