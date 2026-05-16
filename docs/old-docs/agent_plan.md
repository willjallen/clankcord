# Clanky Agent and Automation Plan

Status: working design document
Last updated: 2026-05-14

## Goal

Clanky should be a capable Discord voice-room agent over Clankcord, not a narrow command bot and not an agent that bypasses the runtime.

This document is the working successor for the agent, response, and programmable automation design. It supersedes older `PLAN.md` sections that assume a router model or router-command terminology, without trying to rewrite the full historical product plan in one pass.

The product target is a voice-native assistant that can:

- answer immediate questions from the current room context
- research external topics and report back
- materialize, summarize, and fact-check conversation windows
- set alarms and reminders
- register future conditional behavior
- ask clarifying follow-up questions
- preserve useful conversational context across many agent calls in the same channel

Examples:

- "hey clanky set an alarm for 10 minutes"
- "hey clanky remind Blake next time he gets in here to talk about x"
- "hey clanky next time Blake joins and I am in the room remind me to talk to him about x"
- "hey clanky I have to leave so if Blake comes back let him know I'll be back in 30 minutes"
- "hey clanky go research some facts about bees"
- "hey clanky what can you do?"
- "hey clanky next time half the room is unmuted and the current minute is divisible by 3 play a fart sound"
- "hey clanky take the last 20 to 40 minutes where Vince and I were talking about floating point and summarize it for us, after that do a fact check"
- "hey clanky make a transcript out of the last hour of conversation between Blake and I, and keep it running until I tell you to stop"

The core rule:

```text
The agent uses the Clankcord CLI/API.
The Clankcord CLI/API creates jobs, automations, responses, and timeline records.
The runtime remains authoritative for state, Discord side effects, and history.
```

The agent should stay on named `clankcord` commands. Those commands create the typed runtime jobs.

## What Clankcord Is

Clankcord is the runtime service that owns Discord voice capture, timeline history, runtime jobs, automations, Discord side effects, and publication surfaces.

Important concepts:

- The sqlite timeline is the canonical history of speech, room state, jobs, responses, automation events, and publications.
- Clankcord-owned voice bots such as `clanky-vc1` and `clanky-vc2` capture audio and produce timeline events. They are not Codex agents.
- The Codex-backed Clanky agent is the reasoning layer. It reads Clankcord state and requests work through Clankcord.
- Discord side effects must be performed by Clankcord. The agent does not post to Discord directly.
- The CLI is the agent's stable tool surface.

## Agent Decision Model

The agent should choose a complete and useful course of action for the user, without biasing toward terse or under-scoped actions.

The decision is:

1. Answer directly when the request can be satisfied now.
   - Use `clankcord` read tools and external research tools as needed.
   - Submit the visible response through `clankcord responses ...`.
   - A good answer can be concise, but it should not be lazy.

2. Use a first-class Clankcord command when runtime-owned work is needed.
   - Transcript, room, response, sound, reminder, and research operations should be first-class CLI verbs.
   - Those CLI verbs create the underlying runtime jobs.
   - The agent should stay on the named CLI surface instead of constructing runtime internals.

3. Register an automation when the request is about future, conditional, or recurring behavior.
   - Automations default to one shot.
   - Automations must have scope, owner, expiry policy, and an audit trail.
   - Person names should be resolved to stable Discord user IDs before registration when possible.

4. Ask a clarifying question when the request is underspecified.
   - Ask through Clankcord.
   - Keep the channel Codex session alive so the answer resumes with useful context.

5. Use follow-up steering when the user replies later.
   - Follow-ups from voice and text should route back into the relevant channel session.
   - The session should preserve enough context that the model can continue naturally.

## CLI-First Job Creation

The agent-facing API should be command shaped.

Good:

```bash
clankcord responses submit --job <job-id> --sink agent-chat --stdin
clankcord automations create --stdin
clankcord transcripts materialize --guild <guild-id> --channel <channel> --from <time> --to <time> --publish discord
clankcord rooms join --guild <guild-id> --channel <room>
clankcord sounds play --guild <guild-id> --channel <room> --sound fart
```

Do not expose a generic agent-facing runtime submission command. That puts too much runtime shape into the agent prompt. The agent should use named, documented CLI surfaces. Those surfaces parse arguments or structured automation specs, then lower into native Rust jobs internally.

Boundary JSON is still acceptable where structure is the product, especially `clankcord automations create --stdin`. That JSON is an automation spec that the CLI validates and lowers internally.

## Wake Activation Capture

Wake handling should not be a router agent. It is deterministic turn assembly that
produces a normal agent-task job.

When the wake-word adapter detects "hey Clanky" in a voice room, runtime opens a short
activation capture scoped to:

```text
guild_id + voice_channel_id
```

The capture exists to build the user's actual request. It is not the long-lived Codex
session key and it is not the whole agent conversation. The long-lived Codex session is
still keyed by channel.

Default capture rules:

- Include the last 30 seconds of same-channel transcript/timeline context before the
  wake event. Mark this section as `prior_to_activation`.
- Include the wake event itself with speaker identity, source event IDs, timestamp, wake
  score, and activated text.
- Collect at least 5 seconds after activation.
- Keep collecting while the activating speaker has active audio or an STT job in flight.
- Close the turn after the activating speaker has been idle for 5 seconds.
- Other speakers' finalized transcript events inside the capture window are included,
  but unrelated room chatter does not keep the turn open forever.
- After logical close, wait a short STT flush grace before reading the timeline so late
  segments that ended inside the window are included.
- Cap the capture window so a stuck mic or missing silence signal cannot block forever.

The agent-task payload should contain an activation bundle, not a naked transcript blob:

```text
prior_to_activation + wake_event + post_activation_turn + room_snapshot + source_event_ids
```

The sections should be clearly labeled so the agent understands what happened before the
address and what the user actually said after addressing Clanky.

### Additive Wake Follow-Ups

A second explicit wake in the same channel can amend the current activation.

Initial defaults:

- Additive preemption window: 10 seconds after the latest wake or submitted activation.
- Independent queued-query threshold: 45 seconds after the latest wake or submitted
  activation.

If any user in the same voice room says "hey Clanky" inside the additive preemption
window, Clankcord should treat it as a continuation, correction, or replacement of the
same activation. If an agent-task job was already queued or running for that activation,
Clankcord should cancel/suppress that in-flight result if possible, append the new turn
to the activation bundle, and submit a replacement agent-task job in the same channel
Codex session.

This is the path for:

```text
hey Clanky, summarize the last thing
hey Clanky, actually include what Vince said about floats too
```

The second wake should not become a disconnected request merely because the first agent
job already started.

If a new wake arrives after the independent queued-query threshold and an agent is still
working deeply on the prior task, Clankcord should not rewrite the prior prompt. It
should assemble a new activation capture and queue a separate agent-task job for the same
channel session. The channel Codex session may still remember the prior task, but the job
payloads remain separate and auditable.

Between the two thresholds, Clankcord should be conservative:

- If the prior activation has not started execution yet, replacement is acceptable.
- If the prior agent is already running, only preempt when the new speech clearly
  corrects, cancels, or replaces the prior request.
- Otherwise queue a linked follow-up job rather than mutating an in-flight prompt.

The result should be easy to reason about:

- wake capture creates or amends an activation bundle
- activation bundle creates an agent-task job
- agent-task job runs in the channel Codex session
- later wake turns either replace the current activation or create a separate queued job

## Automation Model

An automation is:

```text
scope + triggers + condition expression + actions + policy + runtime state
```

Automations read runtime state and emit typed jobs internally. They do not call adapters directly and they do not contain arbitrary code.

Runtime state available to automations includes:

- current room snapshot
- recent timeline events
- job lifecycle events
- Discord presence/state snapshots copied into runtime state
- automation cursor and fire count
- current time

## Automation Boundary Schema

The agent may register automations with:

```bash
clankcord automations create --stdin
```

The input is an automation spec:

```json
{
  "schema": "clankcord.automation.v0",
  "title": "Remind Blake about x",
  "idempotency_key": "agent-task-id:remind-blake-about-x",
  "scope": {
    "guild_id": "guild123",
    "voice_channel_id": "code-lounge"
  },
  "created_by": {
    "kind": "agent",
    "user_id": "requester-user-id",
    "source_job_id": "job_123"
  },
  "triggers": [
    {
      "kind": "event",
      "event_kinds": ["room_member_joined"]
    }
  ],
  "when": {
    "eq": [
      { "path": "event.user_id" },
      "blake-user-id"
    ]
  },
  "then": [
    {
      "kind": "response.send",
      "sink": {
        "kind": "agent_chat"
      },
      "content": "<@blake-user-id> reminder from <@requester-user-id>: talk about x"
    }
  ],
  "policy": {
    "max_fires": 1,
    "expires_after": "7d",
    "cooldown": "30s"
  }
}
```

The runtime lowers this into native Rust:

- `AutomationSpec`
- `AutomationTrigger`
- `AutomationCondition`
- `AutomationAction`
- `AutomationPolicy`

The runtime owns these fields:

```json
{
  "automation_id": "aut_...",
  "state": "active",
  "created_at": "2026-05-14T00:00:00Z",
  "updated_at": "2026-05-14T00:00:00Z",
  "last_evaluated_at": "2026-05-14T00:00:00Z",
  "last_fired_at": null,
  "fire_count": 0,
  "cursor": {
    "last_event_sequence": 1234
  }
}
```

States:

- `active`
- `paused`
- `expired`
- `complete`
- `cancelled`
- `failed`

## Trigger Schema

Triggers decide when an automation is evaluated. Conditions decide whether it fires.

Supported trigger classes:

```json
{ "kind": "tick", "interval": "1m" }
```

```json
{ "kind": "event", "event_kinds": ["room_member_joined", "room_member_left", "speech_segment"] }
```

```json
{ "kind": "job", "job_kinds": ["agent_task", "materialize_transcript"], "states": ["complete", "failed"] }
```

Evaluation rules:

- Event triggers use timeline cursors.
- Tick triggers use `next_run_at`.
- No automation should scan unbounded transcript history on every tick.
- If an automation needs expensive context, it should trigger a first-class runtime job through a typed action.

## Condition Expression Schema

The condition system should be expressive, but not a scripting language.

It is a small typed expression algebra:

- boolean combinators: `all`, `any`, `not`
- comparisons: `eq`, `neq`, `gt`, `gte`, `lt`, `lte`
- string tests: `contains`, `starts_with`, `ends_with`
- collection operations: `count`, `exists`, `filter`
- math operations: `add`, `sub`, `mul`, `div`, `mod`, `ratio`
- values: literals and `path`

No arbitrary code. No loops. No user-defined functions. No shell. No regex until there is a strong reason and a bounded implementation.

Path roots:

- `now`
- `event`
- `job`
- `room`
- `requester`
- `automation`

Room members are structured records:

```json
{
  "user_id": "user123",
  "display_name": "Blake",
  "bot": false,
  "muted": false,
  "deafened": false,
  "present": true
}
```

## Generic Wacky Case

The weird request:

```text
next time half the room is unmuted and the current minute is divisible by 3 play a fart sound
```

should not produce bespoke public condition names.

Generic condition:

```json
{
  "all": [
    {
      "gte": [
        {
          "ratio": [
            {
              "count": {
                "from": { "path": "room.members" },
                "where": {
                  "all": [
                    { "eq": [{ "path": "item.bot" }, false] },
                    { "eq": [{ "path": "item.muted" }, false] },
                    { "eq": [{ "path": "item.deafened" }, false] }
                  ]
                }
              }
            },
            {
              "count": {
                "from": { "path": "room.members" },
                "where": {
                  "eq": [{ "path": "item.bot" }, false]
                }
              }
            }
          ]
        },
        0.5
      ]
    },
    {
      "eq": [
        { "mod": [{ "path": "now.minute" }, 3] },
        0
      ]
    }
  ]
}
```

Action:

```json
{
  "kind": "sound.play",
  "sound_id": "fart",
  "volume": 0.5
}
```

The public API has `count`, `ratio`, and `mod`, not a custom `CurrentMinuteModulo` or `FractionUnmuted` condition. The action is also high level. Runtime compiles `sound.play` into the appropriate typed job and effect.

## Action Schema

Automation actions are high-level requests that runtime compiles into jobs.

They are not direct adapter side effects.

Initial action families:

```json
{
  "kind": "response.send",
  "sink": { "kind": "agent_chat" },
  "content": "Reminder text",
  "mentions": ["requester-user-id"]
}
```

```json
{
  "kind": "sound.play",
  "sound_id": "fart",
  "volume": 0.5
}
```

```json
{
  "kind": "agent_task.start",
  "request": "Research whether the claim from the transcript is true.",
  "response_sink": { "kind": "agent_chat" }
}
```

```json
{
  "kind": "transcript.start_live",
  "from": "-1h",
  "participants": ["blake-user-id", "requester-user-id"],
  "publish": {
    "kind": "agent_transcripts_thread"
  }
}
```

The exact action families should mirror CLI verbs. If a thing is useful from automation, it should usually also exist as a `clankcord` command.

## Example Automations

### Alarm

```json
{
  "schema": "clankcord.automation.v0",
  "title": "Alarm in 10 minutes",
  "scope": {
    "guild_id": "guild123",
    "voice_channel_id": "code-lounge"
  },
  "triggers": [
    { "kind": "tick", "interval": "10s" }
  ],
  "when": {
    "gte": [
      { "path": "now.timestamp" },
      "2026-05-14T12:10:00Z"
    ]
  },
  "then": [
    {
      "kind": "response.send",
      "sink": { "kind": "agent_chat" },
      "content": "<@requester-user-id> alarm: 10 minutes is up."
    }
  ],
  "policy": {
    "max_fires": 1,
    "expires_after": "30m"
  }
}
```

### Remind Blake When He Joins

```json
{
  "schema": "clankcord.automation.v0",
  "title": "Remind Blake when he joins",
  "scope": {
    "guild_id": "guild123",
    "voice_channel_id": "code-lounge"
  },
  "triggers": [
    { "kind": "event", "event_kinds": ["room_member_joined"] }
  ],
  "when": {
    "eq": [
      { "path": "event.user_id" },
      "blake-user-id"
    ]
  },
  "then": [
    {
      "kind": "response.send",
      "sink": { "kind": "agent_chat" },
      "content": "<@blake-user-id> reminder: talk about x."
    }
  ],
  "policy": {
    "max_fires": 1,
    "expires_after": "7d"
  }
}
```

### Remind Me When Blake Joins And I Am Present

```json
{
  "schema": "clankcord.automation.v0",
  "title": "Remind me when Blake joins and I am present",
  "scope": {
    "guild_id": "guild123",
    "voice_channel_id": "code-lounge"
  },
  "triggers": [
    { "kind": "event", "event_kinds": ["room_member_joined", "room_member_left"] }
  ],
  "when": {
    "all": [
      {
        "exists": {
          "from": { "path": "room.members" },
          "where": {
            "all": [
              { "eq": [{ "path": "item.user_id" }, "blake-user-id"] },
              { "eq": [{ "path": "item.present" }, true] }
            ]
          }
        }
      },
      {
        "exists": {
          "from": { "path": "room.members" },
          "where": {
            "all": [
              { "eq": [{ "path": "item.user_id" }, "requester-user-id"] },
              { "eq": [{ "path": "item.present" }, true] }
            ]
          }
        }
      }
    ]
  },
  "then": [
    {
      "kind": "response.send",
      "sink": { "kind": "agent_chat" },
      "content": "<@requester-user-id> Blake is here. Talk to him about x."
    }
  ],
  "policy": {
    "max_fires": 1,
    "expires_after": "7d"
  }
}
```

## Agent Master Prompt

The master prompt should be inserted at the beginning of each long-lived Codex session and reinserted during compactions. It should not be repeated as a giant block for every job if the same session is already alive.

Draft:

```text
You are Clanky, a helpful and rigorous Discord server assistant for the people using
this server, especially participants in voice rooms. Your job is to help them understand,
remember, research, coordinate, and act on conversations. You can answer questions,
summarize or inspect prior discussion, fact-check claims, research outside information,
set reminders, create automations, ask clarifying questions, and report useful results
back to the right Discord surface.

Clankcord is the local system that connects you to Discord. It captures voice, turns
speech into transcript events, stores those events in a SQLite-backed timeline, manages
runtime jobs and automations, stores transcript artifacts, and publishes responses. The
timeline is the authoritative memory of what happened in the server: who spoke, what was
said, what jobs ran, what automations fired, and what was published. Use Clankcord tools
to inspect that memory instead of guessing from the user's latest sentence alone.

Clankcord voice bots such as clanky-vc1 and clanky-vc2 capture audio; they are not you.
You are the Codex-backed reasoning agent that reads Clankcord state, reasons about user
requests, uses external tools when useful, and asks Clankcord to perform work through its
CLI/API.

Your main tools are the `clankcord` CLI commands. Use them to inspect timeline history,
render transcript windows, resolve participants, inspect room state, register automations,
ask clarifying questions, and submit user-visible responses. The CLI is the supported
way to ask Clankcord to do work. Do not post to Discord directly. Do not mutate Clankcord
state by editing files or databases directly.

When a user asks for immediate information, gather enough context to answer well. Use
timeline, transcript, participant, room, message, and external research tools as needed.
Then submit the visible answer through `clankcord responses submit --job <job-id>
--sink agent-chat --stdin`. After a successful submission, return only
`RESPONSE_SUBMITTED` as your final message. Final text is not a publication path.
If a user asks you to DM them about something, treat the request and the answer as
private: send the substantive response through the DM sink, and do not publish the topic,
answer, summary, result, or confirmation to a public channel unless the user explicitly
asks for public disclosure.

You may search the web and should use web research when it would materially improve the
answer, especially for current facts, unfamiliar topics, fact-checking, product or
technical details, or anything where the transcript alone is not enough. Do not invent
facts when research is possible.

When a user asks for runtime work such as transcript creation, room control, sound
playback, reminders, or publication, use the corresponding `clankcord` command. If the
right command is missing, report the tool gap clearly; do not invent another side-effect
path.

When a user asks for future, conditional, or recurring behavior, register an automation
with `clankcord automations create --stdin`. Automations default to one shot unless the
user clearly asks for recurring behavior. Give automations reasonable expiries. Resolve
named people to Discord user IDs before storing durable conditions whenever possible.

When the request is underspecified, ask a focused clarifying question through Clankcord.
Keep the ongoing channel context in mind after the user answers.

Be useful, complete, and intellectually honest. Do not choose a weak answer merely
because it is shorter. Do not be sycophantic. If a user asks for your view on something
said in a transcript, do not just repeat the transcript back to them. Analyze it, check
the assumptions, identify what matters, and say something genuinely useful. If your first
answer would be obvious, shallow, or uninteresting, work harder: inspect more context,
research where helpful, compare alternatives, and produce the strongest answer you can
while staying inside Clankcord's authority boundaries.
```

The session prompt should be followed by a compact live context packet:

- guild and voice channel identity
- requester identity
- current active room members
- current job ID and source event IDs
- wake activation text
- recent channel interaction summary
- known pending clarifications
- relevant active automations
- allowed CLI command summary

## Follow-Up Steering And Sessions

Codex sessions should be keyed primarily by channel:

```text
session key = guild + voice_channel_id
```

That gives the model long-lived channel memory and better token caching. Jobs and interactions occur inside the channel session; they do not define the Codex session key.

The runtime should retire channel sessions with its own policy:

- idle TTL
- explicit reset
- severe tool/session failure
- channel no longer configured
- memory pressure or max-session cap

Text replies in `agent-chat` are the routing problem.

When a text message arrives, Clankcord should decide which channel session it belongs to using contextual signals:

1. explicit channel or room mention
2. reply/thread metadata pointing to a prior Clankcord response
3. pending clarification addressed to that user
4. active job owned by that user
5. recent voice command from the same user
6. configured default room only when unambiguous

If multiple channel sessions are plausible, Clankcord should ask a routing clarification instead of guessing.

Interaction records still matter, but they are not Codex session keys. They track:

- source job ID
- requester user ID
- requested channel
- pending question, if any
- response message/thread IDs
- current state
- last routed channel session

Follow-up flow:

```text
agent needs clarification
  -> clankcord responses ask --job <job-id> --stdin
  -> interaction enters waiting state
  -> user replies by voice or in agent-chat
  -> Clankcord routes reply to the channel session
  -> same Codex session resumes with compacted context
```

## CLI And API Gaps

Needed agent-facing CLI:

```bash
clankcord responses submit --job <job-id> --sink agent-chat --stdin
clankcord responses ask --job <job-id> --sink agent-chat --stdin
clankcord responses submit --job <job-id> --sink dm:<user-id> --stdin
```

```bash
clankcord automations create --stdin
clankcord automations validate --stdin
clankcord automations dry-run --stdin
clankcord automations list --guild <guild-id> --channel <channel-id>
clankcord automations get <automation-id>
clankcord automations cancel <automation-id>
```

```bash
clankcord participants resolve --guild <guild-id> --name Blake
clankcord rooms members --guild <guild-id> --channel <room-or-channel>
```

```bash
clankcord transcripts materialize --guild <guild-id> --channel <room-or-channel> --from <time> --to <time> --publish discord
clankcord transcripts live start --guild <guild-id> --channel <room-or-channel> --from <time>
clankcord transcripts live stop --guild <guild-id> --channel <room-or-channel>
```

```bash
clankcord research run --guild <guild-id> --channel <room-or-channel> --query <query> --sink agent-chat
clankcord sounds play --guild <guild-id> --channel <room-or-channel> --sound <sound-id>
```

```bash
clankcord interactions get <interaction-id>
clankcord interactions route --message <discord-message-id>
clankcord interactions answer --interaction <interaction-id> --stdin
```

Existing generic job inspection remains useful:

```bash
clankcord jobs get <job-id>
clankcord jobs wait <job-id> --timeout 120s
clankcord jobs children <job-id>
```

Missing runtime pieces:

- `response` job and response sink executor
- deterministic wake activation capture state
- activation bundle payloads with labeled pre-wake and post-wake context
- additive wake preemption/replacement policy
- persisted `AutomationSpec` storage in sqlite
- typed condition expression evaluator
- automation cursor and expiry state
- automation timeline events
- trigger indexing for event and tick evaluation
- high-level automation actions compiled to typed jobs
- channel-keyed Codex sessions with runtime retirement
- agent-chat text routing into channel sessions
- interaction records for pending clarifications
- master prompt injection once per session and after compaction
- participant resolution and room member views
- permission checks for automations and response sinks
- sound allowlist

## Implementation Order

1. Add deterministic wake activation capture and activation-bundle payloads.
2. Add additive wake preemption/replacement policy.
3. Add `ResponseRequest` and `response` job execution.
4. Add `clankcord responses submit/ask`.
5. Change agent task completion so visible output must flow through the response path.
6. Add channel-keyed Codex sessions and runtime session retirement policy.
7. Add agent master prompt injection once per session and after compaction.
8. Add participant resolution and room member CLI views.
9. Add typed automation structs and sqlite storage.
10. Add `clankcord automations validate/create/list/get/cancel`.
11. Add expression evaluator with boolean, comparison, path, count, ratio, and mod.
12. Add high-level automation actions such as `response.send`, `sound.play`, `agent_task.start`, and `transcript.start_live`.
13. Add event/tick automation runner with cursors and expiry.
14. Add agent-chat text routing to the right channel session.
15. Add richer first-class CLI verbs for research, sound playback, live transcript control, and chained transcript/fact-check workflows.

## Hard Rules

- No interpreted language.
- No arbitrary user code.
- No direct adapter calls from automations.
- No direct Discord posting by agents.
- No generic runtime submission surface in the agent tool contract.
- No JSON as an internal runtime working model.
- No unbounded timeline scans inside automation ticks.
- No bespoke public condition names for every weird user request.
- No router agent for wake handling; wake handling assembles activation bundles and emits normal jobs.
- Automations default to one shot.
- Every automation has an owner, scope, expiry policy, and audit trail.
