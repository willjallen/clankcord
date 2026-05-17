# Agent Session Lifecycle Plan

Status: working design document
Last updated: 2026-05-17

## Goal

Agent sessions should live at the same product granularity as the voice-channel
conversation they serve. A voice session stays active while that voice-channel session is
active, up to an eight-hour cap. A user can sunset the session earlier. After retirement,
the session remains searchable, inspectable, and available as the source for an explicit
resume.

The runtime owns every lifecycle transition. Agents and operators use named Clankcord CLI
commands. State-changing commands create runtime jobs, and runtime maintenance retires
sessions that reach their lifecycle boundary.

## Product Model

An agent session is the route authority for active work and the durable record for later
inspection. The active route is small: one current voice route or DM route points at one
selectable session. Search and resume work against the same durable session records and
their associated jobs, timeline events, managed Discord thread, prompts, results, and
published responses.

Resume is represented as lineage. A resumed conversation creates a new active
`AgentSessionRecord` with `resumed_from_agent_session_id` set to the retired session. The
new record can reuse the prior `codex_session_id` when it exists, and it receives its own
route binding, managed thread, timestamps, cap, jobs, and audit events. This keeps each
active lifecycle bounded while preserving continuity for users.

## Hard Rules

- Exactly one active selectable session exists for a route.
- Active session lifetime is capped at eight hours from session creation.
- `last_activity_at` records use; it does not extend the eight-hour cap.
- Voice sessions retire when the voice-channel session ends or the eight-hour cap is
  reached.
- User sunset is an explicit lifecycle command and uses the same state transition as
  maintenance retirement.
- Retired sessions remain durable query targets.
- Retired sessions are not selected by route lookup.
- Resume creates a new active record linked to the prior session.
- CLI commands expose lifecycle actions by name; agents do not write session rows or
  synthesize lifecycle payloads directly.

## State

Use a small state set:

```text
starting
active
retired
failed
```

`starting` covers managed-thread creation before a voice session is usable. `active` is
the only selectable state for normal routing. `retired` is a complete historical record
that can be searched and resumed from. `failed` covers setup or dispatch failure that
prevents normal use.

Session records need the current routing fields plus lifecycle fields:

```text
agent_session_id
codex_session_id
route_kind
route_key
guild_id
voice_channel_id
dm_user_id
voice_capture_session_id
discord_thread_id
discord_parent_channel_id
text_target
state
created_at
last_activity_at
max_active_until
retired_at
retirement_reason
retired_by_user_id
resumed_from_agent_session_id
```

The product term is `retired`. Existing expiry mechanics become deadline checks and
maintenance retirement. Route selection can keep a deadline predicate so a session at its
cap is never reused between maintenance ticks; the maintenance job materializes the
terminal state and appends the audit event.

## Voice Lifetime

Voice routing binds a session to the active capture or voice-presence epoch for the
guild and voice channel. The session remains selectable while all of these are true:

```text
state == active
route_key == voice:<guild_id>:<voice_channel_id>
now < max_active_until
the bound voice-channel session is active
```

When the voice-channel session ends, maintenance retires the agent session with
`retirement_reason = voice_session_ended`. When the cap is reached, maintenance retires
it with `retirement_reason = max_duration`. If a task is already running, the task keeps
its explicit `agent_session_id` and can publish through the stored response target; new
route selection and managed-thread ingress stop selecting the retired record.

DM sessions use the same eight-hour active cap and explicit sunset/resume commands. DM
resume creates a new DM session linked to the retired one.

## CLI Surface

Add a top-level `agent-sessions` command group. It is the agent and operator surface for
finding, inspecting, sunsetting, and resuming sessions.

```bash
clankcord agent-sessions current --guild <guild-id> --channel <voice-channel-id>
clankcord agent-sessions list --guild <guild-id> --channel <voice-channel-id> --state active
clankcord agent-sessions search --guild <guild-id> --query "floating point" --since -30d
clankcord agent-sessions get <agent-session-id> --format json --file session.json
clankcord agent-sessions sunset <agent-session-id> --reason "user asked"
clankcord agent-sessions resume <agent-session-id> --guild <guild-id> --channel <voice-channel-id>
```

`current`, `list`, `search`, and `get` are reads. They support compact JSON by default
and `--file <path> --format json` for large output.

`sunset` creates a lifecycle job that marks the selected session retired, records
`retired_by_user_id` when available, appends an `agent_session_retired` timeline event,
and returns the final session state.

`resume` creates a lifecycle job that validates the source session is resumable, creates
a new active route record linked by `resumed_from_agent_session_id`, starts a managed
thread when the target is voice, and returns the new `agent_session_id`. If a resume
message is supplied, the job creates the first `agent_task` in the new session after the
route is ready.

Search output should be shaped for action:

```json
{
  "agent_session_id": "ags_...",
  "state": "retired",
  "route_key": "voice:guild:channel",
  "created_at": "...",
  "retired_at": "...",
  "retirement_reason": "voice_session_ended",
  "resumed_from_agent_session_id": "",
  "discord_thread_id": "...",
  "latest_activity": "...",
  "matched_fields": ["transcript", "agent_result"],
  "snippet": "...",
  "resume_command": "clankcord agent-sessions resume ags_... --guild ... --channel ..."
}
```

## HTTP And Jobs

Expose the CLI through narrow HTTP routes:

```text
GET  /v1/voice/agent-sessions/current
GET  /v1/voice/agent-sessions
GET  /v1/voice/agent-sessions/search
GET  /v1/voice/agent-sessions/{agent_session_id}
POST /v1/voice/agent-sessions/{agent_session_id}/sunset
POST /v1/voice/agent-sessions/{agent_session_id}/resume
```

Mutation routes submit jobs. A small lifecycle job family is enough:

```text
agent_session_sunset
agent_session_resume
agent_session_retirement
```

`agent_session_retirement` is maintenance work. `runtime_maintenance` schedules it next
to voice status sync, automation evaluation, stale-job sweeps, and ephemeral job GC. The
retirement handler loads due active sessions, computes concrete retirement reasons, marks
records retired, and appends one timeline event per retired session.

## Search

Search should begin with the durable data Clankcord already owns:

- session projection fields from `agent_sessions`
- managed Discord thread id and parent channel id
- related `agent_task` job metadata
- prompt and result artifacts
- text-delivery jobs tied to agent tasks
- Discord text messages in the managed thread
- transcript events in the session's voice route window

The first implementation can produce a Postgres search document per session from these
sources and refresh it when agent tasks complete, text ingress lands, or retirement runs.
The command returns ranked session rows with snippets and enough identifiers for the
agent to inspect or resume the session.

## Prompt And Preflight

The master agent instructions should name the lifecycle tools:

```text
Use `clankcord agent-sessions current`, `list`, `search`, and `get` to find previous
sessions. Use `clankcord agent-sessions sunset` when a user asks to end the current
session. Use `clankcord agent-sessions resume` when a user asks to continue a retired
session.
```

Agent preflight should check the new command group and the specific read and mutation
subcommands. The per-job prompt should include `agent_session_id` and
`resumed_from_agent_session_id` when present.

## Implementation Sequence

1. Hard-cut the session model to `starting | active | retired | failed`, add lifecycle
   fields, and replace the configurable idle expiry with an eight-hour active cap.
2. Add store reads for current, list, get, and route-safe active selection, keeping the
   hard cap in the selection predicate.
3. Add `agent_session_retirement` maintenance work and tests for voice-ended retirement,
   max-duration retirement, and idempotent repeated maintenance runs.
4. Add user sunset as a runtime job plus CLI and HTTP surfaces.
5. Add resume as a runtime job that creates a linked active record and starts the normal
   voice or DM route flow.
6. Add session search documents and the `search` command.
7. Update active architecture docs after the Rust behavior exists.

## Test Coverage

Focused tests should cover the lifecycle contract:

- route lookup selects one active session for a route
- route lookup excludes sessions at the eight-hour cap
- maintenance retires capped sessions and writes `agent_session_retired`
- maintenance retires sessions whose bound voice-channel session ended
- user sunset retires the selected session and stops managed-thread ingress
- resume creates a new active session linked to the retired source
- resume reuses the prior `codex_session_id` when the source has one
- CLI reads support `--file <path> --format json`
- search returns retired sessions with snippets and a resume command

The active architecture docs should be updated in the implementation change that makes
these tests pass.
