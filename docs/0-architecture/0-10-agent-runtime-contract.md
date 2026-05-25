# Agent Runtime Contract

Agent tasks are Codex process invocations owned by runtime jobs. The runtime chooses the agent session, starts the Discord typing indicator for the response surface, creates the workspace, builds the prompt, launches Codex, captures the result, stops the typing indicator, stores process metadata, and verifies that visible output was submitted through Clankcord response commands.

```text
agent_task job
      |
      +--> resolve persisted AgentSessionRecord
      +--> wait for discord_typing_indicator start
      +--> prepare workspace and environment
      +--> build prompt and context
      +--> run Codex process
      +--> record output and metadata
      +--> wait for discord_typing_indicator stop
      +--> verify response contract
```

The typing indicator is a durable Discord job with `start` and `stop` actions. A start job resolves the same agent-session text target used by response delivery. For a voice session with a stored managed thread, the Discord adapter sends the initial typing request and maintains the indicator with a heartbeat while the agent task is running. For a voice session awaiting its first forum thread, typing start completes with `skipped_no_session_thread`; the first session-targeted `text_delivery` allocates the managed thread through `discord_forum_thread_create` when the agent publishes a visible response. When Discord reports `Unknown Channel`, `Missing Access`, or `Missing Permissions` for a stored managed thread, the runtime records `agent_session_thread_unavailable`, clears the stored thread target, and completes typing with `skipped_unavailable_session_thread`. The stop job cancels any heartbeat after the Codex process returns and before the agent task reaches a terminal state.

## Workspace

Each persisted agent session has a writable directory under `paths.agent_workspaces_root` from `config.toml`. Codex runs with that directory as its current working directory. The directory holds notes, temporary files, command output, and intermediate artifacts. The source checkout is exposed through `CLANKCORD_REPO_DIR`.

The runtime image provides the agent process with `clang`, `python`, and `zip` alongside the Clankcord CLI, `rg`, and `jq`. Coding work happens inside the session workspace. Generated source files, benchmark outputs, and notes can be packaged there as zip artifacts and submitted through the response command surface.

Codex process behavior comes from the `[codex]` configuration. `model` controls the Codex model passed with `-m` when it is set. `reasoning_effort` is a typed `low`, `medium`, `high`, or `xhigh` value passed as Codex's `model_reasoning_effort` config override. `fast_mode` maps to Codex's `fast_mode` feature flag and is passed explicitly on every invocation. The runtime records the resolved command, model, reasoning effort, and fast-mode value in agent-task metadata.

Codex sandbox behavior also comes from `[codex]`. `bypass_sandbox = true` makes the runtime pass Codex's explicit sandbox-bypass flag for agent invocations. Docker Compose deployments can set `CLANKCORD_CODEX_BYPASS_SANDBOX=true` in the Compose environment, which overrides the TOML value at runtime. When that environment value is unset or empty, the runtime uses `config.toml`. Installations that rely on Codex's own sandbox leave the value false and use `sandbox` plus `approval_policy` from `config.toml`.

## Codex Authentication

Deployments give Codex a persistent home directory through `[codex].home`. In the Docker Compose deployment, `./clankcord/runtime-data/codex-home` is mounted at `/codex`, and `CODEX_HOME=/codex` is passed to every agent invocation. `/codex/auth.json` is the live Codex credential store. Codex reads access credentials from that file and writes refreshed credentials back into the same file as tokens rotate.

Operators initialize the mounted Codex home with `scripts/codex-login.sh`. The script stops the runtime, runs `codex login --device-auth` from a one-off container against the mounted `/codex` home, verifies login status, and recreates the runtime container. The runtime container then starts with the existing `/codex/auth.json` and leaves it intact across restarts. Moving a deployment to a new host is a state migration of the Codex home directory, including `auth.json`, `sessions/`, and Codex local state.

Every invocation receives job and session context through environment variables.

```text
CLANKCORD_AGENT_WORKDIR
CLANKCORD_REPO_DIR
CLANKCORD_AGENT_JOB_ID
CLANKCORD_AGENT_SESSION_ID
CLANKCORD_AGENT_GUILD_ID
CLANKCORD_AGENT_SCOPE_ID
CLANKCORD_AGENT_REQUESTED_BY_USER_ID
CLANKCORD_API_BASE_URL
```

The CLI uses these variables for agent-friendly defaults. `responses send`, `responses ask`, and `responses dm` can infer the current job, guild, scope, and requester from the environment while still accepting explicit flags for manual operation. Room-mutating commands require an explicit room target. Agents pass `--channel "$CLANKCORD_AGENT_SCOPE_ID"` for the current voice route and include `--requested-by-user-id "$CLANKCORD_AGENT_REQUESTED_BY_USER_ID"` on commands that accept a requester.

## Prompt

The runtime builds prompts in two stages: session bootstrap and agent invocation. The first Codex invocation for an agent session includes the session bootstrap sections and the current invocation sections. Later invocations in the same Codex session send the invocation sections. The persisted Codex session id is the runtime boundary; a populated session id means the next task resumes the existing Codex session with invocation-specific context. Codex-side compaction remains part of the same persisted session id from the runtime's perspective, and the invocation prompt carries current-job context on every task.

The runtime loads prompt templates from `prompts.dir` in `config.toml`. Deployments provide the same section filenames under their configured prompt directory. Missing prompt section files and unknown template variables fail prompt construction.

Session bootstrap sections are `base.md`, `clankcord-tools.md`, `response-contract.md`, and `runtime-work.md`. These sections describe Clankcord identity, authority boundaries, the CLI surface, environment variables, response publication, automation workflow, coding-artifact workflow, unsupported-automation feedback submission, web research policy, runtime-work commands, and the interpersonal content publication policy.

Every agent task includes the invocation base sections `agent-task-base.md`, `agent-task-local-context.md`, and `agent-task-response-contract.md`. The runtime adds conditional invocation sections from typed route and origin fields. Voice-channel routes add `agent-task-route-voice.md`. DM routes add `agent-task-route-dm.md`. Typed Discord requests add `agent-task-origin-text.md`; public and managed text surfaces also add `agent-task-origin-public-text.md`. Spoken wake activations add `agent-task-origin-voice.md`.

Prompt section selection uses Rust types before Markdown rendering. The route comes from `AgentSessionRouteKind`, the request origin comes from `AgentPromptRequestOrigin`, and the response surface comes from `TextTargetKind`. The template value map is generated from `AgentTaskPromptVars`, so raw string template names are confined to the renderer boundary.

The invocation templates use `{{job_id}}`, `{{agent_session_id}}`, `{{resumed_from_agent_session_id}}`, `{{route_kind}}`, `{{request_origin}}`, `{{response_surface}}`, `{{guild_id}}`, `{{scope_id}}`, `{{requested_by_user_id}}`, `{{requested_by}}`, `{{request}}`, `{{workdir}}`, `{{recent_scope_events}}`, and `{{source_request_events}}`.

The per-job prompt is compact and stable. It contains job identity, session identity, route kind, request origin, response surface, guild, runtime scope, requester, request text, workdir, recent local scope events, the source request events, and a context note.

```text
JOB:
job_id: ...
agent_session_id: ...
resumed_from_agent_session_id: ...
guild_id: ...
scope_id: ...
requested_by_user_id: ...
requested_by: ...
route_kind: ...
request_origin: ...
response_surface: ...
request: ...

WORKDIR:
CLANKCORD_AGENT_WORKDIR=...

===== RECENT SCOPE EVENTS =====
...

===== CURRENT REQUEST EVENTS =====
...

CONTEXT NOTE:
...
```

The captured context is a bounded local window of user-visible speech and Discord text messages from the task scope. It includes all speakers in that window, split into recent scope events and source request events. The prompt excludes raw job packets, wake internals, audio paths, checksums, provider metadata, token details, and duplicated field aliases.

The context note tells the agent to fetch more history when the request depends on earlier discussion, missing participants, broader scope context, or ambiguous references. Large timeline, transcript, search, and job outputs are written with explicit file output and inspected from the workdir.

The invocation response contract is repeated on every task because resumed Codex sessions receive invocation sections without the session bootstrap sections. It carries the interpersonal content publication policy on every invocation and covers private delivery requests across all routes: a request to DM, direct-message, privately reply, or message a specific private recipient is satisfied by the private delivery itself. After successful private delivery, the agent finishes with `RESPONSE_SUBMITTED` and does not publish a session or channel confirmation unless the user explicitly asks for public acknowledgement. For state-changing work that visibly completes through the command itself, the agent finishes with `NO_RESPONSE_NEEDED`.

Typed requests are treated as intentional Discord text. DM route prompts describe the response as private to the DM participant and direct the agent to answer through the current DM session. Public text prompts keep the answer tied to the visible text surface. Voice-origin prompts describe speech-to-text uncertainty and require a short summary of the understood request before a visible answer.

Agent thread title refresh uses its own prompt template, `agent-thread-title.md`. The template asks Codex for a single Discord forum thread title from `{{agent_session_id}}`, `{{current_thread_title}}`, `{{voice_channel_name}}`, `{{response_count}}`, and `{{responses}}`. It applies the same interpersonal content publication boundary to thread titles. The title invocation uses a `thread_title` agent role and a workspace under `paths.agent_workspaces_root/thread-title/<agent_session_id>`.

## Preflight

Before launching Codex, the task handler checks the process and tool surface expected by the agent. Preflight covers the Codex binary, `rg`, `jq`, `clang`, `python`, `zip`, transcript rendering, transcript search, timeline ranges, conversation listing, context resolution, participant tracing, job inspection, agent-session search, agent-session sunset, agent-session resume, response sending, feedback submission, member resolution, room occupants, automation creation/spec commands, and the coding spec command.

Preflight results are stored with the agent-task metadata. They make tool-surface failures visible in job inspection and the debug dashboard.

## CLI Output

Agent-facing CLI output is JSON. Large read commands support explicit file output.

```text
--file <path>
--format json
```

When `--file` is used, stdout stays small. The CLI writes the JSON file, prints the path, and prints useful counts or window bounds when it can derive them. This lets Codex inspect large results with `jq`, `rg`, and `sed` from the session workdir.

`--ephemeral` and `--verbose` control selection and shape independently. `--ephemeral` includes wake, audio, transient capture, and other ephemeral runtime events. `--verbose` expands the selected records without changing which event classes were selected.

## Member And Room Resolution

Member commands give agents stable Discord identity resolution through Clankcord.

```text
clankcord members search <query> --guild <guild-id> --format json
clankcord members resolve <name-or-id> --guild <guild-id> --format json
clankcord members get <user-id> --guild <guild-id> --format json
```

`members resolve` returns a single user for an unambiguous match. Ambiguous or missing queries return ranked candidates with `resolved: false`. Matching compares Discord user id, username, global name, server nick, display name, and cached speaker labels. Normalization lowercases, strips punctuation and spacing, splits camel case, and scores exact matches, prefix matches, containment, and token overlap.

Member data is cached in Postgres under `discord_members`. The runtime refreshes the cache from Discord when the guild cache is older than one hour or empty, then uses the local cache for search and resolution.

Room occupants are exposed through Clankcord as well.

```text
clankcord rooms occupants <room-or-channel> --guild <guild-id> --format json
```

The room occupant view reads current voice-state rows for human and bot users in the requested Discord voice channel. Automations use the same live occupant list to build `room.liveOccupants` and the `room.participants.<user_id>` condition map.

## Publication Outcome

Codex final text is a control signal. Discord posts are created through Clankcord response commands. A successful task either submits visible output through Clankcord or declares the task complete without publication. The agent task reaches terminal state when the Codex invocation returns successfully and the typing-stop child completes. Missing or mismatched final control text is recorded as a suppressed publication outcome on the terminal agent task.

```text
RESPONSE_SUBMITTED
    one or more text_delivery jobs exist for the source agent task

NO_RESPONSE_NEEDED
    the agent intentionally completed the task without publication
```

Agents use `NO_RESPONSE_NEEDED` for false activations, accidental invocations, read-only checks, no-op work where a visible message adds no useful information, and state-changing commands that already visibly complete the requested work. Other state-changing Clankcord commands require a concise visible response after the command reports success. Session lifecycle commands, automations, room controls, feedback, publication, transcript creation, reminders, and sound playback are state-changing actions. A command that publishes the requested response, such as `clankcord responses send` or `clankcord responses dm`, satisfies the visible-response requirement.

DM requests use `clankcord responses dm --to ...`; the CLI resolves the recipient through the member resolver and creates a DM text-delivery target. Public responses use `clankcord responses send` for the current session surface or an explicit sink. Response bodies are read from stdin by default, or from `--file` when the body already exists as a UTF-8 artifact. `--attachment <ZIP>` carries one or more generated zip files through the same response command. The CLI sends canonical artifact paths to the runtime. `text_delivery` stores only attachment path metadata in Postgres, verifies the files, records filename, size, and checksum on the Discord send child, and the Discord adapter uploads the files with the message through multipart HTTP. The runtime verifies publication by looking for text-delivery jobs tied to the agent task.
