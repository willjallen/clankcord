# Agent Session Text Routing Plan

Status: current implementation plan

## Goal

Support text message ingestion for agents without ambiguous shared-channel routing.

The route authority is the Clankcord agent session record. Voice rooms, managed Discord
threads, and DMs are ingress surfaces that resolve to exactly one agent session before
Codex is invoked.

## Hard Rules

- Every agent input belongs to one persisted agent session before it reaches Codex.
- A non-DM agent session owns one managed Discord thread.
- A DM agent session responds in DM only.
- Voice channels map to the current active agent session for that voice channel.
- Managed thread messages route by `thread_id` to the owning agent session.
- Plain top-level `agent-chat` messages are not follow-ups. They do not route by recency,
  author, quoted text, or guesswork.
- `agent-chat` can remain a dedicated explicit sink, but it is not used as an implicit
  ingress router for session follow-ups.
- Agent work serializes by agent session, not by voice channel.
- Expired sessions are not silently reused.

## Route Model

```text
voice:<guild_id>:<voice_channel_id>
  -> current active AgentSessionRecord
  -> managed Discord thread
  -> Codex session id

thread:<guild_id>:<thread_id>
  -> AgentSessionRecord that owns the thread
  -> same Codex session id

dm:<user_id>
  -> current active DM AgentSessionRecord
  -> Discord DM channel
  -> Codex session id
```

## Agent Session Record

```text
agent_session_id              internal Clankcord id, known before Codex runs
codex_session_id              returned by Codex after first invocation
route_kind                    voice | dm | thread
route_key                     stable ingress key
guild_id
voice_channel_id
dm_user_id
discord_thread_id             non-DM only
discord_parent_channel_id     non-DM only
response_sink                 channel:<thread_id> or dm:<user_id>
state                         starting | active | expired | failed
created_at
last_activity_at
expires_at
```

## Voice Flow

1. A voice activation or command resolves `voice:<guild_id>:<voice_channel_id>`.
2. If an active, unexpired session exists, reuse it.
3. If no active session exists, create a new session record and a managed Discord thread.
4. Create the `AgentTask` with `agent_session_id`.
5. Codex resumes from `codex_session_id` if the session has one.
6. The agent response defaults to the session response sink.
7. Text follow-ups inside the managed thread route back to the same session.

## DM Flow

1. A DM resolves `dm:<user_id>`.
2. If an active, unexpired DM session exists, reuse it.
3. If no active session exists, create a new DM session record.
4. Responses stay in DM.

## Expiration

When a session expires, it stops being selected by its route key. The next voice or DM
message creates a new agent session. Messages posted into expired managed threads are
handled as expired-session input, not as a request to create a new session in that same
thread.

## Job Shape

```text
DiscordTextMessage job
  -> records a discord_text_message event
  -> resolves DM or managed thread route
  -> emits AgentTask(agent_session_id)

WakeActivation / Command job
  -> resolves voice route to an active or new session
  -> emits AgentTask(agent_session_id)

AgentTask job
  -> loads AgentSessionRecord
  -> invokes Codex with codex_session_id when present
  -> stores returned codex_session_id
  -> requires responses through the session response sink
```

## Concurrency

Session creation is serialized by job ordering. Jobs that can create or select a voice
or DM session use the route ordering key:

```text
agent:route:<route_key>
```

That key covers the active-session lookup and, when needed, the new session/thread
creation. Concurrent requests for the same voice route or DM route therefore converge
on one active session through the scheduler instead of racing into duplicate sessions.

The job ordering key for agent tasks is:

```text
agent:session:<agent_session_id>
```

That gives these properties:

- same Codex session: serialized FIFO
- different voice sessions: concurrent
- different DM users: concurrent
- same DM user/session: serialized
