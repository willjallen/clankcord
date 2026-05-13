# Clanky Voice Memory and Codex Agent Plan

Status: living design document  
Last updated: 2026-05-12  
Audience: someone who has not heard the previous voice discussion

## 0. Executive Summary

Clanky is intended to become an always-available Discord voice memory and action surface. The product problem is simple: useful work conversations often become important only after they have already been going for ten minutes, forty minutes, or an hour. Inviting a bot only after realizing "this should have been recorded" loses the crucial context.

The desired system is not a traditional recorder. It is a trustworthy ambient memory layer:

1. Clawcord-owned voice bots can sit in configured Discord voice rooms.
2. They locally capture draft transcript events and source audio into multi-day ephemeral corpora.
3. Each voice channel has its own contiguous annotated timeline.
4. Multiple channel timelines can run in parallel.
5. Conversations, summaries, transcripts, Linear issues, fact-checks, and Discord artifacts are derived from selected windows of those timelines.
6. Nothing becomes a permanent Discord transcript unless a user asks for it.
7. When a transcript is made permanent, the rough local STT should be replaced by a high-quality transcription from the corresponding mixed source audio, initially via ElevenLabs.
8. Codex should provide agent orchestration, model routing, hooks, jobs, and tool access.
9. Clawcord should remain the source of truth for Discord voice capture, Discord publishing, voice bot assignment, transcript storage, and Discord-specific control.

The most important architectural rule is:

> Build the boring multi-channel timeline substrate first. Then make Codex agents elegant dispatchers over stable Clawcord primitives.

The most important Codex rule is:

> Do not create many cron-driven agents that repeatedly read transcript text. Use deterministic event hooks, cheap wake/command detection, cached cursors, and job-scoped strong agents only when needed.

The most important Discord topology rule is:

> The Codex-native `Clanky` bot and the Clawcord-owned voice bots `clanky-vc1`, `clanky-vc2`, and future `clanky-vcN` bots are different actors with different responsibilities. Do not blur them.

## 1. Discord Bot Topology

This is a hard architectural constraint, not an implementation detail.

There are currently three Discord bot identities:

1. **`Clanky`**
    - Codex-native Discord bot.
    - Text/agent interaction surface.
    - Handles agent-chat replies, confirmations, worker results, summaries, issue proposals, status replies, and general Codex-native Discord behavior.
    - Lives in the Discord text interface, especially the dedicated `agent-chat` channel.
    - Should not be treated as the voice capture bot.
    - Should not be moved between voice channels as part of Clawcord's capture pool.

2. **`clanky-vc1`**
    - Clawcord-owned voice capture bot.
    - Joins Discord voice channels.
    - Receives voice packets.
    - Feeds per-speaker audio into Clawcord.
    - Emits local STT transcript events into the voice timeline.
    - May participate in live transcript publication through Clawcord-controlled Discord operations.
    - Is not an Codex agent.

3. **`clanky-vc2`**
    - Same role as `clanky-vc1`.
    - Exists because Discord only allows one bot account to be in one voice channel at a time.
    - Enables a second voice channel to be captured in parallel.

Future scaling should use the same pattern:

- `clanky-vc3`
- `clanky-vc4`
- etc.

The voice capture design should treat these as a pool of N Clawcord-controlled capture identities.

## 2. Why the Three-Bot Split Matters

Discord voice has a hard constraint: one bot identity cannot be in multiple voice channels at the same time. If the team uses multiple voice channels in parallel, one voice bot cannot cover them all.

Therefore the voice capture layer must be modeled as a **Clawcord voice bot pool**, not as a single global Clanky session.

This creates several requirements:

- Timelines are per voice channel, not per bot.
- Capture runs record which bot identity captured them.
- A voice bot can move between channels over time.
- A channel timeline may have multiple capture runs captured by different bot identities across days.
- Codex should not directly manage `clanky-vc1`/`clanky-vc2` as agents.
- Clawcord owns voice bot assignment, presence, audio capture, and low-level Discord behavior.
- Codex receives events and creates jobs through Clawcord APIs.
- Results from Codex should generally be posted by `Clanky` in `agent-chat` or by Clawcord in transcript publication surfaces, depending on the interaction.

The conceptual split:

```text
Discord text / agent surface:
  Clanky
    owned by Codex
    speaks in text channels
    handles confirmations, responses, worker results

Discord voice capture pool:
  clanky-vc1
  clanky-vc2
  clanky-vc3...
    owned by Clawcord
    join voice channels
    capture audio
    create transcript events
    publish live transcripts when asked
```

Do not describe `clanky-vc1` or `clanky-vc2` as Codex agents. They are Discord bot identities controlled by Clawcord.

## 3. Product Thesis

Clanky should feel like:

> "The useful part of the conversation was not lost."

The system should support:

- "Clanky, start a live transcript from ten minutes ago."
- "Clanky, pull up what we said an hour ago."
- "Clanky, make this conversation permanent."
- "Clanky, materialize the fixed-point discussion."
- "Clanky, fact-check what Vince just said."
- "Clanky, propose Linear issues from the last twenty minutes."
- "Clanky, forget that."
- "Clanky, pause for twenty minutes."
- "Clanky, get out of here."
- Text command in `agent-chat`: "Clanky, send a VC bot to Code Lounge."

The product should not require users to know whether they need a transcript, a summary, a fact-check, a Linear issue draft, or an artifact before the conversation begins. They should be able to ask after the fact, using natural time/topic references.

## 4. Context and Motivation

### 4.1 What Clanky Does Today

Clanky is a Discord-integrated bot/agent system. It can participate in text channels, interact with prior transcripts, and use Codex-style agent infrastructure. There is also Clawcord, which is the custom Discord integration layer. Clawcord exists because generic Discord harnesses are not good enough for the Discord-specific behavior needed here.

The current voice transcription direction already has useful pieces:

- Discord voice packets can be captured.
- Audio can be buffered per speaker.
- Local speech-to-text can produce rough transcript events.
- There is a manager pipeline that can publish transcript-like artifacts.
- There is agent infrastructure available through Codex.
- Discord is already the team-facing place where transcripts and agent messages should appear.
- There are two Clawcord-owned voice capture bot accounts, `clanky-vc1` and `clanky-vc2`, to support parallel voice channels.

The problem is that the old mental model is too close to:

> join voice room → start a transcript session → create Discord transcript thread → stream/finalize artifacts → stop

That is not the product we want.

The new model should be:

> Clawcord assigns an available voice bot to a room → capture local ephemeral channel timeline → later materialize selected windows → publish/refine only when requested → let Codex agents act on selected context

### 4.2 Why Inviting Clanky Late Fails

Useful conversations often start casually:

- complaining about a bug
- arguing about an implementation detail
- venting about a design problem
- explaining a half-formed idea
- pushing back on a decision
- talking through a system from first principles

Only later does the conversation become obviously valuable. By then, a transcript starting at "now" is missing the setup, assumptions, counterarguments, and the moment where the problem was framed.

Therefore the command should not be:

> "Clanky, record now."

It should often be:

> "Clanky, start the transcript from ten minutes ago."

or:

> "Clanky, pull up what we said an hour ago."

or:

> "Clanky, make this whole conversation permanent."

That requires more than a 30-minute RAM buffer. It requires a multi-day ephemeral transcript/audio corpus.

### 4.3 Why This Must Still Feel Trustworthy

An always-present voice memory can easily feel Orwellian if the product contract is wrong. The goal is not to permanently record everything. The goal is to preserve recoverable local context long enough that useful work can be materialized intentionally.

Trust should come from:

- visible state: deafened, locally buffering, live transcript active, refining, paused
- clear retention policy
- explicit promotion into Discord
- ability to pause/deafen/forget unpublished context
- participant-aware permissions where needed
- clear distinction between draft and refined transcript quality
- no hidden Discord publication
- no agent silently acting on huge private transcript windows without a command

Do not claim "this is unrecoverable" while also intentionally retaining multiple days. The honest contract is:

> Clanky keeps unpublished local voice memory for a bounded time, and users can see/control when that memory becomes permanent.

## 5. Closed Policy Decisions

This section closes the earlier open questions. These are the defaults unless later changed by explicit configuration.

### 5.1 Voice Bot Auto-Join Policy

Clawcord voice bots should eagerly auto-join only these configured lounges:

- `Art Lounge`
- `Code Lounge`
- `Environment Lounge`

A configured lounge becomes eligible for auto-capture when:

- effective human count is at least 2, and
- a Clawcord voice bot is available, and
- the channel is not in cooldown or explicitly disabled.

Effective human count means:

```text
non-bot members in the voice channel who are not deafened
```

Muted users still count. Deafened users do not count. This matches the intended heuristic: if six people are present but four are deafened and two are muted, the system treats the room as having two effective people.

Other voice rooms are not eagerly auto-buffered. They can still be captured by explicit request through the main `Clanky` text interface in `agent-chat`, or by an admin/operator command.

### 5.2 On-Demand Capture Policy

Any Discord user in the server may ask `Clanky` in text to send a VC bot to a room.

Examples:

- "Clanky, send a VC bot to Code Lounge."
- "Clanky, start listening in Art Lounge."
- "Clanky, bring clanky-vc here."

If a VC bot is available, Clawcord assigns it. If no VC bot is available, `Clanky` replies in `agent-chat` with capacity status.

### 5.3 Capacity Policy

With `clanky-vc1` and `clanky-vc2`, the system can capture at most two voice channels in parallel.

If more than two rooms request capture:

- do not silently preempt an active capture
- do not move a voice bot without explicit admin force-move
- reply in `agent-chat` with "no spare VC bot" and show where the current bots are
- future scaling should add `clanky-vc3`, `clanky-vc4`, etc.

Example capacity-full reply:

```text
No spare Clanky VC bot is available.

clanky-vc1: Code Lounge · locally buffering
clanky-vc2: Art Lounge · live transcript active

An admin can force-move one with:
clawcord voice pool move --bot clanky-vc1 --to <channel>
```

### 5.4 Release and Anti-Flapping Policy

Voice bots should not constantly join and leave. That is distracting.

Defaults:

- Minimum dwell after auto-join: 5 minutes.
- Auto-release after no speech activity for 10 minutes, if not live-publishing and no materialization/refinement job needs the live capture.
- Auto-release after effective human count drops below 2 for 5 minutes.
- Paused/deafened rooms release after 20 minutes if no one resumes.
- After auto-release, apply a 10-minute auto-join cooldown for that channel unless a user explicitly requests capture.
- Explicit text request bypasses cooldown.
- Admin force-move bypasses cooldown.

No speech activity means no transcript events with speech content, not merely "everyone is muted" as a Discord state. Discord mute/deafen state helps with heuristics, but the source of truth for "room is doing work" is recent speech activity.

### 5.5 Admin Force-Move Policy

Admins may force-move a voice bot.

Force move should:

1. stop/pause the old channel capture run cleanly
2. write a bot release event to the old channel timeline
3. write an assignment event to the new channel timeline
4. start a new capture run in the new channel
5. post a status message in `agent-chat`

Admin force-move should be a Clawcord operation exposed to Codex as a tool/command.

### 5.6 Retention Policy

Default ephemeral retention:

- draft transcript events: 7 days
- source audio: 7 days
- job metadata: 30 days
- permanent transcript artifacts: Discord/durable publication policy

Source audio retention should equal draft transcript retention by default. If a draft transcript exists but source audio is gone, the system loses the ability to do high-quality refinement; avoid that mismatch in the normal case.

### 5.7 Publication Permission Policy

Any Discord server member may request that a transcript be made permanent.

When a permanent transcript is being rendered, `Clanky` should announce it in the appropriate Discord text surface and ping everyone currently in the relevant voice room.

Clanky should not announce mere local buffering when a VC bot joins.

### 5.8 Forget Permission Policy

Any Discord server member may request deletion/tombstoning of unpublished local context.

"Forget" is only clean before publication. If content has already been posted to Discord, the system may delete/edit/withdraw what it can, but it must not imply perfect erasure.

### 5.9 Absent Participant Policy

If a materialized window includes participants who are no longer present, there is no special blocking behavior by default.

No absent-participant approval gate. No special warning. The normal publication and permanent transcript announcement policy applies.

### 5.10 Storage Format Policy

Use SQLite as the primary transcript/timeline substrate.

JSONL is acceptable as a generated export/debug format, but it should not be the primary store for multi-day ephemeral transcripts. The local corpus needs fast range scans, speaker/time filters, job lookups, publication state, retention cleanup, and transcript text search. SQLite gives that without adding a separate database service.

Acceptable v1 storage:

- SQLite for capture runs, transcript events, conversations, publications, jobs, occupancy, and command routing state
- SQLite FTS5 for transcript text search
- filesystem paths for source audio segments and generated artifacts
- generated Markdown/TXT/JSONL only for Discord publication, exports, diagnostics, or repair snapshots
- bounded debug/provider payload storage only when useful, preferably in separate tables with shorter retention

There is no requirement to backfill or migrate the existing JSONL corpus. The current JSONL data is disposable and may be blown away during the transition.

The schema should reduce per-snippet metadata. A transcript event row should carry repeated timeline fields in indexed SQLite columns, with `payload_json` limited to segment-specific extras. Future normalization can split speakers/audio segments into separate tables if volume justifies it, but v1 should not repeat large compatibility payloads on every speech segment.

### 5.11 ElevenLabs Refinement Policy

Use mixed audio for ElevenLabs refinement, not per-speaker stems.

Reason:

- billing is based on audio duration
- per-speaker stems multiply cost because each stem has the full window duration and is mostly silence
- mixed audio means a 30-minute conversation is billed as 30 minutes, not 5 speakers × 30 minutes

The first implementation should:

- generate one mixed mono audio file for the selected window
- call ElevenLabs Speech-to-Text with diarization enabled
- request word-level timestamps
- use the returned timing and speaker ids to heuristically align ElevenLabs speakers to known Discord speakers
- store the alignment confidence and allow later repair

### 5.12 Cross-Channel Scope Policy

Commands originating in a voice channel are channel-local by default, but the system must not be rigidly single-channel.

Default behavior:

- "this conversation" = current originating channel
- "last 20 minutes" from voice = current originating channel
- "what Vince just said" = current originating channel
- "from 1pm to 2pm" without a channel qualifier = all relevant parallel timelines in the guild for that absolute time range
- "everywhere we talked about Lumen" = cross-channel transcript search
- text commands in `agent-chat` may naturally be broader and should ask/resolve scope if ambiguous

Important nuance:

A person may hop from one channel to another for five minutes to ask a question, then return. That short excursion may be relevant to the original conversation. The system should preserve participant movement events and let the worker/context resolver follow those context clues when asked.

Do not overbuild cross-channel conversation reconstruction in v1, but do design the timeline and search APIs so this is possible.

## 6. Main Design Principle: Parallel Annotated Timelines

### 6.1 The Timeline Is the Primary Substrate

All captured voice context should become a contiguous annotated timeline per guild/voice-channel pair. This timeline is the substrate from which everything else is derived.

The timeline contains events such as:

- voice bot assigned to channel
- participant joined/left voice channel
- speaker audio segment captured
- local STT draft text produced
- conversation boundary assigned
- command detected
- transcript window selected
- live transcript publication created
- high-quality refinement completed
- Discord message updated
- ephemeral data retired
- voice bot released from channel

A transcript is not a separate world. A conversation is not a separate world. A Linear issue proposal is not a separate world. They are all artifacts derived from selected windows of one or more channel timelines.

### 6.2 Timelines Are Per Channel, Not Per Bot

Because `clanky-vc1`, `clanky-vc2`, and future `clanky-vcN` bots are a pool, the bot identity is not the timeline identity.

Correct:

```text
timeline key = guild_id + voice_channel_id
capture run = a period within that channel timeline, captured by clanky-vc1 or clanky-vc2
```

Incorrect:

```text
timeline key = clanky-vc1
timeline key = clanky-vc2
```

A voice bot is just the capture worker assigned to a channel at a point in time.

### 6.3 Raw Audio, Draft STT, and Refined STT

There are three relevant truth layers:

1. **Source audio**: the deepest truth if retained.
2. **Refined transcript spans**: authoritative text for materialized windows after high-quality transcription.
3. **Draft local STT events**: provisional text for everything not yet refined.

The caveat is important:

> Once a section has been materialized and refined by ElevenLabs or the configured high-quality provider, that refined transcript should become the source of truth for that section's text.

That does not mean deleting the raw timeline. It means the query/materialization layer should prefer refined transcript spans over draft local STT for any covered interval.

### 6.4 The Timeline Should Use an Overlay Model

Do not destructively replace raw draft events with refined transcript text. Instead, add refined transcript overlays.

For example:

```json
{
  "authoritative_span_id": "span_01J...",
  "kind": "refined_transcript",
  "provider": "elevenlabs",
  "window_id": "win_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "start_time": "2026-05-11T20:12:04.000Z",
  "end_time": "2026-05-11T20:47:18.000Z",
  "covers_event_ids": ["evt_001", "evt_002", "evt_003"],
  "text_artifact_path": "durable/publications/pub_01J/transcript.refined.txt",
  "speaker_alignment_path": "durable/publications/pub_01J/speaker_alignment.json",
  "quality": "refined",
  "created_at": "2026-05-11T21:05:00.000Z"
}
```

Transcript retrieval follows this rule:

- If a requested window intersects a refined span, use refined text for that covered part.
- If a requested window includes unrefined gaps, use draft local STT for those gaps.
- If source audio still exists and the user asks to make a draft window permanent, queue refinement.
- If source audio is gone, clearly mark the transcript as draft-only/unrefinable.

This gives the best of both worlds:

- raw event history remains debuggable
- refined transcripts become the authoritative text
- existing derived artifacts can point to the refined source
- future searches use the better transcript where available
- alignment errors can be repaired without destroying old data

### 6.5 Conversation Objects Are Views, Not Truth

A conversation is a derived grouping over timeline events. It may be based on silence gaps, participants, topic labels, and semantic continuity. It can be wrong. It must be recomputable.

The timeline is truth. Conversation ids are convenience handles.

## 7. Voice Bot Pool Management

### 7.1 Voice Bot Pool

Clawcord should own a `VoiceBotPool` containing available voice bot identities.

Initial pool:

```json
[
  {
    "bot_id": "clanky-vc1",
    "discord_user_id": "...",
    "state": "available"
  },
  {
    "bot_id": "clanky-vc2",
    "discord_user_id": "...",
    "state": "available"
  }
]
```

Future pool:

```json
[
  "clanky-vc1",
  "clanky-vc2",
  "clanky-vc3",
  "clanky-vc4"
]
```

Each bot can be assigned to at most one voice channel at a time.

### 7.2 Voice Channel Assignment

When a channel should be captured, Clawcord creates or updates an assignment:

```json
{
  "assignment_id": "assign_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "voice_channel_name": "Code Lounge",
  "voice_bot_id": "clanky-vc1",
  "voice_bot_discord_user_id": "...",
  "capture_run_id": "cap_01J...",
  "state": "capturing",
  "mode": "local_buffering",
  "assigned_at": "2026-05-11T20:12:00Z",
  "released_at": null,
  "assignment_reason": "auto_join_effective_humans_ge_2"
}
```

This assignment is operational state. It is not the transcript. The transcript timeline is still keyed by guild/channel.

### 7.3 Assignment States

Suggested states:

- `available`
- `reserved`
- `joining`
- `capturing`
- `deafened_paused`
- `live_transcript_active`
- `releasing`
- `failed`
- `offline`

### 7.4 Capture Run Includes Bot Identity

Every capture run should record which voice bot captured it:

```json
{
  "capture_run_id": "cap_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "voice_channel_name": "Code Lounge",
  "voice_bot_id": "clanky-vc1",
  "voice_bot_discord_user_id": "...",
  "started_at": "2026-05-11T20:12:00Z",
  "ended_at": null,
  "state": "active",
  "mode": "local_buffering"
}
```

This matters for debugging:

- Did `clanky-vc1` have packet loss?
- Did `clanky-vc2` produce bad audio?
- Was a speaker attribution bug specific to one bot session?
- Did a capture gap happen because no voice bot was available?

### 7.5 Effective Occupancy

Clawcord should continuously compute an effective occupancy snapshot per voice channel:

```json
{
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "actual_human_count": 6,
  "effective_human_count": 2,
  "muted_human_count": 2,
  "deafened_human_count": 4,
  "non_deafened_human_ids": ["user_a", "user_b"],
  "last_speech_at": "2026-05-11T20:12:00Z"
}
```

Rules:

- bots do not count
- deafened humans do not count
- muted humans count
- last speech activity controls release after inactivity

### 7.6 Capacity Full Behavior

If a third room needs capture while both bots are in use:

1. Do not auto-evict.
2. Write/emit a capacity-full event.
3. `Clanky` replies in `agent-chat`.
4. Provide current bot assignments.
5. Mention admin force-move option.

This is preferable to surprising users by moving a voice bot away from an active room.

## 8. Vocabulary

### Clanky

The Codex-native Discord bot. Text and agent surface.

### Clawcord Voice Bot

A Discord bot identity controlled by Clawcord for voice capture, currently `clanky-vc1` and `clanky-vc2`.

### VoiceBotPool

The Clawcord-managed pool of voice capture bot identities.

### VoiceChannelAssignment

The current binding between one Clawcord voice bot and one Discord voice channel.

### Channel Timeline

The contiguous annotated timeline for a guild/voice-channel pair.

### Capture Run

A continuous period where one Clawcord voice bot is assigned to one Discord voice channel and capable of receiving audio.

A capture run may produce:

- zero published artifacts
- many conversation views
- many materialized transcript windows
- many command detections
- many jobs

### Transcript Event

A timestamped local STT event tied to:

- speaker identity
- voice channel
- source audio path
- draft text
- timing
- capture run id
- voice bot id
- provisional conversation id

### Ephemeral Corpus

The unpublished local corpus of transcript events, source audio, conversation views, and job state. "Ephemeral" means unpublished and retired by policy, not necessarily short-lived.

### Permanent Transcript

A transcript promoted into Discord's team-facing transcript surface and retained as a durable artifact. Permanent transcripts should prefer refined STT.

### Live Transcript

A Discord-visible draft transcript that can be started now or retroactively from a selected recent window. It streams draft local STT first and is later refined if made permanent or if refinement is requested.

### Transcript Window

A selected time/topic/conversation slice from one or more channel timelines.

Examples:

- last 10 minutes in Code Lounge
- 1:00pm to 1:45pm across all active voice timelines
- current conversation in this room
- conversation `conv_01J...`
- the fixed-point discussion
- what Vince just said
- the five-minute question Will asked in Art Lounge before returning to Code Lounge

### Publication

The Discord-facing state for a materialized transcript window:

- thread id
- message chunks
- draft/refined state
- attached artifacts
- recording artifact
- refinement job id

### Codex Router

A very cheap command detection agent/process that looks at a rolling recent transcript window only when there is a wake-word/command candidate.

### Codex Worker

A strong, job-scoped agent that acts on selected transcript windows. It has no ambient cron. It exists only because a command/job asks for involved work.

### Refinement Worker

A background job runner, mostly not an LLM agent, that submits source mixed audio to ElevenLabs or equivalent and updates authoritative transcript spans.

## 9. Chosen Codex Integration Model

This section makes a concrete decision instead of leaving "hooks vs cron" ambiguous.

### 9.1 Chosen Pattern

Use an Codex plugin-style integration named:

```text
clawcord_voice
```

This plugin exposes:

1. an event bridge from Clawcord to Codex
2. a small set of Codex tools backed by Clawcord CLI/API
3. a job queue bridge for strong worker invocations
4. a model-call helper for cheap router classification

Codex should not run free-form autonomous cron agents for voice. Clawcord should run deterministic schedulers for pool/retention/status, and Codex should be invoked only through events/jobs.

### 9.2 Config Location

Use one voice integration config file:

```text
host/linux/codex/config/clanky_voice.yaml
```

Use separate agent prompt files:

```text
host/linux/codex/config/seeds/agents/clanky-voice-router/AGENTS.md
host/linux/codex/config/seeds/agents/clanky-voice-worker/AGENTS.md
```

The maintainer and refinement worker should be configured as Clawcord/Codex job routines, not normal chat agents.

If Codex's actual config system requires a different file shape, implement a thin adapter that reads this desired config and registers the equivalent hooks/tools/jobs. Do not change the architecture to fit a bad cron-based default.

### 9.3 Event-Driven Hooks, Not Cron Agents

Use hooks for:

- transcript event candidate
- router command detected
- confirmation approved
- job created
- publication created
- refinement completed
- voice bot assignment changed

Use deterministic scheduled jobs for:

- voice bot pool watchdog
- retention sweep
- job timeout sweep
- status refresh

The scheduled jobs should not read transcript text unless triggered by a concrete boundary/delta event.

### 9.4 Job-Scoped Worker Invocation

Clawcord invokes strong Codex work by creating a voice job and then calling Codex with a compact job packet.

Canonical invocation:

```bash
codex exec \
  --json \
  --output-last-message /path/to/job_result.txt \
  -
```

The job packet contains IDs and bounded instructions, not a giant transcript blob. The worker then uses Clawcord tools to fetch exactly the transcript window it needs.

Clawcord owns the session lane and passes the prompt on stdin. The Codex adapter captures stdout/stderr and the final message; the runtime agent harness records the session id for later reuse.

### 9.5 Router Invocation

The router should be a cheap model call inside the `clawcord_voice` plugin, not a full autonomous agent session.

Canonical invocation:

```bash
codex exec \
  --json \
  --output-last-message /path/to/router_result.json \
  -
```

The router returns JSON only. Clawcord validates it and emits a command event. The router is stateless from Clawcord's perspective; worker jobs use sticky Codex sessions, but router classification does not.

### 9.6 Tool Permission Model

Use least privilege per component.

Router:

- can read channel status
- can emit command detection
- can request confirmation
- cannot read full transcripts
- cannot write arbitrary Discord messages
- cannot access Linear/GitHub/web

Worker:

- can render bounded transcript windows
- can search transcripts within declared scope
- can publish through Clawcord publication APIs
- can use Linear/GitHub/web/Notion when job kind allows
- cannot access raw voice corpus files directly
- cannot create Linear issues without approval

Maintainer:

- can update pool/status/conversation metadata
- can apply retention
- cannot call external research/tools
- usually uses no model

Refinement worker:

- can export audio windows
- can call ElevenLabs
- can create authoritative spans
- can update publication artifacts
- cannot do broad agent reasoning

## 10. Minimal Codex Agent Architecture

### 10.1 Do Not Build Many Agents

The design should not become a dozen independent Codex agents with cron jobs. That would be expensive, hard to debug, and unnecessary.

The agents are intelligent. They can generalize across tasks. We should split only where the operational requirements are genuinely different.

Recommended default split:

1. **`clanky-voice-router`**  
   Cheap, ambient, narrow. Detects commands from rolling recent transcript context. No heavy tools. Implemented as a plugin model call, not a full autonomous cron agent.

2. **`clanky-voice-worker`**  
   Strong, job-scoped. Handles involved user-requested work: materialization planning, summarization, fact-checking, Linear issue proposals, transcript search, research, and tool use.

3. **`clanky-voice-maintainer`**  
   Mostly deterministic Clawcord/Codex routine, not a normal agent. Handles routine timeline/index/job hygiene. Uses cheap model calls sparingly for conversation titles/summaries only when necessary.

4. **`clanky-refinement-worker`**  
   Background job runner, not a general agent. Handles audio export, ElevenLabs calls, transcript alignment, Discord draft replacement, and authoritative span updates.

Everything else should be a hook, job type, command mode, deterministic function, or Clawcord service.

### 10.2 Voice Bots Are Not Codex Agents

`clanky-vc1` and `clanky-vc2` should not be represented as Codex agents. They are Clawcord-managed Discord bot identities.

Codex should see their output as events:

```text
clanky-vc1 captures audio in Code Lounge
  -> Clawcord emits transcript events
  -> router may detect command
  -> Codex creates/handles jobs

clanky-vc2 captures audio in Art Lounge
  -> Clawcord emits transcript events
  -> router may detect command
  -> Codex creates/handles jobs
```

The router/worker can be channel-aware without becoming per-bot.

### 10.3 Why These Splits Make Sense

The split is justified by different cost/risk profiles:

| Component | Runs Ambiently? | Reads Transcript Text? | Model Cost | Tool Access | Reason For Split |
|---|---:|---:|---:|---|---|
| Clawcord voice bots | yes | no LLM; capture/STT only | local STT/provider | Discord voice | Discord one-bot-per-VC constraint |
| Router | yes, but gated | rolling 3 minutes only | very cheap | minimal | must be always available but should not burn tokens |
| Maintainer | yes, low cadence/event-driven | rarely, deltas only | cheap/none | Clawcord metadata | routine upkeep, indexing, summaries |
| Worker | no | selected job windows | strong | broad | involved reasoning and tool use |
| Refinement Worker | job queue | selected windows/audio | provider cost, little LLM | STT provider + Clawcord | async transcript quality loop |

This prevents the common Codex failure mode:

> cron agent wakes up, reads a bunch of context, thinks a little, does nothing, repeats forever

## 11. Agent 1: `clanky-voice-router`

### 11.1 Purpose

Detect explicit Clanky commands from recent voice transcript events and emit structured command jobs.

It should not answer the user. It should not summarize conversations. It should not call Linear. It should not call web. It should not materialize transcripts. It should only decide whether a recent utterance is a command addressed to Clanky and, if so, normalize it.

### 11.2 Multi-Channel Behavior

The router is logically one Codex/plugin routine, but it processes candidate events from multiple channel timelines.

It must always be channel-scoped:

- input includes `guild_id`
- input includes `voice_channel_id`
- input includes `capture_run_id`
- input includes `voice_bot_id`
- dedupe is per channel
- rate limits are per channel
- command jobs are created with channel identity
- ambiguous phrases like "this conversation" resolve within the originating voice channel by default

The router should not merge the last three minutes from multiple channels into one prompt. Each router evaluation is for one channel.

### 11.3 Model

Use the cheapest reliable model available.

Recommended order:

gpt-5.5 medium reasoning

The router should be allowed to fail closed. Missing a command occasionally is less bad than firing expensive or destructive jobs constantly.

### 11.4 Triggering and Interaction Sessions

Do not model routing as a one-shot wake window. Model it as a short-lived **interaction session** scoped to one `(guild_id, voice_channel_id, voice_bot_id)`.

The deterministic wake gate is the only way an interaction starts or receives a follow-up. The required wake phrase is:

- "Hey Clanky"
- "Hay Clanky"
- "Hey" followed by a supported local STT variant such as "Blanky", "Planky", "Klanky", or "Clankey"

Bare "Clanky" should not activate the router. This reduces accidental triggers when people are talking about Clanky in the third person.

An interaction session:

- starts when a wake phrase is detected
- includes a 30 second lookback before the wake event
- collects follow-up transcript after the wake event
- can receive more "Hey Clanky" follow-ups inside the same session
- keeps the previous router result, acknowledgement, and related jobs visible to the router
- is sunset after 10 minutes of no interaction activity

Follow-up wakes should not be blocked. If Clanky acknowledges or starts the wrong thing, a user must be able to say "Hey Clanky, no, I meant..." and have the router interpret that as a correction, cancellation, or replacement relative to the prior state.

#### Stage 0: Deterministic Wake Prefilter

Run this on every new transcript event. This is not an agent call.

Only wake phrases start or resume an interaction. Command hints such as "leave", "deafen", "start a transcript", or "tell me about birds" must not activate routing without a wake phrase.

#### Stage 1: Debounced Router Evaluation

Once an interaction starts, collect an idle-closed post-wake turn before asking the router model. The turn should remain open while the requester still has active audio or an in-flight STT task, close after a short idle period, and hard-cap at a longer maximum. Other speakers' finalized transcript rows inside the window are still included, but unrelated channel chatter should not indefinitely hold the router open. After logical close, wait a short STT flush grace before querying SQLite so late-arriving segments that ended inside the logical window are included.

Another explicit "Hey Clanky" wake inside the TTL supersedes an in-flight router evaluation or reopens the interaction as a follow-up/correction. Ordinary non-wake speech is collected while the turn is open. If the model returns `wait_for_more`, Clawcord enters a bounded follow-up capture mode where the user's next non-wake answer can continue the same interaction.

The router sees:

- the full current interaction window, not just the wake event
- the 30 second lookback before the wake event
- follow-up utterances collected after the wake event
- current room mode
- active voice bot id
- speaker id/label
- last router result and last acknowledgement for this interaction
- active queued/running jobs in this channel
- cancellable job ids
- list of recent commands dispatched in this channel for dedupe

The router should return `wait_for_more` when the user is still explaining. Otherwise it should dispatch, ignore, cancel, amend, or replace.

#### Stage 2: Deterministic Validation and Action

A deterministic validator checks:

- JSON schema
- allowed action
- confidence threshold for dispatch/cancel/replace/amend actions
- supported command kind
- dedupe hash
- permission requirements
- confirmation requirements
- whether the command is destructive/permanent
- whether the target job is cancellable
- whether a matching job already exists

Only then is a job created, cancelled, replaced, or amended.

### 11.5 Cadence, TTL, and Token Budget

Recommended hard limits:

- Wake lookback: 30 seconds.
- Interaction TTL: 10 minutes after the last wake/follow-up/action.
- Router settle window: initially 10 seconds after each "Hey Clanky" wake.
- Router max collection window before forced decision: 5 minutes.
- Router input: max 1,500 to 2,500 tokens.
- Router output: max 300 tokens.
- Max router LLM calls: 2 per minute per active voice channel.
- Max global router LLM calls: configurable, initially 4 per minute because there are two voice bots.
- No router LLM call if no candidate since last cursor.
- Dedupe by `(guild_id, voice_channel_id, voice_bot_id, source_event_ids, normalized_command_kind, normalized_args_hash)`.
- Ignore repeated classification for already-processed event ids.
- Do not include transcript beyond the interaction window unless the user explicitly asks for older context.
- Do not include other channel transcripts.

Typical cost should be near zero when nobody says "Hey Clanky".

### 11.6 Allowed Tools

The router should have almost no tools.

Allow:

- read current Clawcord voice status
- create/emit a structured command event
- maybe ask for confirmation through Clawcord if deterministic validator marks it needed

Deny:

- Discord message writing except confirmation/status helper
- Linear
- GitHub
- web/search
- file system browsing
- full transcript search
- source audio access

### 11.7 Router Output Schema

The router returns an action enum. `is_command` may remain as a compatibility field, but action is the canonical decision.

```json
{
  "action": "dispatch_now",
  "is_command": true,
  "confidence": 0.93,
  "wake_phrase_detected": true,
  "command_kind": "start_live_transcript",
  "requested_by_user_id": "123",
  "requested_by_speaker_label": "Will",
  "guild_id": "456",
  "voice_channel_id": "789",
  "capture_run_id": "cap_01J...",
  "voice_bot_id": "clanky-vc1",
  "arguments": {
    "relative_start": "-10m",
    "relative_end": "now",
    "live": true,
    "refine": true
  },
  "requires_confirmation": false,
  "reason": "User explicitly asked Clanky to start a live transcript from ten minutes ago.",
  "acknowledgement_text": "Working on that transcript for you.",
  "source_event_ids": ["evt_01J..."]
}
```

If the user is still explaining:

```json
{
  "action": "wait_for_more",
  "is_command": false,
  "confidence": 0.45,
  "reason": "The user woke Clanky and is describing the task, but has not yet stated enough actionable detail.",
  "source_event_ids": ["evt_01J..."]
}
```

If not a command:

```json
{
  "action": "ignore",
  "is_command": false,
  "confidence": 0.12,
  "reason": "Speaker said a wake-like phrase while discussing Clanky in third person, not to ask Clanky to act.",
  "source_event_ids": ["evt_01J..."]
}
```

If correcting or cancelling a previous action:

```json
{
  "action": "cancel_job",
  "confidence": 0.9,
  "target_job_id": "job_01J...",
  "reason": "User said 'no, stop that' immediately after Clanky acknowledged the job.",
  "acknowledgement_text": "Got it. I am cancelling that.",
  "source_event_ids": ["evt_01J..."]
}
```

Supported actions:

- `dispatch_now`
- `wait_for_more`
- `ignore`
- `cancel_job`
- `amend_job`
- `replace_job`

### 11.8 Router System Prompt Sketch

```text
You are clanky-voice-router.

Your job is only to decide what to do with a short-lived Clanky voice interaction from one voice channel.

You must return JSON only.

Do not answer the user.
Do not summarize the conversation.
Do not call tools except the command dispatch tool.
Do not infer a command from casual discussion about Clanky.
Prefer false negatives over false positives.
Destructive, permanent, or ambiguous actions require confirmation.

The transcript context is from exactly one voice channel and one voice bot. Do not infer anything about other channels.

Activation has already been gated by "Hey Clanky" or a supported STT variant. Your main job is nuance:

- inspect the entire `window_events` list, not only the wake event
- decide whether the user is talking to Clanky or merely about Clanky
- treat "actually do this" and "actually <verb>" inside a wake-gated interaction as an explicit address override; do not reject those turns merely because they are framed as examples, hypotheticals, quotes, or third-person demonstrations
- treat "no", "wait", "stop", "that's not what I meant", and similar phrases as possible corrections to the current interaction
- use active job context when cancelling, amending, or replacing previous work
- return `wait_for_more` if the user is still explaining
- route only when the intent is clear

Recognized command kinds:
- start_live_transcript
- start_draft_transcript
- materialize_transcript
- make_permanent
- voice_agent_task
- pause_listening
- resume_listening
- forget_window
- leave_room
- deafen_listening

Recognized actions:
- dispatch_now
- wait_for_more
- ignore
- cancel_job
- amend_job
- replace_job

Resolve only obvious arguments.
Use relative time windows when spoken.
For fuzzy references such as "this conversation" or "what Vince just said", emit the phrase as a context_reference rather than trying to solve it.
When routing, include `acknowledgement_text`, a short status phrase Clawcord can post in agent-chat before dispatch.
```

### 11.9 Job Cancellation and Replacement

Voice-originated worker jobs must be cancellable enough for normal interaction repair.

Minimum behavior:

- queued jobs can be marked `cancelled`
- running jobs can be marked `cancel_requested` and their worker process group is killed
- if a cancelled running job finishes anyway, Clawcord suppresses the Discord result post
- the job record should preserve the worker output for debugging
- the router packet should expose active and cancellable jobs for the originating channel

Better behavior:

- worker subprocesses should expose richer lifecycle/status on the debug dashboard
- worker prompts should receive a cancellation token or job status file path and check it before posting expensive results

Replacement behavior:

- `replace_job` cancels the prior queued/running job if possible, then dispatches the new normalized command
- `amend_job` should either update a queued job payload or cancel-and-replace if the existing job is already running

### 11.10 Current Implementation Notes

The first implementation pass lives in Clawcord, not in the generic Codex Discord harness.

Current defaults:

- `CLAWCORD_ROUTER_LOOKBACK_SECONDS=30`
- `CLAWCORD_ROUTER_MODEL=` (empty means Codex default)
- `CLAWCORD_ROUTER_FALLBACK_MODEL=` (empty means no fallback)
- `CLAWCORD_ROUTER_IDLE_SECONDS=3`
- `CLAWCORD_ROUTER_MIN_SETTLE_SECONDS=1.5`
- `CLAWCORD_ROUTER_STT_FLUSH_GRACE_SECONDS=2`
- `CLAWCORD_ROUTER_TURN_MAX_SECONDS=300`
- `CLAWCORD_ROUTER_INTERACTION_TTL_SECONDS=600`
- `CLAWCORD_ROUTER_MAX_FOLLOWUP_SECONDS=300`

Runtime behavior:

- interactions are in-memory and keyed by `guild_id:voice_channel_id:voice_bot_id`
- ordinary non-wake speech is collected while the current turn is open
- requester active speech or requester STT-in-flight postpones evaluation until the requester goes idle or the turn max is reached
- after the logical turn closes, Clawcord waits for the STT flush grace before building the router packet
- a later "Hey Clanky" wake supersedes an in-flight evaluation or starts a correction turn
- `wait_for_more` opens a bounded follow-up capture where non-wake speech can answer the router's clarification
- `routerInteractions` in the debug overview shows active/recent interaction state
- router packets include `interaction_context.turn_history` so the router can compose follow-up turns with prior decisions
- router packet artifacts are still written under the channel `router/` directory
- queued voice worker jobs can be marked `cancelled`
- running voice worker jobs are marked `cancel_requested` and the tracked Codex worker process group is killed
- cancelled running worker results are retained on the job record but suppressed from Discord

This is intentionally enough to make follow-up correction work without introducing a new schema or migration. If Clawcord restarts, active in-memory interactions are forgotten, but durable jobs and router packet/result artifacts remain in SQLite/channel storage.

## 12. Agent/Routine 2: `clanky-voice-maintainer`

### 12.1 Purpose

Maintain the voice timeline system without burning tokens.

This component should be mostly deterministic. It can be implemented as an Codex routine/job if Codex gives good scheduling/hooks, but it should not be a free-roaming chat agent.

Responsibilities:

- voice bot pool monitoring
- assignment health checks
- presence/watchdog checks
- job queue supervision
- retention sweeps
- conversation boundary maintenance
- conversation title/summary generation when useful
- status card updates
- detecting stale failed jobs
- compacting derived indexes
- ensuring live transcript sinks are healthy

### 12.2 Multi-Channel Behavior

The maintainer must handle multiple active channel timelines in parallel.

It should maintain:

- global voice bot pool state
- one channel state object per active voice channel
- one active capture run per assigned voice bot/channel pair
- separate timeline cursors per channel
- separate conversation segmentation state per channel
- separate live transcript sink state per publication

It should not summarize or scan all active channels together unless a user explicitly asks for cross-channel search.

### 12.3 Model Use

Most maintainer tasks do not need an LLM.

No model needed:

- voice bot pool assignment checks
- voice presence watchdog
- retention sweeper
- job state polling
- Discord status updates
- source audio existence checks
- timeline append/checkpoint
- publication state transitions

Cheap model may be used:

- conversation title generation
- topic labels
- semantic boundary merge/split
- incremental conversation summaries

Even then, it should only see deltas or bounded windows from one channel.

### 12.4 Cadence

Recommended:

- Voice bot pool watchdog: every 30 to 60 seconds, no model.
- Job supervisor: every 15 seconds when active jobs exist, every 5 minutes when idle, no model.
- Retention sweeper: hourly or daily, no model.
- Conversation boundary update: event-driven on transcript events, deterministic.
- Conversation semantic labeling: on soft boundary, conversation close, or every 10-15 minutes of accumulated new speech, cheap model.
- Status reporter: event-driven on state transitions, no model; periodic refresh every 5-15 minutes while active.

Do not run a cron job that repeatedly reads the last hour of transcript text in every active channel.

### 12.5 Maintainer Token Budget

- Semantic boundary/title call input: max 2,000 tokens.
- Incremental summary call input: max 3,000 tokens.
- Summary output: max 500 tokens.
- Per active conversation: at most 1 semantic model call every 10 minutes, unless the conversation closes.
- Retention/status/job loops: zero tokens.

## 13. Agent 3: `clanky-voice-worker`

### 13.1 Purpose

This is the strong agent. It should use the best available model/Codex harness when the task actually requires reasoning or tool use.

It is not ambient. It has no cron. It is invoked by a job created from:

- router command
- Discord button/action
- text command
- operator CLI
- another approved Codex workflow

It can handle many task kinds. Do not create separate strong agents for every task unless the split is operationally necessary.

### 13.2 Multi-Channel Behavior

Every job must specify its channel scope or cross-channel scope.

Most voice-originated jobs are single-channel:

```json
{
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "window_id": "win_01J..."
}
```

Cross-channel jobs must be explicit or contextually justified:

```json
{
  "guild_id": "guild_123",
  "scope": "all_voice_channels",
  "query": "Lumen",
  "since": "-7d"
}
```

The worker should not silently search all channel timelines when the user asked from one voice room unless:

- the user asked for "all transcripts", "everywhere", or an absolute time range without a channel qualifier, or
- context resolution identifies a participant excursion that is probably relevant, or
- the text command in `agent-chat` is clearly global.

### 13.3 Participant Excursions

The worker/context resolver should be able to notice:

1. user leaves Channel A
2. user joins Channel B for a short interval
3. user speaks or asks a question
4. user returns to Channel A
5. Channel A conversation continues

This can be relevant to the Channel A conversation.

Do not try to solve this with a complicated global conversation graph in v1. Instead:

- record participant movement events
- expose a `participant_trace` Clawcord command
- let the worker follow the trace when a query implies it
- include excursion snippets only when likely relevant

Example:

```bash
clawcord voice participant trace \
  --guild <guild-id> \
  --user <user-id> \
  --from 2026-05-11T20:00:00Z \
  --to 2026-05-11T21:00:00Z \
  --include-speech-snippets \
  --json
```

### 13.4 Task Kinds

The same worker can handle:

- materialize transcript window
- summarize selected conversation
- search previous transcripts
- fact-check recent claim
- extract decisions
- propose Linear issues
- create approved Linear issues
- generate implementation plan from voice discussion
- turn transcript into follow-up questions
- compare current discussion to prior transcript corpus
- route to Codex/code-search for code-related tasks

### 13.5 Tools

Allow:

- Clawcord CLI/API for transcript windows, publications, Discord status
- Linear tools
- GitHub/code search when relevant
- Notion/docs when relevant
- web/research when relevant and allowed
- transcript search API
- job status API

Important restriction:

> Discord/transcript operations must go through Clawcord commands or APIs. The worker should not directly manipulate Discord state or raw transcript files when a Clawcord primitive exists.

### 13.6 Worker Context

The worker should receive a curated job packet, not the entire history.

Example:

```json
{
  "job_id": "job_01J...",
  "kind": "voice_agent_task",
  "requested_by_user_id": "123",
  "guild_id": "456",
  "voice_channel_id": "789",
  "window": {
    "window_id": "win_01J...",
    "start_time": "2026-05-11T20:12:00Z",
    "end_time": "2026-05-11T20:47:00Z",
    "selection_reason": "last 35 minutes of active conversation"
  },
  "transcript_quality": "mixed_refined_and_draft",
  "policy": {
    "may_create_linear_without_confirmation": false,
    "may_publish_to_discord": true
  },
  "requested_output": "Issue proposals only. Do not create Linear issues yet."
}
```

The worker can then call:

```bash
clawcord voice transcript render --window win_01J --prefer-refined --format markdown
```

or:

```bash
clawcord voice transcript search --guild 456 --query "fixed point" --since -7d
```

### 13.7 Worker Budget

Set task budgets by job kind.

| Job Kind | Strong Model? | Initial Transcript Tokens | Tool Budget |
|---|---:|---:|---|
| exact materialize transcript | no | 0, deterministic render | Clawcord only |
| summarize current conversation | yes/medium | 8k-32k selected window | no external unless asked |
| fact-check recent claim | yes | 1k-4k around claim | web/research capped |
| propose Linear issues | yes | 8k-24k selected window | Linear lookup optional |
| create approved Linear issues | medium/strong | issue proposal packet only | Linear create |
| search old transcripts | medium/strong | search snippets first | transcript search |
| code follow-up | strong/Codex | selected transcript + repo context | GitHub/Codex |

The worker should not pull a full multi-day transcript unless a user explicitly asks for broad search/synthesis and the tool path is bounded.

## 14. Component 4: `clanky-refinement-worker`

### 14.1 Purpose

High-quality transcript refinement is a background job, not a general agent conversation.

Responsibilities:

1. Resolve transcript window.
2. Export mixed source audio for the window.
3. Submit to ElevenLabs STT.
4. Receive refined transcript with word timestamps and diarized speaker ids.
5. Align ElevenLabs speakers to known Discord speakers.
6. Create authoritative transcript span.
7. Update Discord draft messages if there is a publication.
8. Attach final transcript artifact.
9. Mark job complete or failed.

### 14.2 ElevenLabs Request

Use the Speech-to-Text endpoint:

```text
POST /v1/speech-to-text
```

Recommended request parameters:

```json
{
  "model_id": "scribe_v2",
  "diarize": true,
  "timestamps_granularity": "word",
  "num_speakers": "<known_or_estimated_active_speaker_count>",
  "webhook": true,
  "webhook_metadata": {
    "job_id": "job_01J...",
    "publication_id": "pub_01J...",
    "window_id": "win_01J...",
    "guild_id": "guild_123",
    "voice_channel_id": "vc_456"
  }
}
```

Use `webhook=true` for longer jobs when webhook infrastructure exists; otherwise synchronous response is acceptable for a first local implementation if request duration is tolerable.

Do not use `use_multi_channel=true` in v1. ElevenLabs supports multi-channel transcription, but each channel is billed independently at full audio duration, which is exactly the cost explosion this design is avoiding.

### 14.3 Mixed Audio Export

The audio export layer should:

1. take the transcript window
2. gather all source audio segments covering that window
3. mix into a single mono provider-ready file
4. preserve a sidecar mapping of local Discord speaker activity by time
5. submit the mixed file to ElevenLabs

Sidecar example:

```json
{
  "window_id": "win_01J...",
  "mixed_audio_path": "jobs/job_01J/mixed.wav",
  "window_start_time": "2026-05-11T20:12:00Z",
  "local_speaker_segments": [
    {
      "speaker_user_id": "user_a",
      "speaker_label": "Will",
      "start_offset": 12.4,
      "end_offset": 17.8,
      "source_event_ids": ["evt_010", "evt_011"]
    },
    {
      "speaker_user_id": "user_b",
      "speaker_label": "Vince",
      "start_offset": 18.1,
      "end_offset": 22.9,
      "source_event_ids": ["evt_012"]
    }
  ]
}
```

### 14.4 Speaker Alignment

ElevenLabs returns word-level timings and diarized `speaker_id` values. The local timeline knows which Discord user spoke at which times. Alignment should be heuristic but principled.

Algorithm:

1. Convert ElevenLabs word timings to absolute offsets within the transcript window.
2. Group words into speaker turns by ElevenLabs `speaker_id`.
3. Build an overlap matrix:

```text
score[elevenlabs_speaker_id][discord_user_id] =
  total time overlap between ElevenLabs words/turns and local Discord speaker segments
```

4. Weight overlap by:
    - word duration
    - local segment confidence if available
    - whether local STT text roughly matches nearby refined words
5. Solve assignment with maximum-weight bipartite matching.
6. Apply assignment if confidence exceeds threshold.
7. If confidence is low, mark speaker as `unknown` or `speaker_unresolved` rather than confidently wrong.
8. Store `speaker_alignment.json`.
9. Allow future manual repair or rerun.

Speaker alignment output:

```json
{
  "alignment_id": "align_01J...",
  "window_id": "win_01J...",
  "method": "temporal_overlap_hungarian_assignment",
  "assignments": [
    {
      "provider_speaker_id": "speaker_0",
      "discord_user_id": "user_a",
      "speaker_label": "Will",
      "confidence": 0.91
    },
    {
      "provider_speaker_id": "speaker_1",
      "discord_user_id": "user_b",
      "speaker_label": "Vince",
      "confidence": 0.87
    }
  ],
  "unresolved_provider_speakers": [],
  "notes": []
}
```

### 14.5 Multi-Channel Behavior

Refinement jobs are window-scoped. A refinement job should not care which voice bot originally captured the audio except for debugging.

The important identifiers:

- `guild_id`
- `voice_channel_id`
- `window_id`
- `capture_run_ids` covered by the window
- `source_audio_paths`
- `publication_id`

A single refined window might cross capture run boundaries if a channel had a bot restart or reassignment. The audio export layer must handle that.

### 14.6 Failure Handling

If refinement fails:

- keep draft transcript visible
- mark publication `failed_draft_retained`
- preserve source audio until normal retention/retry expiry
- log error in job metadata
- eventually add a private operator logging channel/DM, but do not block v1 on that

If Discord edits fail:

- log it
- keep final artifact as the authoritative transcript
- optionally post a replacement refined message if easy
- do not overbuild around rare edit failure in v1

## 15. Proposed Codex Configuration

This is the chosen config contract for the plan.

```yaml
plugins:
  clawcord_voice:
    enabled: true
    config_path: host/linux/codex/config/clanky_voice.yaml
    event_source: clawcord.voice.events
    tool_namespace: clawcord.voice
    job_queue: clawcord.voice.jobs

voice_bot_pool:
  bots:
    - id: clanky-vc1
      discord_user_id_env: CLANKY_VC1_DISCORD_USER_ID
      token_env: CLANKY_VC1_DISCORD_BOT_TOKEN
    - id: clanky-vc2
      discord_user_id_env: CLANKY_VC2_DISCORD_USER_ID
      token_env: CLANKY_VC2_DISCORD_BOT_TOKEN
  future_bot_pattern: clanky-vc{n}
  auto_join:
    enabled: true
    eligible_voice_channel_names:
      - Art Lounge
      - Code Lounge
      - Environment Lounge
    effective_human_threshold: 2
    count_muted_users: true
    count_deafened_users: false
    min_dwell_after_join: 5m
    release_after_no_speech: 10m
    release_after_effective_count_below_threshold: 5m
    cooldown_after_auto_release: 10m
  capacity:
    allow_auto_preemption: false
    admin_force_move: true
    capacity_full_surface: agent_chat

retention:
  draft_transcript_events: 7d
  source_audio: 7d
  job_metadata: 30d

storage:
  primary_event_store: sqlite
  sqlite_enabled: true
  sqlite:
    path: runtime/codex-home/clawcord/voice/voice.sqlite3
    journal_mode: WAL
    synchronous: NORMAL
    fts5_enabled: true
  generated_exports:
    jsonl: publication_and_debug_only

agents:
  clanky-voice-router:
    description: Cheap event-gated voice command router.
    invocation: plugin_model_call
    model:
      preferred: local-qwen-small-or-equivalent
      fallback: cheap-openrouter-model
      max_input_tokens: 2500
      max_output_tokens: 300
      temperature: 0
    triggers:
      transcript_event_candidate:
        source: clawcord.voice.timeline
        window: 3m
        prefilter: wake_or_command_candidate
        throttle_per_channel: 5s
        max_calls_per_channel_per_minute: 2
        max_global_calls_per_minute: 4
    tools:
      allow:
        - clawcord.voice.status.read
        - clawcord.voice.command.emit
        - clawcord.voice.confirmation.request
      deny:
        - clawcord.voice.transcript.full_read
        - clawcord.discord.raw_write
        - linear.*
        - github.*
        - web.*
        - filesystem.*
    memory:
      persistent: false
      include_other_channels: false
    output:
      format: json
      schema: clanky.voice.CommandDetectionResult

  clanky-voice-worker:
    description: Strong job-scoped worker for involved voice tasks.
    invocation: codex_exec_sticky_session
    model:
      preferred: gpt-5.5-codex-or-strongest-configured
      fallback: strong-openrouter-model
      max_input_tokens: 64000
      max_output_tokens: 8000
      temperature: 0.2
    tools:
      allow:
        - clawcord.voice.*
        - clawcord.discord.publication.*
        - linear.*
        - github.search
        - github.fetch
        - notion.search
        - web.search
        - web.open
      deny:
        - filesystem.raw_voice_corpus_access
        - clawcord.discord.raw_write_without_publication_context
    guardrails:
      linear_creation_requires_confirmation: true
      destructive_forget_requires_confirmation: true
      max_subagents: 2
      no_recursive_subagents: true
      cross_channel_reads_require_explicit_scope_or_context_reason: true

maintainer:
  invocation: deterministic_clawcord_routine
  schedules:
    voice_bot_pool_watchdog:
      interval: 60s
      uses_model: false
    job_supervisor_active:
      interval: 15s
      only_when: active_voice_jobs_exist
      uses_model: false
    job_supervisor_idle:
      interval: 5m
      only_when: no_active_voice_jobs
      uses_model: false
    retention_sweeper:
      interval: 1h
      uses_model: false
    status_refresh:
      interval: 10m
      only_when: active_voice_channels_exist
      uses_model: false
  semantic_model_calls:
    conversation_soft_boundary:
      max_input_tokens: 2000
      throttle_per_conversation: 10m
    conversation_closed:
      max_input_tokens: 3000

refinement:
  provider: elevenlabs
  audio_mode: mixed_mono
  model_id: scribe_v2
  diarize: true
  timestamps_granularity: word
  use_multi_channel: false
  webhook: true
  alignment:
    method: temporal_overlap_hungarian_assignment
    fallback_low_confidence: speaker_unresolved
```

## 16. Hooks and Job Flow

Codex should use hooks to trigger narrow work. Hooks are cheap. Agents are expensive. Prefer hooks.

### 16.1 Hook: `on_voice_bot_assignment_changed`

Runs when Clawcord assigns or releases `clanky-vc1` or `clanky-vc2`.

Steps:

1. Update voice bot pool state.
2. Create or close capture run.
3. Write assignment event into channel timeline.
4. Update status surface.
5. Do not call a model.

### 16.2 Hook: `on_voice_state_changed`

Runs when humans join/leave/mute/deafen.

Steps:

1. Update channel occupancy snapshot.
2. Recompute effective human count.
3. If eligible lounge crosses threshold and bot is available, auto-assign after debounce.
4. If effective count drops below threshold, start release timer.
5. If capacity full, write capacity status only if there is an explicit request.
6. Do not call a model.

### 16.3 Hook: `on_transcript_event`

Runs whenever local STT produces a new event.

Deterministic steps:

1. Append event to the correct channel timeline.
2. Store source audio metadata.
3. Update active capture run.
4. Update `last_speech_at`.
5. Run wake/command prefilter.
6. If candidate, enqueue router evaluation for that channel.
7. Update activity-based conversation boundary state for that channel.
8. Notify live transcript sink if an active publication covers this event.

No strong model call.

### 16.4 Hook: `on_router_command_detected`

Runs after cheap router emits a valid command.

Deterministic steps:

1. Validate schema.
2. Check permission.
3. Check dedupe.
4. Determine whether confirmation is required.
5. If confirmation required, create confirmation card.
6. Otherwise create a `TranscriptJob` or direct Clawcord action.

No strong worker unless the command needs involved reasoning.

### 16.5 Hook: `on_confirmation_approved`

Runs when a user approves a confirmation.

Examples:

- "Create issues 1 and 3."
- "Yes, forget the last ten minutes."
- "Yes, make this permanent."

Deterministic steps:

1. Resolve approval to pending command/job.
2. Create or continue job.
3. Invoke worker only if reasoning/tool use is required.

### 16.6 Hook: `on_job_created`

Routes job to deterministic path or worker.

| Job | Path |
|---|---|
| exact `start_live_transcript --since -10m` | deterministic materialization + live sink |
| `make_permanent` with resolved window | deterministic publication + refinement |
| `summarize this` | strong worker |
| `fact-check that` | strong worker |
| `propose Linear issues` | strong worker |
| `forget last 10m` | deterministic after confirmation |
| `search where we talked about Lumen` | worker using transcript search |

### 16.7 Hook: `on_publication_created`

If `refine_requested=true`:

1. Create refinement job.
2. Announce permanent rendering if this is a permanent transcript.
3. Ping everyone currently in the relevant voice room.
4. Update status: draft published, refinement queued.
5. Refinement worker handles audio export/provider call.

### 16.8 Hook: `on_refinement_complete`

1. Add authoritative refined transcript span to the correct channel timeline.
2. Update publication metadata.
3. Edit/replace Discord draft messages.
4. Attach final transcript artifact.
5. Update search behavior to prefer refined text.
6. Mark job complete.

### 16.9 Hook: `on_retention_expiry`

1. Find unpublished transcript/audio outside retention.
2. Skip anything referenced by active jobs.
3. Skip source audio needed for pending refinement.
4. Delete or tombstone according to policy.
5. Delete expired SQLite rows and source audio files in the same retention pass.
6. Run FTS maintenance/optimization when needed.
7. Update status metrics.

No model.

## 17. Token Burn Control

This deserves its own section because it is the easiest way for the system to become bad.

### 17.1 Rules

1. No strong model cron jobs.
2. No cheap model cron jobs that read transcript text without a new event or boundary reason.
3. No agent should read the full transcript corpus by default.
4. The router only sees one channel's rolling 3-minute window after deterministic prefilter.
5. The maintainer reads transcript deltas, not whole conversations, except on close/materialization.
6. Strong worker agents are job-scoped.
7. Subagents are opt-in and capped.
8. Status, retention, and voice bot pool management are deterministic.
9. Use cursors and event ids everywhere.
10. Cache summaries and refined spans.
11. Cross-channel reads require explicit scope or a specific context-resolution reason.

### 17.2 Router Cost Control

Bad:

```text
Every 10 seconds, for every active voice channel:
  read last 3 minutes
  ask cheap model if command exists
```

Better:

```text
On every transcript event:
  deterministic prefilter checks for wake/command candidate
  if no candidate: do nothing
  if candidate: enqueue router eval with last 3 minutes from the same channel
  cheap router returns JSON
```

Even better:

- Only one router eval per candidate event.
- Dedupe repeated wake phrase fragments.
- Throttle per channel and globally.
- Use local model if good enough.

### 17.3 Maintainer Cost Control

Bad:

```text
Every 5 minutes:
  summarize every active conversation from scratch in every channel
```

Better:

```text
On conversation delta >= 10 minutes of new speech:
  summarize only new delta for that channel
  merge with cached running summary
```

Bad:

```text
Every hour:
  ask an agent what jobs need cleanup
```

Better:

```text
Deterministic job sweeper reads job states and timestamps.
```

### 17.4 Worker Cost Control

Bad:

```text
User: "make issues from this"
Worker reads last 7 days of transcripts across every voice channel.
```

Better:

```text
Context resolver selects current channel's active conversation or requested window.
Worker reads only that window.
Worker proposes issues.
Linear creation requires approval.
```

Bad:

```text
Fact-check command launches broad research over entire conversation.
```

Better:

```text
Select the last speaker turn or ±2 minutes.
Extract claims.
Research at most 3 claims unless user asks for more.
```

### 17.5 Subagent Cost Control

Codex subagents are useful when there is real parallel decomposition. They are dangerous when they become "ask three agents to think about the same transcript."

Rules:

- The strong worker may spawn at most 2 subagents by default.
- No recursive subagents.
- Subagents must have a narrow tool/domain purpose.
- Subagents receive only the relevant excerpt, not the whole transcript.
- Subagent use must be justified by the job kind.

Good subagent uses:

- one research subagent per independent factual claim
- one code-search subagent for a repo-specific question
- one Linear-context subagent to find related existing issues

Bad subagent uses:

- three agents all summarizing the same conversation
- a subagent that "thinks about privacy"
- a subagent that reads the whole transcript just in case
- router spawning a worker spawning a router

## 18. Clawcord API/CLI Contract

The Codex agents need stable primitives. The exact implementation language can change, but the command contract should be stable.

### 18.1 Voice Bot Pool Status

```bash
clawcord voice pool status --guild <guild-id> --json
```

Returns:

- `clanky-vc1` assignment/status
- `clanky-vc2` assignment/status
- capacity
- active channels
- paused/deafened states
- failures

### 18.2 Voice Bot Assignment

```bash
clawcord voice pool assign \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --reason explicit_request \
  --json

clawcord voice pool release \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --json

clawcord voice pool move \
  --guild <guild-id> \
  --bot clanky-vc1 \
  --to <voice-channel-id> \
  --reason admin_force_move \
  --json
```

This is Clawcord-owned. Codex may request assignment through Clawcord, but Clawcord decides capacity/locks.

### 18.3 Channel Status

```bash
clawcord voice status \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --json
```

Returns:

- Clanky mode in that channel
- assigned voice bot id
- capture run id
- active conversation id
- retention policy
- live publications
- active jobs
- whether listening/deafened
- effective human count
- last speech timestamp
- last event timestamp

### 18.4 Timeline Tail

```bash
clawcord voice timeline tail \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --since -1h \
  --prefer-refined \
  --json
```

Returns transcript events/spans for a bounded window. `--prefer-refined` means use authoritative refined spans when available.

### 18.5 Absolute-Time Multi-Timeline Query

For questions like "what did people say between 1pm and 2pm?":

```bash
clawcord voice timeline range \
  --guild <guild-id> \
  --from 2026-05-11T13:00:00 \
  --to 2026-05-11T14:00:00 \
  --all-channels \
  --prefer-refined \
  --json
```

Returns grouped timeline snippets by channel.

### 18.6 Conversation List

```bash
clawcord voice conversations list \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --since -2d \
  --json
```

Cross-channel:

```bash
clawcord voice conversations list \
  --guild <guild-id> \
  --all-channels \
  --since -2d \
  --json
```

### 18.7 Resolve Context

```bash
clawcord voice context resolve \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --reference "what Vince just said" \
  --now-event <event-id> \
  --json
```

For participant excursions:

```bash
clawcord voice context resolve \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --reference "the question Will went to ask in Art Lounge" \
  --allow-participant-excursions \
  --json
```

### 18.8 Participant Trace

```bash
clawcord voice participant trace \
  --guild <guild-id> \
  --user <user-id> \
  --from 2026-05-11T20:00:00Z \
  --to 2026-05-11T21:00:00Z \
  --include-speech-snippets \
  --json
```

### 18.9 Materialize

```bash
clawcord voice transcript materialize \
  --window <window-id> \
  --publish discord \
  --live \
  --refine \
  --json
```

or:

```bash
clawcord voice transcript materialize \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --since -10m \
  --publish discord \
  --live \
  --refine \
  --json
```

Creates publication, draft thread/messages, and optional refinement job.

### 18.10 Render Transcript

```bash
clawcord voice transcript render \
  --window <window-id> \
  --prefer-refined \
  --format markdown
```

The worker uses this instead of raw file access.

### 18.11 Search

Single-channel search:

```bash
clawcord voice transcript search \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --query "fixed point" \
  --since -7d \
  --prefer-refined \
  --json
```

Cross-channel search:

```bash
clawcord voice transcript search \
  --guild <guild-id> \
  --all-channels \
  --query "Lumen" \
  --since -7d \
  --prefer-refined \
  --json
```

### 18.12 Jobs

```bash
clawcord voice jobs list --guild <guild-id> --json
clawcord voice jobs get <job-id> --json
clawcord voice jobs retry <job-id> --json
```

### 18.13 Privacy Controls

```bash
clawcord voice pause \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --duration 20m \
  --json

clawcord voice resume \
  --guild <guild-id> \
  --channel <voice-channel-id> \
  --json

clawcord voice forget \
  --window <window-id> \
  --unpublished-only \
  --json
```

## 19. User Experience Flows

### 19.1 Auto-Join in Configured Lounge

Two non-deafened humans join Code Lounge.

Flow:

1. Clawcord sees effective human count for Code Lounge become 2.
2. Code Lounge is in the eager auto-buffer allowlist.
3. `VoiceBotPool` has an available bot.
4. Clawcord assigns `clanky-vc1`.
5. `clanky-vc1` joins Code Lounge.
6. Clawcord starts a capture run.
7. No announcement is posted solely for local buffering.
8. Timeline starts accumulating local draft transcript/audio.

### 19.2 Explicit Text Request for VC Bot

User types in `agent-chat`:

> Clanky, send a VC bot to Environment Lounge.

Flow:

1. Codex-native `Clanky` receives text command.
2. Worker or deterministic command handler calls `clawcord voice pool assign`.
3. If available, Clawcord assigns a VC bot and replies with status.
4. If no bot is available, `Clanky` says no spare VC bot and lists current assignments.

### 19.3 Starting a Live Transcript From Ten Minutes Ago

User says in Code Lounge, currently captured by `clanky-vc1`:

> "Clanky, start a live transcript from ten minutes ago."

Flow:

1. `clanky-vc1` captures audio in Code Lounge.
2. Clawcord local STT emits transcript event into Code Lounge timeline.
3. Wake prefilter sees "Clanky" and "live transcript."
4. Router gets last 3 minutes from Code Lounge only.
5. Router returns `start_live_transcript`.
6. Validator sees no confirmation required.
7. Clawcord creates a window from `now - 10m` to live for Code Lounge.
8. Draft transcript thread is created.
9. Draft events from the last 10 minutes are backfilled.
10. Live transcript sink begins streaming new events from Code Lounge.
11. Refinement is not necessarily required unless the user asks to make it permanent; if policy says live transcripts should refine automatically, queue refinement.
12. Thread says:
    - "Draft transcript active."
    - "High-quality refinement pending/queued" if applicable.
13. If refinement completes, Discord draft content is updated/replaced and final artifact is attached.

If `clanky-vc2` is simultaneously capturing Art Lounge, none of its transcript context appears in this prompt or materialization.

### 19.4 Making a Conversation Permanent

User says:

> "Clanky, make this conversation permanent."

Flow:

1. Router detects `make_permanent`.
2. Context resolver maps "this conversation" to active conversation id in the originating channel.
3. Publication is created in Discord.
4. `Clanky` announces in the configured text surface that a permanent transcript is being rendered.
5. `Clanky` pings everyone currently in the relevant voice room.
6. Draft transcript is posted immediately.
7. Mixed source audio window is queued for ElevenLabs refinement.
8. Refined transcript becomes source of truth for that window.
9. Discord thread is updated.
10. Final transcript and recording artifacts are attached.

### 19.5 Pulling Up What Was Said An Hour Ago

User says in a channel:

> "Clanky, pull up what we said an hour ago."

Flow:

1. Router detects `materialize_transcript` with context reference `an hour ago`.
2. Context resolver searches the conversation index for the originating channel near `now - 1h`.
3. If there is one strong candidate:
    - create a draft summary/transcript card.
4. If ambiguous:
    - ask:
        - "I found two conversations in this room: terrain streaming at 1:05 and build workers at 1:42. Which one?"
5. No permanent publication unless requested.
6. If the selected section has a refined span, use refined text.
7. If not, use draft text and optionally offer refinement.

### 19.6 Absolute-Time Cross-Channel Query

User asks in text:

> "What did people say between 1pm and 2pm?"

Flow:

1. This is not anchored to a single current voice room.
2. Context resolver treats it as an absolute-time multi-timeline query.
3. Clawcord pulls all channel timelines with events in that interval.
4. Worker groups results by channel and conversation.
5. Worker summarizes or offers transcripts for each group.

### 19.7 Participant Excursion Query

Will is in Code Lounge, hops to Art Lounge for five minutes to ask Blake a question, then returns.

Later user asks:

> "Clanky, include the question Will went to ask Blake when summarizing this."

Flow:

1. Context resolver starts with current Code Lounge conversation.
2. It traces Will's channel movement during the conversation window.
3. It finds a short Art Lounge excursion.
4. It pulls the relevant Art Lounge snippet.
5. Worker includes it as a related excerpt, not as if it were part of the Code Lounge timeline.
6. Summary notes the channel switch if relevant.

### 19.8 Cross-Channel Transcript Search

User says or types:

> "Clanky, find everywhere we've talked about Lumen in voice transcripts this week."

Flow:

1. Router or text command detects explicit cross-corpus search intent.
2. Worker calls transcript search with `--all-channels`.
3. Search returns snippets grouped by channel/conversation.
4. Worker reports:
    - Code Lounge conversation
    - Art Lounge conversation
    - Environment Lounge conversation
    - agent transcript archives if included by policy
5. Worker does not materialize full transcripts unless requested.

### 19.9 Fact-Checking a Recent Claim

User says:

> "Clanky, fact-check what Vince just said."

Flow:

1. Router dispatches a `voice_agent_task`.
2. Worker resolves the referenced claim, selecting the last Vince speaker turn or ±2 minutes in the originating channel.
3. Strong worker extracts claims.
4. Strong worker researches bounded claim set.
5. Response goes to `agent-chat` or configured channel:
    - claim
    - verdict
    - evidence
    - uncertainty
    - transcript window reference
6. No transcript is made permanent by default.

### 19.10 Proposing Linear Issues

User says:

> "Clanky, propose Linear issues from the last twenty minutes."

Flow:

1. Router dispatches a `voice_agent_task`.
2. Worker selects the exact last 20 minutes or active conversation subset in the originating channel.
3. Worker renders transcript window, preferring refined spans.
4. Worker drafts issue proposals.
5. Discord/agent chat displays proposals.
6. User approves:
    - "Create issues 1, 3, and 4."
7. Worker creates Linear issues and links them back to the transcript window.

### 19.11 Forgetting Unpublished Context

User says:

> "Clanky, forget the last ten minutes."

Flow:

1. Router detects destructive `forget_window`.
2. Context resolver selects last 10 minutes in the originating channel.
3. Confirmation card:
    - "Forget unpublished local transcript/audio from 2:14-2:24 in Code Lounge?"
4. On approval:
    - local unpublished draft events/audio are deleted or tombstoned
    - search/conversation indexes are updated
    - source audio is removed unless referenced by publication/refinement policy
5. If content was already posted to Discord:
    - use separate published-content deletion/withdrawal policy
    - do not pretend deletion is perfect

## 20. Data Model

### 20.1 Voice Bot

```json
{
  "voice_bot_id": "clanky-vc1",
  "discord_user_id": "bot_user_123",
  "state": "capturing",
  "assigned_guild_id": "guild_123",
  "assigned_voice_channel_id": "vc_456",
  "active_capture_run_id": "cap_01J...",
  "last_heartbeat_at": "2026-05-11T20:15:00Z"
}
```

### 20.2 Voice Channel Assignment

```json
{
  "assignment_id": "assign_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "voice_channel_name": "Code Lounge",
  "voice_bot_id": "clanky-vc1",
  "voice_bot_discord_user_id": "bot_user_123",
  "capture_run_id": "cap_01J...",
  "state": "capturing",
  "mode": "local_buffering",
  "assigned_at": "2026-05-11T20:12:00Z",
  "released_at": null,
  "assignment_reason": "auto_join_effective_humans_ge_2"
}
```

### 20.3 Occupancy Snapshot

```json
{
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "actual_human_count": 6,
  "effective_human_count": 2,
  "muted_human_count": 2,
  "deafened_human_count": 4,
  "non_deafened_human_ids": ["user_a", "user_b"],
  "last_speech_at": "2026-05-11T20:12:00Z",
  "updated_at": "2026-05-11T20:13:00Z"
}
```

### 20.4 Capture Run

```json
{
  "capture_run_id": "cap_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "voice_channel_name": "Code Lounge",
  "voice_bot_id": "clanky-vc1",
  "voice_bot_discord_user_id": "bot_user_123",
  "started_at": "2026-05-11T20:12:00Z",
  "ended_at": null,
  "state": "active",
  "mode": "local_buffering",
  "retention_policy_id": "default_7d"
}
```

### 20.5 Timeline Event

Timeline events should include more than speech.

Examples of event kinds:

- `voice_bot_assigned`
- `voice_bot_released`
- `participant_joined`
- `participant_left`
- `participant_muted`
- `participant_unmuted`
- `participant_deafened`
- `participant_undeafened`
- `speech_segment`
- `command_detected`
- `publication_created`
- `refinement_completed`
- `forget_applied`
- `retention_retired`

Speech event example:

```json
{
  "event_id": "evt_01J...",
  "event_kind": "speech_segment",
  "capture_run_id": "cap_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "voice_channel_name": "Code Lounge",
  "voice_bot_id": "clanky-vc1",
  "speaker_user_id": "user_789",
  "speaker_label": "Will",
  "segment_start_time": "2026-05-11T20:15:30.512Z",
  "segment_end_time": "2026-05-11T20:15:34.946Z",
  "text_draft": "Clanky, start a live transcript from ten minutes ago.",
  "stt_provider": "local",
  "stt_model": "local-whisper-or-equivalent",
  "source_audio_path": "ephemeral/guild_123/channel_456/audio/cap_01J/speaker_user_789/seg_01J.wav",
  "audio_checksum": "sha256:...",
  "gap_since_previous_speech_ms": 4200,
  "provisional_conversation_id": "conv_01J...",
  "quality": "draft"
}
```

### 20.6 Authoritative Transcript Span

```json
{
  "span_id": "span_01J...",
  "kind": "refined_transcript",
  "provider": "elevenlabs",
  "window_id": "win_01J...",
  "publication_id": "pub_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "start_time": "2026-05-11T20:12:00Z",
  "end_time": "2026-05-11T20:47:00Z",
  "text_artifact_path": "durable/publications/pub_01J/transcript.refined.txt",
  "speaker_alignment_path": "durable/publications/pub_01J/speaker_alignment.json",
  "covers_event_id_start": "evt_001",
  "covers_event_id_end": "evt_230",
  "capture_run_ids": ["cap_01J..."],
  "voice_bot_ids": ["clanky-vc1"],
  "created_at": "2026-05-11T21:05:00Z"
}
```

### 20.7 Conversation

```json
{
  "conversation_id": "conv_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "event_id_start": "evt_001",
  "event_id_end": "evt_230",
  "start_time": "2026-05-11T20:12:00Z",
  "end_time": "2026-05-11T20:47:00Z",
  "participants": ["user_1", "user_2"],
  "title": "Codex agent design for Clanky voice memory",
  "topic_labels": ["Clanky", "Codex", "voice transcript", "agent routing"],
  "summary_draft": "Discussion about using a cheap router agent, minimizing cron token burn, supporting multiple Clawcord voice bots, and making refined transcripts authoritative for materialized spans.",
  "state": "ephemeral",
  "transcript_quality": "mixed"
}
```

### 20.8 Transcript Window

```json
{
  "window_id": "win_01J...",
  "guild_id": "guild_123",
  "scope": "single_channel",
  "voice_channel_id": "vc_456",
  "selection_kind": "relative_time",
  "selection_reference": "last twenty minutes",
  "start_time": "2026-05-11T20:27:00Z",
  "end_time": "2026-05-11T20:47:00Z",
  "event_id_start": "evt_080",
  "event_id_end": "evt_230",
  "capture_run_ids": ["cap_01J..."],
  "voice_bot_ids": ["clanky-vc1"],
  "quality": "draft",
  "requires_refinement_for_permanent": true
}
```

Cross-channel window:

```json
{
  "window_id": "win_01J_cross...",
  "guild_id": "guild_123",
  "scope": "all_voice_channels",
  "selection_kind": "absolute_time_range",
  "selection_reference": "1pm to 2pm",
  "start_time": "2026-05-11T13:00:00Z",
  "end_time": "2026-05-11T14:00:00Z",
  "channel_windows": [
    {
      "voice_channel_id": "vc_code",
      "event_id_start": "evt_a",
      "event_id_end": "evt_b"
    },
    {
      "voice_channel_id": "vc_art",
      "event_id_start": "evt_c",
      "event_id_end": "evt_d"
    }
  ],
  "quality": "mixed"
}
```

### 20.9 Publication

```json
{
  "publication_id": "pub_01J...",
  "window_id": "win_01J...",
  "guild_id": "guild_123",
  "voice_channel_id": "vc_456",
  "discord_thread_id": "thread_123",
  "state": "live_draft_published",
  "created_by_user_id": "user_1",
  "created_at": "2026-05-11T20:48:00Z",
  "draft_artifact_path": "durable/publications/pub_01J/transcript.draft.txt",
  "refined_artifact_path": null,
  "recording_artifact_path": null,
  "refinement_job_id": "job_01J..."
}
```

## 21. Storage Model

Use SQLite as the primary store.

Suggested first layout:

```text
runtime/codex-home/clawcord/voice/
  voice.sqlite3
  voice.sqlite3-wal
  voice.sqlite3-shm
  pool/
    bots.json
  ephemeral/
    guild-<guild-id>/
      channel-<voice-channel-id>/
        audio/
          <capture-run-id>/
            speaker-<user-id>/
              <segment-id>.wav
  durable/
    publications/
      <publication-id>/
        transcript.draft.txt
        transcript.refined.txt
        speaker_alignment.json
        recording.mp3
        metadata.json
```

Requirements:

- gitignored
- inspectable through `sqlite3` and Clawcord CLI commands
- structured enough for repair without repeating huge JSON metadata on every speech event
- explicit retention
- safe for backups if desired
- agents reach it through Clawcord commands, not by depending on raw paths

Existing JSONL files do not need to be backfilled. This system has not been running long enough to preserve them, so the SQLite transition can start clean.

### 21.1 SQLite Shape

Design the schema for fast transcript reconstruction first.

The common hot path is:

```text
given room/channel + time range or conversation id
  fetch transcript events ordered by started_at
  join speaker labels
  optionally join refined text spans
  render Markdown/TXT/Discord chunks
```

Core tables:

- `voice_rooms`: one row per Discord voice channel.
- `voice_bots`: Clawcord-owned capture identities such as `clanky-vc1`.
- `speakers`: Discord user identity and latest display labels.
- `capture_runs`: bot presence/capture intervals per room.
- `audio_segments`: source audio file path, speaker, timing, duration, size, hash, capture run.
- `transcript_events`: minimal local STT event rows for timeline reconstruction.
- `conversations`: activity/semantic windows over transcript events.
- `conversation_events`: optional mapping table if one event may belong to more than one view.
- `publications`: Discord materialization state.
- `publication_messages`: Discord message ids/chunks for editable live transcript posts.
- `transcript_jobs`: materialization, live transcript, refinement, retry, and cleanup jobs.
- `command_events`: detected voice commands and router decisions.
- `provider_payloads`: optional raw STT/refinement/debug payloads with shorter retention.

Minimal `transcript_events` columns:

- `id`
- `room_id`
- `capture_run_id`
- `audio_segment_id`
- `speaker_id`
- `conversation_id`
- `started_at_ms`
- `ended_at_ms`
- `sequence`
- `source`
- `text`
- `created_at_ms`

Do not store repeated guild/channel names, thread state, packet diagnostics, full provider JSON, or large debug blobs in each transcript event. Put those in normalized tables or bounded debug tables.

### 21.2 Indexes

Required indexes:

- `transcript_events(room_id, started_at_ms, id)`
- `transcript_events(conversation_id, started_at_ms, id)`
- `transcript_events(speaker_id, started_at_ms, id)`
- `audio_segments(capture_run_id, started_at_ms, id)`
- `audio_segments(room_id, started_at_ms, id)`
- `capture_runs(room_id, started_at_ms, ended_at_ms)`
- `conversations(room_id, started_at_ms, ended_at_ms)`
- `transcript_jobs(state, next_run_at_ms)`
- `publications(room_id, state, created_at_ms)`

Use an FTS5 table for transcript search. Keep it content-linked to `transcript_events` if practical, so text search can return event ids and then the normal timeline query can gather surrounding context.

SQLite should run in WAL mode. Clawcord should own writes; Codex agents should query through Clawcord commands/APIs rather than opening the database directly.

## 22. Conversation Segmentation

### 22.1 Goal

The user wants cohesive conversations, not arbitrary sessions or tiny chunks.

Bad:

- 8 minutes of silence creates separate meaningless conversations.
- every mute/unmute creates a new conversation.
- a whole day in one voice room becomes one blob.
- conversations in different voice channels are accidentally merged.

Good:

- short pauses stay inside the conversation.
- medium pauses become soft boundaries.
- long silence creates a new conversation.
- semantic continuation can merge soft boundaries.
- each voice channel has independent segmentation.
- participant excursions can be included later when contextually relevant.
- raw timeline remains intact.

### 22.2 Initial Deterministic Heuristic

Suggested defaults:

- gap under 3 minutes: same conversation
- gap 3-15 minutes: soft boundary
- gap 15-60 minutes: likely new conversation, mergeable
- gap over 60 minutes: new conversation by default

These thresholds should be configurable.

### 22.3 Cheap Semantic Pass

Only run on soft boundaries or conversation close.

Input:

- last N events before boundary in the same channel
- first N events after boundary in the same channel
- participant set
- gap duration
- current conversation summary/title
- max 2,000 tokens

Output:

```json
{
  "decision": "merge",
  "confidence": 0.78,
  "title": "Clanky voice memory architecture",
  "topic_labels": ["Clanky", "Codex", "voice memory"],
  "reason": "Same speakers resumed the same architecture discussion after a short pause."
}
```

Never let this semantic pass delete or permanently hide raw events.

## 23. Discord UX

### 23.1 Global Voice Bot Pool Status

Posted or requested in `agent-chat`:

```text
Clanky Voice Bot Pool

clanky-vc1
State: locally buffering
Room: Code Lounge
Current conversation: Codex agent design
Since: 2:12pm

clanky-vc2
State: live transcript active
Room: Art Lounge
Current conversation: Terrain material review
Since: 2:40pm

Capacity: 2/2 in use
```

### 23.2 Channel Status Card

A channel-specific status card should make Clanky's state visible when requested:

```text
Clanky Voice Status

Room: Code Lounge
Voice bot: clanky-vc1
Mode: locally buffering
Retention: 7d transcript / 7d audio
Current conversation: Codex agent design for Clanky voice memory
Participants seen: Will, Vince
Live transcript: inactive
Refinement jobs: none

Commands:
- start live transcript
- make permanent
- summarize
- pause
- forget
```

### 23.3 Local Buffering Announcement

Do not announce local buffering just because a VC bot joined.

The bot's visible Discord presence is enough for the initial version. Announcement fatigue would make the system feel noisy and creepy.

### 23.4 Permanent Transcript Announcement

When a permanent transcript is being rendered:

```text
Clanky is rendering a permanent transcript for Code Lounge.

Window: 2:12pm-2:47pm
Status: draft transcript now, ElevenLabs refinement queued.
Participants currently in room: @Will @Vince
```

Ping everyone currently in the relevant room.

### 23.5 Live Transcript Header

```text
Draft Live Transcript

Source: Code Lounge
Captured by: clanky-vc1
Window: started 10 minutes before request
Status: draft local STT
Participants seen: Will, Vince

This transcript may contain local STT errors until refinement completes.
```

### 23.6 Refined Transcript Completion

```text
Refined Transcript Complete

The high-quality transcript has replaced the draft where possible.
Provider: ElevenLabs
Final artifacts:
- transcript.refined.txt
- recording.mp3
```

### 23.7 Action Cards

When context resolution is fuzzy:

```text
I found the likely conversation:

"Codex agent design for Clanky voice memory"
Code Lounge · 2:12pm-2:47pm · Will, Vince · 35m

Actions:
- Show draft transcript
- Make permanent and refine
- Summarize
- Propose Linear issues
```

This is often better than dumping a raw transcript immediately.

## 24. Privacy and Trust Semantics

### 24.1 Visible Listening State

Clanky states should mean something precise:

- **voice bot absent**: no Clawcord voice capture bot is in that room.
- **deafened**: assigned voice bot is not ingesting audio.
- **locally buffering**: assigned voice bot is storing unpublished local transcript/audio.
- **live transcript active**: assigned voice bot/Clawcord is posting draft text to Discord.
- **refining**: a high-quality STT job is running.
- **paused**: ingestion is temporarily disabled.
- **permanent/refined**: selected transcript window is now a durable artifact.

### 24.2 Deafen vs Leave

Deafen is better for temporary privacy because Discord leave/join chimes can feel weird and draw attention.

Suggested behavior:

- "Clanky, pause" -> deafen/stop ingesting in this channel until resume.
- "Clanky, pause for twenty minutes" -> deafen/stop ingesting in this channel for duration.
- "Clanky, get out of here" -> deafen or release the voice bot depending on configured policy; default to deafen for 60 minutes.
- "Clanky, resume" -> resume local buffering if permitted.

If capacity is scarce, paused channels release their voice bot after the pause release timeout.

### 24.3 Forget Semantics

Before publication:

- deleting/tombstoning local unpublished timeline/audio can be meaningful and should be supported.

After publication:

- Discord content may have been seen, copied, cached, or referenced.
- The system can delete/withdraw/edit Discord messages, but should not imply perfect erasure.
- Policy must define what happens to refined artifacts and source audio.

### 24.4 AI Redaction Is Not a Privacy Foundation

A model can help omit irrelevant or sensitive content when summarizing, but privacy must not depend on "the model will know what people do not want saved."

Privacy should be controlled by:

- publication boundary
- retention policy
- pause/deafen
- forget
- confirmations
- visible state

## 25. Implementation Plan

### Phase 0: Fix Capture Correctness

Do this first.

- Verify per-speaker stream attribution.
- Verify timing alignment.
- Verify source audio segment paths.
- Verify reconstruction of arbitrary time windows.
- Add checksums.
- Add diagnostics for speaker/timing mismatch.
- Confirm local STT events map to the right Discord speaker.
- Confirm both `clanky-vc1` and `clanky-vc2` behave identically.
- Add diagnostics that identify the capturing voice bot for every segment.

Exit criteria:

- Given an event id, you can locate the correct speaker audio.
- Given a time window, you can reconstruct the correct audio.
- Speaker attribution is reliable enough to trust.
- Parallel capture in two voice channels produces separate timelines without accidental cross-contamination.

### Phase 1: Voice Bot Pool and Timeline Store

- Add `VoiceBotPool`.
- Add `VoiceChannelAssignment`.
- Add effective occupancy snapshots.
- Add eager auto-join policy for Art Lounge, Code Lounge, Environment Lounge.
- Add capacity-full status.
- Add admin force-move.
- Add `CaptureRun`.
- Add append-only `TimelineEvent`.
- Store source audio segments.
- Store participant join/leave/mute/deafen events.
- Add SQLite timeline tables keyed by guild/channel/room.
- Add FTS5 transcript search.
- Add retention metadata.
- Add `voice pool status`.
- Add `voice status`.
- Add `timeline tail`.

Exit criteria:

- `clanky-vc1` and `clanky-vc2` can be assigned to different channels.
- Auto-join works for configured lounges when effective human count reaches 2.
- Each channel has a separate timeline.
- Timeline survives restart.
- Operators can inspect recent transcript events per channel.

### Phase 2: Capture/Publication Split

- Refactor current `join` semantics.
- Capture run is not a transcript publication.
- Publications are explicit jobs.
- Finalization applies to publications/jobs, not every capture run.
- One capture run can support multiple materialized transcript windows.
- One channel timeline can include multiple capture runs over time.

Exit criteria:

- Clawcord voice bot can be in a room buffering locally with no transcript thread.
- A user can materialize a selected time window on demand.

### Phase 3: Authoritative Span Layer

- Add refined transcript span model.
- Add transcript render with `--prefer-refined`.
- Add search behavior that prefers refined text.
- Preserve draft events for debugging/retention.

Exit criteria:

- If a window has been refined, future renders/searches use refined text.

### Phase 4: Materialization

- Select exact windows by relative time.
- Select absolute-time multi-channel windows.
- Create Discord draft thread.
- Backfill draft events.
- Continue live streaming if requested.
- Track Discord message chunks.
- Attach draft artifact.

Exit criteria:

- "Start a live transcript from ten minutes ago" works using draft local STT.
- It works independently in two simultaneous voice channels.

### Phase 5: ElevenLabs Refinement

- Export selected mixed audio.
- Support windows that cross capture run boundaries.
- Submit to ElevenLabs with diarization and word timestamps.
- Align refined provider speakers to Discord speakers.
- Create authoritative span.
- Update Discord draft chunks.
- Attach final artifact.
- Track failure/retry.

Exit criteria:

- Permanent transcript ends refined, not just draft.
- Draft remains visible if refinement fails.
- Refined spans are channel-scoped and preferred for future reads.
- Speaker alignment confidence is stored.

### Phase 6: Cheap Router

Only now add ambient command detection.

- Deterministic wake prefilter.
- Cheap router model call with 3-minute channel-local window.
- JSON command output.
- Dedupe/throttle per channel and globally.
- Confirmation rules.
- Dispatch to Clawcord jobs.

Exit criteria:

- Voice commands in either active channel trigger the same primitives that CLI can trigger.
- Router cost is near zero when no commands are spoken.
- Router never mixes context from two voice channels.

### Phase 7: Maintainer Routine

- Voice bot pool watchdog.
- Presence watchdog.
- Job sweeper.
- Retention sweeper.
- Conversation boundaries.
- Status updates.
- Cheap title/summary on close or deltas.

Exit criteria:

- Routine work is visible and stable without token burn.
- Multi-channel status is clear.

### Phase 8: Strong Worker

- Summaries.
- Fact-checking.
- Transcript search.
- Linear issue proposals.
- Approved issue creation.
- Implementation plans from voice context.
- Participant excursion tracing.

Exit criteria:

- Spoken "make issues from this" is faster than manually opening Linear.
- Cross-channel search works when explicitly requested.
- Participant excursions can be included when relevant.

### Phase 9: Privacy Polish

- Pause/deafen/leave flows.
- Forget flow.
- Discord action cards/buttons.
- Capacity-full messaging.
- Future operator logging channel/DM.
- Non-work timeout only if still desired and opt-in.

## 26. Concrete First Engineering Slice

Build this first:

1. `VoiceBotPool`
2. `VoiceChannelAssignment`
3. effective occupancy snapshots
4. auto-join/release policy for Art Lounge, Code Lounge, Environment Lounge
5. admin force-move and capacity-full status
6. `CaptureRun`
7. `TimelineEvent`
8. source audio segment store
9. channel-keyed SQLite timeline append/query
10. `clawcord voice pool status`
11. `clawcord voice status --channel`
12. `clawcord voice timeline tail --channel --since`
13. `clawcord voice transcript render --prefer-refined`
14. basic `materialize --channel --since -10m --draft-only`
15. no automatic Discord thread for local buffering

This gives the substrate that every agent depends on.

## 27. Remaining Future Questions

These are intentionally deferred, not blockers.

- Should a future operator logging channel DM Will directly for failures?
- Should live transcripts always refine, or only permanent transcripts?
- Should non-work timeout exist, or is speech inactivity enough?
- How many `clanky-vcN` bots are enough before Discord UX becomes weird?
- Should refined transcript spans be manually editable from Discord?
- Should the agent proactively suggest "this seems worth saving" in high-value conversations?
- What threshold would justify moving beyond SQLite/FTS5 to a separate database or search service?

## 28. Final Architectural Guidance

The right architecture is not "one huge omniscient Clanky agent."

It is:

- `Clanky` as Codex-native text/agent surface
- `clanky-vc1`, `clanky-vc2`, and future `clanky-vcN` as Clawcord-owned voice capture bots
- Clawcord voice bot pool management
- channel-keyed SQLite timeline substrate
- refined transcript overlays
- cheap gated router model call
- low-token deterministic maintainer routines
- job-scoped strong worker
- background ElevenLabs refinement worker
- Clawcord as the stable Discord/voice/transcript API
- Codex as the agent/job/tool orchestration layer

This keeps the magic while avoiding the three failure modes:

1. **Surveillance-feeling recorder**: everything gets immediately published or permanent.
2. **Token furnace agent swarm**: lots of cron agents repeatedly reading transcript text and doing nothing.
3. **Single-channel assumption**: one voice timeline or one bot identity accidentally becomes the model even though Discord voice usage is parallel.

The product should feel magical because a user can casually say in any captured voice channel:

> "Clanky, make the useful part of that conversation permanent and propose the Linear issues."

And the system can:

1. identify the originating voice channel,
2. use that channel's timeline,
3. follow participant movement to another timeline if contextually relevant,
4. find the right window,
5. show the selection,
6. publish a draft,
7. refine it from mixed audio,
8. use the refined transcript as source of truth,
9. propose issues,
10. create approved issues,
11. link everything back to the conversation.

That is the loop to close.
