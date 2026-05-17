# Agent Runtime Contract

Agent tasks are Codex process invocations owned by runtime jobs. The runtime chooses the agent session, creates the workspace, builds the prompt, launches Codex, captures the result, stores process metadata, and verifies that visible output was submitted through Clankcord response commands.

```text
agent_task job
      |
      +--> resolve persisted AgentSessionRecord
      +--> prepare workspace and environment
      +--> build prompt and context
      +--> run Codex process
      +--> record output and metadata
      +--> verify response contract
```

## Workspace

Each persisted agent session has a writable directory under `paths.agent_workspaces_root` from `config.toml`. Codex runs with that directory as its current working directory. The directory holds notes, temporary files, command output, and intermediate artifacts. The source checkout is exposed through `CLANKCORD_REPO_DIR`.

Codex sandbox behavior comes from the `[codex]` configuration. `bypass_sandbox = true` makes the runtime pass Codex's explicit sandbox-bypass flag for agent invocations. Docker Compose deployments can set `CLANKCORD_CODEX_BYPASS_SANDBOX=true` in the Compose environment, which overrides the TOML value at runtime. When that environment value is unset or empty, the runtime uses `config.toml`. Installations that rely on Codex's own sandbox leave the value false and use `sandbox` plus `approval_policy` from `config.toml`.

Every invocation receives job and session context through environment variables.

```text
CLANKCORD_AGENT_WORKDIR
CLANKCORD_REPO_DIR
CLANKCORD_AGENT_JOB_ID
CLANKCORD_AGENT_SESSION_ID
CLANKCORD_AGENT_GUILD_ID
CLANKCORD_AGENT_VOICE_CHANNEL_ID
CLANKCORD_AGENT_REQUESTED_BY_USER_ID
CLANKCORD_API_BASE_URL
```

The CLI uses these variables for agent-friendly defaults. `responses send`, `responses ask`, and `responses dm` can infer the current job, guild, channel, and requester from the environment while still accepting explicit flags for manual operation.

## Prompt

The first Codex invocation for an agent session includes the master session instructions. Later invocations in the same Codex session send the per-job prompt. The runtime loads prompt templates from `prompts.dir` in `config.toml`. The current agent task templates are `master.md` and `agent-task.md` under `res/prompts`; deployments can point `prompts.dir` at another directory with files of the same names. Missing template files and unknown template variables fail prompt construction. `agent-task.md` uses `{{job_id}}`, `{{agent_session_id}}`, `{{resumed_from_agent_session_id}}`, `{{guild_id}}`, `{{voice_channel_id}}`, `{{requested_by_user_id}}`, `{{requested_by}}`, `{{request}}`, `{{workdir}}`, `{{previous_context}}`, and `{{question}}`.

The master instructions describe Clankcord authority boundaries, transcript handling for speech-to-text context, the CLI surface, response behavior, private DM handling, automation workflow, unsupported-automation feedback submission, web research policy, and runtime-work commands.

The per-job prompt template is compact and stable. It contains job identity, session identity, guild, voice channel, requester, request text, workdir, previous local context, the wake or question segment, and a context note.

```text
JOB:
job_id: ...
agent_session_id: ...
resumed_from_agent_session_id: ...
guild_id: ...
voice_channel_id: ...
requested_by_user_id: ...
requested_by: ...
request: ...

WORKDIR:
CLANKCORD_AGENT_WORKDIR=...

===== PREVIOUS CONTEXT =====
...

===== QUESTION / ACTIVATION =====
...

CONTEXT NOTE:
...
```

The captured context is a compact five-minute local window of user-visible speech from the same guild and voice channel. It includes all speakers in that window, split into lead-in context and the wake or question segment. The prompt excludes raw job packets, wake internals, audio paths, checksums, provider metadata, token details, and duplicated field aliases.

The context note tells the agent to fetch more history when the request depends on earlier discussion, missing participants, broad room context, or ambiguous references. Large timeline, transcript, search, and job outputs are written with explicit file output and inspected from the workdir.

Agent thread title refresh uses its own prompt template, `agent-thread-title.md`. The template asks Codex for a single Discord forum thread title from `{{agent_session_id}}`, `{{current_thread_title}}`, `{{voice_channel_name}}`, `{{response_count}}`, and `{{responses}}`. The title invocation uses a `thread_title` agent role and a workspace under `paths.agent_workspaces_root/thread-title/<agent_session_id>`.

## Preflight

Before launching Codex, the task handler checks the process and tool surface expected by the agent. Preflight covers the Codex binary, `rg`, `jq`, transcript rendering, transcript search, timeline ranges, conversation listing, context resolution, participant tracing, job inspection, agent-session search, agent-session sunset, agent-session resume, response sending, feedback submission, member resolution, room occupants, and automation creation/spec commands.

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

Codex final text is a control signal. Discord posts are created through Clankcord response commands. A successful task either submits visible output through Clankcord or declares the task complete without publication.

```text
RESPONSE_SUBMITTED
    one or more text_delivery jobs exist for the source agent task

NO_RESPONSE_NEEDED
    the agent intentionally completed the task without publication
```

DM requests use `clankcord responses dm --to ...`; the CLI resolves the recipient through the member resolver and creates a DM text-delivery target. Public responses use `clankcord responses send` for the current session surface or an explicit sink. Response bodies are read from stdin by default, or from `--file` when the body already exists as a UTF-8 artifact. The runtime verifies publication by looking for text-delivery jobs tied to the agent task.
