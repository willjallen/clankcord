# Agents And Sessions

An agent session is the durable route authority for Codex-backed work. Voice routes, managed Discord thread follow-ups, and DMs all resolve to a persisted `AgentSessionRecord` before Codex is invoked. The session connects Discord routing, the Codex session id, the response target, expiry, and the serialized ordering key used by `agent_task`.

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
Codex process invocation
      |
      v
Clankcord response command
      |
      v
text_delivery -> discord_text_send
```

## Routing

Every agent input belongs to one persisted session. Voice inputs resolve by `voice:<guild_id>:<voice_channel_id>`. DM inputs resolve by `dm:<user_id>`. Managed Discord thread messages resolve by the `discord_thread_id` stored on the voice session that owns that thread.

Top-level `agent-chat` channel messages complete as ignored text ingress. `agent_chat` is a text-delivery target used for runtime responses; follow-up conversation happens in the managed thread, DM, or active voice route. Expired sessions fall out of selection, and agent work serializes by `agent:session:<agent_session_id>`.

```text
voice:<guild_id>:<voice_channel_id>
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

`AgentSessionRecord` stores the route, Discord target, Codex identity, lifecycle, and expiry.

```text
agent_session_id
codex_session_id
route_kind                  voice | dm
route_key
guild_id
voice_channel_id
dm_user_id
discord_thread_id
discord_parent_channel_id
text_target
state                       starting | active | expired | failed
created_at
last_activity_at
expires_at
```

Voice sessions start in `starting` while the managed Discord thread is created. Once the thread child completes, the session becomes `active` and stores the channel target. DM sessions are created active with a DM target. Session expiry comes from `agents.session_expiry_seconds` in `config.toml`, clamped between 60 seconds and seven days.

## Voice Sessions

A wake activation or voice command resolves the voice route. The runtime reuses an active unexpired session that already owns a thread, or reuses a starting session for the same route. A route without a selectable session creates a new `AgentSessionRecord` and an `agent_session_start` job.

`agent_session_start` creates a `discord_forum_thread_create` child. After the thread child completes, the parent marks the session active, stores the thread target, and creates the first `agent_task`. Later messages inside that managed thread route back to the same session through `discord_thread_id`. Voice sessions require `agentThreadsChannelId` because the managed thread is the persistent public surface for the session.

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
              +--> discord_forum_thread_create
              +--> mark session active
              +--> agent_task
```

## DM Sessions

A DM resolves to `dm:<user_id>`. The runtime reuses an active unexpired DM session for that user or creates a new active record. The text target remains `dm:<user_id>`, and responses stay in DM unless an explicit response command selects another target.

## Text Ingress

Discord text messages enter as `discord_text_message` jobs. The ingress handler routes DMs to DM agent sessions and managed thread messages to the owning voice session. Routed messages append a `discord_text_message` timeline event and create an `agent_task` child for the selected session.

Empty messages, expired managed threads, top-level `agent-chat` messages, and unmanaged guild channels complete as ignored ingress cases. They still pass through the runtime job path, which keeps text ingress visible in job inspection and timeline diagnostics.

## Agent Task

`agent_task` is a blocking snapshot job on the `agent` lane. It validates the job and session identity, creates or reuses the session workspace under `paths.agent_workspaces_root`, runs preflight checks, builds a prompt from the active job and a compact five-minute timeline context, includes master session instructions on the first Codex invocation for the session, invokes Codex with the prior session id when present, and stores prompt, result, raw JSONL output, stderr preview, command display, model, session id, and usage metadata.

The detailed process contract is documented in [Agent Runtime Contract](0-9-agent-runtime-contract.md).

## Publication

A successful agent task ends by submitting visible output through Clankcord or declaring the task complete without publication.

```text
RESPONSE_SUBMITTED
    one or more text_delivery jobs exist for the source agent task

NO_RESPONSE_NEEDED
    the agent intentionally completed the task without publication
```

Codex final text is treated as a control signal. Visible Discord output is created through `clankcord responses ...`, `text_delivery`, and Discord text adapter jobs. An agent task may submit multiple visible responses; each submission creates its own `text_delivery` job tied to the same source task. This preserves runtime authority over Discord delivery, session routing, DMs, and managed threads.

## Recovery

Startup recovery inspects interrupted running `agent_task` jobs. A task with an existing text-delivery job for the same source job is marked complete. The remaining interrupted task is marked `agent_dispatch_failed`.

Agent dispatch retries non-infrastructure failures up to three attempts. Infrastructure failures such as auth and token-refresh errors mark the job `agent_dispatch_failed` and may submit a visible "ChatGPT unavailable" text delivery when the response path is still empty.
