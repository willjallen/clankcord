# Agents And Sessions

An agent session is the durable route authority for Codex-backed work. Voice routes, managed Discord thread follow-ups, and DMs all resolve to a persisted `AgentSessionRecord` before Codex is invoked. The session connects Discord routing, the Codex session id, the response target, lifecycle cap, resume lineage, and the serialized ordering key used by `agent_task`.

```text
voice wake, voice command, DM, or managed thread message
      |
      v
resolve AgentSessionRecord
      |
      v
agent_task ordered by session
      |
      v
discord_typing_indicator start
      |
      v
Codex process invocation
      |
      v
discord_typing_indicator stop
      |
      v
Clankcord response command
      |
      v
text_delivery -> discord_text_send
```

## Routing

Every agent input belongs to one persisted session. Voice inputs resolve by `voice:<guild_id>:<scope_id>`, where `scope_id` is the Discord voice channel id. DM inputs resolve by `dm:<scope_id>`, where `scope_id` is the Discord user id. Managed Discord thread messages resolve by the `discord_thread_id` stored on the voice session that owns that thread.

Top-level `agent-chat` channel messages complete as ignored text ingress. `agent_chat` is a text-delivery target used for runtime responses; follow-up conversation happens in the managed thread, DM, or active voice route. Retired sessions fall out of route selection, managed thread messages can reactivate the owning voice session, and agent work serializes by `agent:session:<agent_session_id>`.

```text
voice:<guild_id>:<scope_id>
      -> active AgentSessionRecord
      -> managed Discord thread
      -> Codex session id

managed thread message
      -> lookup by discord_thread_id
      -> owning voice AgentSessionRecord
      -> same Codex session id

dm:<user_id>
      -> active DM AgentSessionRecord
      -> Discord DM target
      -> Codex session id
```

## Session Record

`AgentSessionRecord` stores the route, Discord target, Codex identity, lifecycle, cap deadline, retirement fields, and resume lineage.

```text
agent_session_id
codex_session_id
route_kind                  voice | dm
route_key
guild_id
scope_id
dm_user_id
voice_capture_session_id
discord_thread_id
discord_parent_channel_id
text_target
state                       starting | active | retired | failed
created_at
last_activity_at
max_active_until
retired_at
retirement_reason
retired_by_user_id
resumed_from_agent_session_id
```

Voice sessions start in `starting` while `agent_session_start` is queued. The start job marks the session `active` and creates the first `agent_task`. A voice session stores its managed Discord thread when the first response-surface operation needs a Discord channel. Agent-task typing start and session-targeted text delivery use that same managed-thread allocation path. DM sessions are created active with a DM target. Active session lifetime is capped at eight hours from `created_at`; activity updates `last_activity_at` and does not extend `max_active_until`.

## Voice Sessions

A wake activation or voice command resolves the voice route. The runtime retires due sessions, reuses an active session for the route, or reuses a starting session for the same route. A route without a selectable session creates a new `AgentSessionRecord` and an `agent_session_start` job.

`agent_session_start` marks the session active and creates the first `agent_task`. The agent task creates a `discord_typing_indicator` start child before Codex runs. For a voice session that needs its managed thread, the typing start job creates a `discord_forum_thread_create` child, stores the resulting channel target on the session, and starts the typing heartbeat in that thread. `text_delivery` with the `session` target resolves the current session target when the message is sent and reuses the stored thread. The thread starts with a readable name from the voice room name and the session creation timestamp in the configured local timezone, such as `Code Lounge 2026-05-17 03:28`. The opening post names the actual voice room, mentions the requester and the other users currently present in that voice channel, and records the `agent_session_id`. Later messages inside that managed thread route back to the same session through `discord_thread_id`.

```text
wake activation or command
      |
      v
resolve voice route
      |
      +--> reuse active or starting session
      |
      +--> create starting session
              |
              v
          agent_session_start
              |
              +--> mark session active
              +--> agent_task
                      |
                      +--> discord_typing_indicator(start)
                      |       |
                      |       +--> discord_forum_thread_create when needed
                      |
                      +--> Codex process
                      |
                      +--> discord_typing_indicator(stop)
                      |
                      +--> text_delivery(session)
                              |
                              +--> discord_text_send
```

## DM Sessions

A DM resolves to `dm:<user_id>`. The runtime retires due sessions, reuses an active DM session for that user, or creates a new active record. The text target remains `dm:<user_id>`, and responses stay in DM unless an explicit response command selects another target.

## Text Ingress

Discord text messages enter as `discord_text_message` jobs. The ingress handler routes DMs to DM agent sessions and managed thread messages to the owning voice session. Routed messages append a `discord_text_message` timeline event. An active managed thread creates an `agent_task` child for the selected session. A managed thread attached to a retired or capped voice session creates an `agent_session_resume` child with the message text; resume reactivates the stored session, keeps its Codex session id, and creates the first `agent_task` in that reactivated session.

Empty messages, top-level `agent-chat` messages, and unmanaged guild channels complete as ignored ingress cases. They still pass through the runtime job path, which keeps text ingress visible in job inspection and timeline diagnostics.

## Thread Titles

Runtime maintenance keeps managed thread names readable. The maintenance handler scans active voice sessions with managed Discord threads and counts completed agent tasks that delivered a visible response into the session thread. When a session has at least two delivered responses beyond the last title-refresh attempt, maintenance creates one `agent_thread_title_refresh` job for that pass.

`agent_thread_title_refresh` runs on the agent lane with the same `agent:session:<agent_session_id>` ordering key as agent work. It writes an `agent_thread_title_refresh_attempted` event before launching Codex, builds a compact title prompt from the session id, current thread name, voice room name, response count, and visible request/response summaries, then creates a `discord_forum_thread_rename` child with the generated title. When the rename child completes, the parent appends `agent_thread_titled` with the title, response count, refresh job id, and rename job id.

The title-refresh selector is bounded by three durable facts: one candidate per maintenance pass, a single active title-refresh job per session, and a response-count gate recorded by the attempted event. A failed Codex or Discord rename attempt advances the attempted response count, so the next maintenance pass waits for two more delivered agent responses before creating another title-refresh job.

## Retirement And Resume

Agent session retirement is runtime maintenance and an explicit user action. `agent_session_retirement` runs from `runtime_maintenance` and retires sessions whose `max_active_until` has passed or whose bound `voice_capture_session_id` is no longer active. `agent_session_sunset` retires a selected session when a user or operator asks to end it. Retirement sets `state = retired`, records `retired_at`, `retirement_reason`, and `retired_by_user_id` when present, and appends `agent_session_retired`.

Retired sessions remain queryable by id, list, search, and managed-thread continuation. Route lookup selects active sessions for normal voice and DM routing. Managed-thread continuation creates `agent_session_resume` for the stored voice session before agent work starts.

`agent_session_resume` reactivates the selected retired session. Resume takes over the target route by retiring the current active or starting session with `agent_session_resume_route_takeover`, clears the selected session's retirement fields, extends its active cap, and keeps its existing Codex session id. Voice resume uses the selected session's existing Discord thread as the session target. A voice session without an existing Discord thread fails resume. A resume job with message text creates the first `agent_task` in the reactivated session.

```text
retired AgentSessionRecord
      |
      +--> agent_session_resume
              +--> same AgentSessionRecord becomes active
              +--> optional first agent_task
```

## Agent Task

`agent_task` is a blocking snapshot job on the `agent` lane. It waits for a `discord_typing_indicator` start child before launching Codex, validates the job and session identity, creates or reuses the session workspace under `paths.agent_workspaces_root`, runs preflight checks, builds a prompt from templates loaded through `prompts.dir` and a compact five-minute timeline context, includes master session instructions on the first Codex invocation for the session, invokes Codex with the prior session id when present, stores prompt, result, raw JSONL output, stderr preview, command display, model, session id, and usage metadata, then waits for a `discord_typing_indicator` stop child before final result handling. The Codex process uses the runtime-selected workspace, skips the Git repository trust check for that workspace, and ignores user Codex config.

The detailed process contract is documented in [Agent Runtime Contract](0-10-agent-runtime-contract.md).

## Publication

A successful agent task ends by submitting visible output through Clankcord or declaring the task complete without publication.

```text
RESPONSE_SUBMITTED
    one or more text_delivery jobs exist for the source agent task

NO_RESPONSE_NEEDED
    the agent intentionally completed the task without publication
```

Agents use `NO_RESPONSE_NEEDED` for false activations, accidental invocations, read-only checks, and no-op work where a visible message adds no useful information. State-changing Clankcord commands require a concise visible response after the command reports success.

Codex final text is treated as a control signal. Visible Discord output is created through `clankcord responses ...`, `text_delivery`, and domain-executed Discord text jobs. An agent task may submit multiple visible responses; each submission creates its own `text_delivery` job tied to the same source task. Session-targeted text delivery resolves the destination at send time. When the source task's session has been retired by `agent_session_resume_route_takeover`, delivery uses the current active session for that route. This preserves runtime authority over Discord delivery, session routing, DMs, and managed threads.

## Recovery

Startup recovery inspects interrupted running `agent_task` jobs. A task with an existing text-delivery job for the same source job is marked complete. The remaining interrupted task is marked `failed`, with the restart interruption recorded in dispatch metadata.

Agent dispatch retries non-infrastructure failures up to three attempts. Infrastructure failures such as auth and token-refresh errors mark the job `failed`, record the dispatch cause in `metadata.error` and `metadata.agent_task.dispatch_error`, and may submit a visible "ChatGPT unavailable" text delivery when the response path is still empty.
