# Command Surfaces

CLI commands, HTTP routes, Discord slash commands, Discord text messages, dashboard actions, confirmation buttons, and agent tool calls all enter the same runtime model. Each surface has its own boundary mechanics. Once a request crosses the boundary, state-changing work is represented as typed jobs or runtime-control jobs.

```text
CLI / HTTP / Discord / dashboard / agent tool
      |
      v
boundary parser or protocol handler
      |
      v
runtime command, job, or runtime-control request
      |
      v
durable jobs and timeline events
```

## CLI

The root `clankcord` command is both the operator surface and the primary tool surface for agents. It is organized by capability: service startup, status, rooms, messages, timeline, transcripts, conversations, context, participants, members, jobs, responses, automations, confirmations, pause, resume, and forget.

Voice and control mutations lower into typed command jobs. Automation, confirmation, response, and job-control commands call HTTP or runtime surfaces that create automation records, `text_delivery` jobs, or `runtime_control` jobs. Read commands query rendered timeline and runtime views.

Agent-facing reads default to compact JSON. Large outputs can be written with `--file <path> --format json`, leaving stdout as a short confirmation plus counts, ids, or window bounds. `--ephemeral` includes transient runtime events such as wake and audio internals. `--verbose` expands fields for the selected records.

Member and room-occupant commands are part of the agent contract. `members search`, `members resolve`, and `members get` read the Discord member cache and resolve names to durable Discord user ids. `rooms occupants` reads current voice-state rows for a room. Agents use these commands before writing automation conditions, DM targets, or participant references.

## HTTP

HTTP routes are mounted over the runtime handle. They cover health, status, voice state, commands, responses, automations, timeline, transcripts, conversations, context, participants, members, jobs, confirmations, debug views, and the dashboard.

```text
/healthz
/v1/status
/v1/voice/status
/v1/voice/pool/status
/v1/voice/rooms/occupants
/v1/voice/commands
/v1/voice/responses
/v1/voice/automations
/v1/voice/timeline/*
/v1/voice/transcript/*
/v1/voice/conversations/list
/v1/voice/context/resolve
/v1/voice/participant/trace
/v1/voice/members/*
/v1/voice/jobs/*
/v1/voice/confirmations/*
/v1/voice/debug/*
/debug
```

Read routes render runtime and timeline views. Mutation routes submit jobs or runtime-control jobs through `RuntimeHandle`.

## Runtime Commands

`CommandRequest` is the typed envelope for runtime commands. The command set covers agent tasks, live and draft transcript creation, transcript materialization, permanent publication, pause, deafen, resume, forget, leave, join, voice mute, and voice cue playback.

```text
agent_task
start_live_transcript
start_draft_transcript
materialize_transcript
make_permanent
pause_listening
deafen_listening
resume_listening
forget_window
leave_room
join_room
set_voice_mute
play_voice_cue
```

Command jobs either handle the control directly or emit child jobs. Transcript commands materialize windows and can create publication and refinement work. Pause, resume, and deafen update room controls and can create cue playback. Join and leave create `room_agent_placement`. Voice mute and cue commands create concrete voice adapter jobs. Agent task commands resolve or create an agent session and emit `agent_session_start` or `agent_task`. `forget_window` enters confirmation before executing.

## Confirmations

Sensitive commands use the confirmation flow. `forget_window` builds a preview from recent speech and transcript events, sends a DM confirmation card, enters `confirmation_pending`, and waits for an approve or cancel runtime-control job from CLI, HTTP, or Discord buttons. Approval creates the confirmed command child.

```text
forget_window
      |
      v
confirmation_required
      |
      +--> DM confirmation card
      +--> runtime_control approval or cancellation
      +--> approved command child
```

## Discord Ingress

Discord slash commands enter as `discord_slash_command` jobs. `/join` lowers to `command(join_room)`, `/leave` lowers to `command(leave_room)`, and `/feedback` completes with feedback output on the slash job. Unknown slash commands complete with `ignored_unknown_command`.

Discord text messages enter as `discord_text_message` jobs. Runtime ingress decides whether the message belongs to a DM session, managed agent thread, top-level `agent-chat` channel, or unmanaged channel. DMs and managed threads become agent tasks. Top-level `agent-chat` messages complete as ignored ingress. The `agent_chat` target remains available as a response sink for `text_delivery`.

## Responses

`responses send`, `responses dm`, `responses submit`, and `responses ask` create `text_delivery` jobs. `text_delivery` resolves the target to an agent session, agent-chat sink, concrete channel, or DM target, then creates `discord_text_send`.

Agents publish visible responses through the CLI. The supported path is command submission, `text_delivery`, and `discord_text_send`, which keeps Discord delivery under runtime job state and records delivery metadata with the source job. Each response submission creates a distinct `text_delivery` job, so one agent task can publish multiple messages.
