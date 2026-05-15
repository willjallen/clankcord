# Agent Runtime Context Plan

This document is the implementation plan for reducing agent prompt bloat, improving
agent workspace handling, and making Clankcord CLI outputs safer for Codex context
windows.

## Goals

- Stop sending giant per-job packets to Codex.
- Keep the master prompt, because it is sent once per Codex session.
- Give each Clankcord agent session a writable working directory.
- Run Codex from that working directory so relative temp files are convenient.
- Make per-job prompts small, readable, and focused on user speech.
- Encourage agents to write large command outputs to files explicitly.
- Keep default CLI output compact and human-meaningful.
- Preserve raw/debug detail behind explicit flags.
- Allow the agent to complete jobs without publishing visible responses when that is the
  correct behavior.

## Agent Work Directory

Clankcord should create one writable work directory per Clankcord task session. Use
Clankcord's own session key, not the Codex session id.

Proposed path:

```text
/clankcord/state/agent-workspaces/task/<guild_id>/<voice_channel_id>/
```

This directory is not a security sandbox. The system already runs in Docker. The
directory is simply the agent's working area for notes, temp files, command outputs, and
intermediate artifacts.

Codex should be spawned with this directory as its current working directory. The source
checkout remains available through an environment variable.

Environment variables for every invocation:

```text
CLANKCORD_AGENT_WORKDIR=<agent work directory>
CLANKCORD_REPO_DIR=/workspace/clankcord
CLANKCORD_AGENT_JOB_ID=<current job id>
CLANKCORD_AGENT_GUILD_ID=<current Discord guild id>
CLANKCORD_AGENT_VOICE_CHANNEL_ID=<current Discord voice channel id>
CLANKCORD_AGENT_REQUESTED_BY_USER_ID=<requesting Discord user id>
```

Do not create separate work and artifact directories. One work directory is enough.

## Master Prompt Additions

Keep the existing master prompt, but add durable environment guidance because this prompt
is sent once per Codex session.

Add an `ENVIRONMENT` section:

```text
ENVIRONMENT:
You run from $CLANKCORD_AGENT_WORKDIR, a writable working directory for notes, temp
files, command outputs, and intermediate artifacts. The Clankcord source checkout is at
$CLANKCORD_REPO_DIR.

Current job context is available in CLANKCORD_AGENT_JOB_ID,
CLANKCORD_AGENT_GUILD_ID, CLANKCORD_AGENT_VOICE_CHANNEL_ID, and
CLANKCORD_AGENT_REQUESTED_BY_USER_ID.

For large transcript, timeline, search, or job outputs, prefer explicit file output like
`--file result.json --format json`, then inspect files with jq, rg, and sed. Large files
may be very large; avoid printing them into your conversation context.

Use `clankcord --help` and subcommand `--help` to discover the CLI. For visible
responses, inspect `clankcord responses --help`; prefer `clankcord responses send` and
`clankcord responses dm`.
```

Add response behavior guidance:

```text
RESPONSE BEHAVIOR:
You do not have to publish a visible response for every job.

If the wake word appears to be a false activation, cross-talk, an accidental invocation,
or the captured question is not actually directed at Clankcord, do not respond visibly.
Finish with NO_RESPONSE_NEEDED.

If the user requested a straightforward action where a visible answer would add noise,
perform the action through Clankcord and finish with NO_RESPONSE_NEEDED unless the action
failed or the user clearly expects confirmation.

If a user asks you to DM them about something, treat the request and the answer as
private. Use `clankcord responses dm` for the substantive response, and do not publish
the topic, answer, summary, result, or confirmation to a public channel unless the user
explicitly asks for public disclosure.

If you do publish a visible response, use `clankcord responses send` or
`clankcord responses dm`. After successful submission, finish with RESPONSE_SUBMITTED.
```

This replaces any instruction that the agent must always submit to `--sink agent-chat`.

## Remove Per-Job Packets

Delete the packet-based agent prompt path:

- No `*.packet.json` files.
- No `JOB_PACKET_JSON` section in prompts.
- No generated `tools` section.
- No packet `schema`.
- No packet `manuals`.
- No packet `policy`.
- No raw activation JSON in the prompt.

The per-job prompt should be generated directly from the job, recent transcript context,
and current environment.

## Per-Job Prompt Shape

Per-job prompts should be compact and mostly plain text.

Example:

```text
JOB:
job_id: job_87af41a9c6c54544b377c9cfd15f55ca
guild_id: 553018603226529802
voice_channel_id: 1204188344993447956
requested_by_user_id: 218519280235446272
requested_by: will
request: the difference between SSE 4.1 and 4.2?

WORKDIR:
CLANKCORD_AGENT_WORKDIR=/clankcord/state/agent-workspaces/task/553018603226529802/1204188344993447956

===== PREVIOUS CONTEXT =====
[2026-05-15T01:28:10Z] vince (123): ...
[2026-05-15T01:29:02Z] will (218519280235446272): ...

===== QUESTION / ACTIVATION =====
[2026-05-15T01:32:49Z] will (218519280235446272): Hey Clanky
[2026-05-15T01:32:53Z] will (218519280235446272): Can you tell me the difference between SSE 4.1 and 4.2?

CONTEXT NOTE:
The transcript above is only a compact 5-minute local window. If the request appears to
depend on earlier discussion, missing speaker turns, prior channel context, or broader
history, use Clankcord CLI commands to search or render more user messages before
answering. Prefer writing large outputs to files with `--file <name> --format json` and
inspect them from your workdir.
```

## Captured Message Window

Use a 5-minute default captured-message window for agent tasks.

The default per-job prompt should include compact user-visible speech from the same
guild and voice channel from:

```text
activation_or_job_time - 5 minutes
through the captured post-wake question end
```

Include all speakers in that channel window, not only the requester.

Split the rendered transcript into:

- `===== PREVIOUS CONTEXT =====`: the 5-minute lead-in before the wake/question.
- `===== QUESTION / ACTIVATION =====`: the wake-triggering utterance and post-wake
  captured messages.

Each message line should include only:

- timestamp
- speaker label
- speaker user id
- text

Do not include:

- `job_created` events
- wake internals
- playback, mute, join, or control events
- audio paths
- audio checksums
- STT token metadata
- token logprobs
- raw payload JSON
- duplicated snake_case and camelCase aliases

## Agent Context-Seeking Behavior

The agent should be strongly instructed that the 5-minute prompt context is not
authoritative full history. If the task appears to rely on prior conversation, broader
room context, missing participants, or ambiguous references, the agent should use the
Clankcord CLI to fetch more context before answering.

The desired behavior is:

- Use the compact prompt for straightforward requests.
- Search or render more user messages when the prompt context is insufficient.
- Prefer explicit file output for large data.
- Inspect large files with focused tools instead of printing them into chat context.

## CLI Output Semantics

Fix overloaded `--verbose` behavior.

New semantics:

```text
--ephemeral
    Include wakeword, audio segment, transient capture, and other ephemeral runtime
    events.

--verbose
    Include expanded fields and richer detail for the selected records. This should not
    change which event classes are selected.

--file <path>
    Write the command result to a file selected by the caller.

--format json
    Output structured JSON. JSON is the only structured format needed.
```

No `jsonl` or markdown output format is needed for this pass.

Default command output should be compact. It should not include raw payloads, token
logprobs, audio metadata, repeated aliases, or ephemeral events unless the user opts in.

This applies especially to:

- `clankcord timeline range`
- `clankcord timeline tail`
- `clankcord transcripts render`
- `clankcord transcripts search`
- `clankcord messages read`
- `clankcord messages search`
- `clankcord jobs get`

Commands should not write files by default. File output must be explicit through
`--file`.

When `--file` is used, stdout should stay small:

```text
Wrote JSON to transcript.json
Records: 184
Window: 2026-05-15T01:00:00Z to 2026-05-15T01:35:00Z
```

## Help Text

Large-output commands should mention file output in help text:

```text
Large windows can be written with --file <path> --format json. Agents should prefer
writing large outputs to files in $CLANKCORD_AGENT_WORKDIR and inspecting them with
jq, rg, and sed.

Use --ephemeral for wake/audio/transient events. Use --verbose for expanded fields.
```

## Member And Room Resolution

Agents need ergonomic ways to resolve Discord users and current room occupants. They
should not have to grep for bot tokens, call Discord APIs directly, inspect Postgres, or
infer live voice state from transcript artifacts.

Add a durable Discord member cache in the Postgres timeline store:

- Cache all members for configured guilds.
- Refresh infrequently, initially once per hour.
- Store user id, username, global name, server nick, display name, avatar metadata if
  useful, and `updated_at`.
- Refresh opportunistically from Discord gateway/member events when available.
- Use the cache for CLI member lookup first.
- If a direct Discord API lookup is needed, keep that behind the Clankcord command
  surface, not agent-written `curl`.

Add member CLI commands:

```text
clankcord members search <query> --guild <guild-id> --format json
clankcord members resolve <name-or-id> --guild <guild-id> --format json
clankcord members get <user-id> --guild <guild-id> --format json
```

`members resolve` should return one resolved user only when the match is unambiguous.
Otherwise it should return ranked candidates and `resolved: false`.

Resolution should compare against:

- Discord user id
- username
- global name
- server nick
- display name
- cached voice/transcript speaker labels

Normalization should:

- lowercase
- strip spaces
- strip punctuation, underscores, and hyphens
- split camel case
- compare full normalized strings and token overlap

Example: `Mystery Man Chien` should strongly match `mysterymanchien` /
`MysteryManChien` because both normalize to `mysterymanchien`.

Example resolved output:

```json
{
  "query": "Mystery Man Chien",
  "resolved": true,
  "confidence": "high",
  "user": {
    "id": "284362763386617857",
    "username": "mysterymanchien",
    "global_name": "MysteryManChien",
    "nick": null,
    "display_name": "MysteryManChien"
  },
  "candidates": []
}
```

Example ambiguous output:

```json
{
  "query": "mystery",
  "resolved": false,
  "reason": "ambiguous",
  "candidates": [
    {
      "id": "284362763386617857",
      "display_name": "MysteryManChien",
      "score": 0.91
    },
    {
      "id": "123",
      "display_name": "MysteryGuest",
      "score": 0.86
    }
  ]
}
```

Add current room occupancy commands:

```text
clankcord rooms occupants <room-or-channel> --guild <guild-id> --format json
clankcord rooms status <room-or-channel> --guild <guild-id> --format json
```

Clankcord should retain current guild voice states for non-bot users, not only voice bot
state. Room status should show who is currently sitting in each configured voice room,
including users in rooms where no Clankcord voice bot is assigned.

## Response Commands

Make response commands job-context aware through the agent environment. The agent should
not have to pass `--job` on the happy path when Clankcord already provides
`CLANKCORD_AGENT_JOB_ID`.

Preferred agent commands:

```text
clankcord responses send --sink agent-chat --stdin
clankcord responses send --sink dm:<user-id> --stdin
clankcord responses dm --to "Mystery Man Chien" --stdin
```

The CLI should fill source job, guild, channel, and requester from:

```text
CLANKCORD_AGENT_JOB_ID
CLANKCORD_AGENT_GUILD_ID
CLANKCORD_AGENT_VOICE_CHANNEL_ID
CLANKCORD_AGENT_REQUESTED_BY_USER_ID
```

Explicit flags such as `--job`, `--guild`, `--channel`, and
`--requested-by-user-id` can remain as admin/manual overrides, but they should not be
required for agent execution.

`responses dm --to <name-or-id>` should use the member resolver. If resolution is
ambiguous, it should fail with ranked candidates instead of guessing.

## Dashboard And Debug

Agent debug views should show the workdir without dumping its contents into the session
trace.

Useful debug fields:

- workdir path
- file list
- file size
- mtime
- small text preview for small files only

Cumulative session text should include prompts, visible messages, and concise job
summaries. It should not embed large workspace files.

## Implementation Targets

Primary code areas:

- `clankcord/src/runtime/domain/interactions/tasks.rs`
- `clankcord/src/runtime/agents/*`
- `clankcord/src/cli.rs`
- `clankcord/src/runtime/views/*`
- `clankcord/src/runtime/timeline/*`
- `clankcord/src/dashboard/*`
- Discord member cache storage and refresh logic
- response CLI helpers

Expected removals from `tasks.rs`:

- `AgentTaskPacket`
- `AgentTaskTools`
- `AgentTaskManuals`
- packet JSON serialization
- packet file writing
- `JOB_PACKET_JSON` prompt section

Expected additions:

- workdir creation
- Codex cwd set to workdir
- current-job environment variables
- compact message rendering for the 5-minute prompt window
- prompt section markers for previous context and question activation
- response behavior guidance in the master prompt

## Tests

Add tests for:

- Workdir path uses Clankcord guild/channel session key, not Codex session id.
- Codex invocation cwd is the agent workdir.
- Master prompt documents the agent env vars.
- Master prompt tells agents to use `clankcord --help` and response send/dm help.
- Master prompt allows `NO_RESPONSE_NEEDED`.
- Per-job prompt contains `===== PREVIOUS CONTEXT =====`.
- Per-job prompt contains `===== QUESTION / ACTIVATION =====`.
- Per-job prompt includes the 5-minute context window.
- Per-job prompt includes all speakers in the same channel window.
- Per-job prompt has no packet JSON, schema, tools, manuals, or policy.
- Per-job prompt excludes job events, wake internals, logprobs, audio metadata, and raw
  payload JSON.
- No packet file is written for agent tasks.
- Default timeline/job output excludes ephemeral events.
- `--ephemeral` includes wake/audio/transient events.
- `--verbose` expands fields without changing event class inclusion.
- `--file --format json` writes JSON and keeps stdout compact.
- Member cache refresh stores guild members in Postgres.
- `members resolve "Mystery Man Chien"` resolves `MysteryManChien` by normalized exact
  match.
- `members resolve` returns candidates instead of guessing on ambiguous matches.
- `rooms occupants art-lounge` reports non-bot users sitting in that voice room.
- `responses send` uses current agent job env without requiring `--job`.
- `responses dm --to <name-or-id>` resolves the recipient and submits a DM response job.
