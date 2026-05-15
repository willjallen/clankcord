# Clankcord Automation JSON Spec

Clankcord automations are durable rules stored in the SQLite timeline. They are created at the JSON boundary with `clankcord automations create --stdin`, validated with `clankcord automations validate --stdin`, and loaded back into memory when Clankcord starts.

Agents should read this spec before writing automation JSON:

```sh
clankcord automations spec
```

Create an automation:

```sh
cat automation.json | clankcord automations validate --stdin
cat automation.json | clankcord automations create --stdin
```

## Top-Level Shape

```json
{
  "schema": "clankcord.automation.v0",
  "name": "short human-readable name",
  "idempotency_key": "optional stable dedupe key",
  "owner": {
    "kind": "agent",
    "user_id": "discord-user-id",
    "source_job_id": "job_..."
  },
  "scope": {
    "guild_id": "discord-guild-id",
    "voice_channel_id": "discord-voice-channel-id"
  },
  "trigger": {
    "kind": "event",
    "event_kinds": ["participant_joined"]
  },
  "condition": {
    "kind": "true"
  },
  "expiry": {
    "max_fires": 1,
    "expires_at": "2026-05-15T16:00:00Z"
  },
  "actions": [
    {
      "kind": "response.send",
      "sink": {"kind": "agent_chat"},
      "content": "Message to publish"
    }
  ]
}
```

Required fields: `name`, `owner`, `scope`, `trigger`, `condition`, and at least one action.

`schema` defaults to `clankcord.automation.v0` when omitted. `expiry.max_fires` defaults to `1`, so automations are one-shot unless the user clearly asks for recurring behavior.

Both snake_case and the documented camelCase aliases are accepted at the boundary for common fields such as `guildId`, `voiceChannelId`, `sourceJobId`, `idempotencyKey`, `maxFires`, and `expiresAt`. Prefer snake_case in generated JSON.

## Owners

Agent-created automation:

```json
{
  "kind": "agent",
  "user_id": "discord-user-id-who-asked",
  "source_job_id": "job_that_created_this_automation"
}
```

`source_job_id` is required for agent-owned automations. Include `user_id` when the automation came from a user request so later actions can preserve attribution.

User-owned automation:

```json
{"kind": "user", "user_id": "discord-user-id"}
```

System-owned automation:

```json
{"kind": "system"}
```

## Scope

Scope binds an automation to one Discord guild and one voice channel:

```json
{
  "guild_id": "553018603226529802",
  "voice_channel_id": "1204188344993447956"
}
```

Do not use room names in durable specs. Resolve names to IDs first with the Clankcord CLI, current job packet, or timeline context.

## Triggers

### Tick

Runs when the runtime automation loop sees that the interval is due.

```json
{
  "kind": "tick",
  "interval_seconds": 60
}
```

Use tick triggers for recurring checks. Pair them with an expiry so they do not live forever accidentally.

### Event

Runs when matching timeline events are observed after the automation cursor.

```json
{
  "kind": "event",
  "event_kinds": ["participant_joined", "speech_segment"]
}
```

Common event kinds include `participant_joined`, `participant_left`, `occupancy_updated`, `room_state_changed`, `speech_segment`, `transcript`, `automation_created`, `automation_fired`, and job lifecycle events emitted by the runtime.

### Job

Runs when matching jobs in the scoped channel reach one of the listed states.

```json
{
  "kind": "job",
  "job_kinds": ["response", "agent_task"],
  "states": ["complete"]
}
```

Common job kinds: `audio_segment`, `wake_activation`, `agent_task`, `response`, `refine_transcript`, `confirmation_required`, `command`, `room_agent_placement`, `runtime_control`.

Common states: `queued`, `running`, `waiting`, `complete`, `failed`, `cancelled`, `agent_dispatch_failed`, `confirmation_pending`, `approval_failed`.

### Room State Changed

Shortcut trigger for room-related timeline changes:

```json
{"kind": "room_state_changed"}
```

This evaluates on room state, occupancy, join, and leave events.

## Evaluation Context

Conditions run against a context object. The exact event and job payloads are runtime data, so inspect with `clankcord timeline tail`, `clankcord jobs get`, or the dashboard when unsure.

Context shape:

```json
{
  "runtime": {"now": "2026-05-14T16:00:00Z"},
  "room": {"...": "room status snapshot"},
  "event": {"...": "timeline event or null"},
  "job": {"...": "job record or null"},
  "automation": {"...": "automation record"}
}
```

Predicate paths use dot notation. Array indexes are numeric path parts, for example `event.participants.0.user_id`.

Use paths that are actually present in the trigger context:

```json
{"kind": "predicate", "path": "event.speaker_user_id", "op": "eq", "value": "218519280235446272"}
```

```json
{"kind": "predicate", "path": "room.occupancy.effective_human_count", "op": "gte", "value": 2}
```

## Conditions

Always true:

```json
{"kind": "true"}
```

All child conditions must match:

```json
{
  "kind": "all",
  "conditions": [
    {"kind": "predicate", "path": "event.kind", "op": "eq", "value": "participant_joined"},
    {"kind": "predicate", "path": "event.user_id", "op": "eq", "value": "218519280235446272"}
  ]
}
```

Any child condition may match:

```json
{
  "kind": "any",
  "conditions": [
    {"kind": "predicate", "path": "event.text", "op": "contains", "value": "floating point"},
    {"kind": "predicate", "path": "event.text", "op": "contains", "value": "IEEE 754"}
  ]
}
```

Negation:

```json
{
  "kind": "not",
  "condition": {"kind": "predicate", "path": "room.control.listening_paused", "op": "eq", "value": true}
}
```

Predicate operators:

```text
eq, ne, gt, gte, lt, lte, contains, matches, present, empty
```

`eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `contains`, and `matches` require `value`. `present` and `empty` do not.

Scalar values may be strings, numbers, booleans, or tagged values:

```json
{"kind": "number", "value": 3}
```

Prefer untagged JSON scalars unless the value is ambiguous.

## Expiry

Default expiry:

```json
{"max_fires": 1}
```

Time-bounded one-shot:

```json
{
  "max_fires": 1,
  "expires_at": "2026-05-15T16:00:00Z"
}
```

Recurring for a day:

```json
{
  "max_fires": 50,
  "expires_at": "2026-05-15T16:00:00Z"
}
```

`expires_at` must be RFC3339. Do not create unbounded recurring automations unless the user explicitly asked for a persistent rule.

## Actions

### response.send

Publishes a response through Clankcord.

```json
{
  "kind": "response.send",
  "sink": {"kind": "agent_chat"},
  "content": "Blake, Will said he'll be back in 30 minutes."
}
```

Sinks:

```json
{"kind": "agent_chat"}
```

```json
{"kind": "channel", "id": "discord-channel-id"}
```

```json
{"kind": "dm", "id": "discord-user-id"}
```

### agent_task.start

Starts a new agent task when the automation fires.

```json
{
  "kind": "agent_task.start",
  "prompt": "Summarize the completed transcript window and post the answer in agent chat."
}
```

Use this when the action requires reasoning, research, summarization, or follow-up tool use.

### transcript.start_live

Starts a live transcript job.

```json
{
  "kind": "transcript.start_live",
  "title": "Floating point discussion"
}
```

### sound.play

The schema accepts this action:

```json
{"kind": "sound.play", "name": "fart"}
```

The runtime currently records an action failure for `sound.play` until a sound adapter job exists. Do not use it for user-visible behavior yet unless that runtime support has been added.

## Examples

### Remind A User Next Time They Join

```json
{
  "schema": "clankcord.automation.v0",
  "name": "remind blake about x when he joins",
  "idempotency_key": "remind-blake-x:job_abc123",
  "owner": {
    "kind": "agent",
    "user_id": "requesting-user-id",
    "source_job_id": "job_abc123"
  },
  "scope": {
    "guild_id": "553018603226529802",
    "voice_channel_id": "1204188344993447956"
  },
  "trigger": {
    "kind": "event",
    "event_kinds": ["participant_joined"]
  },
  "condition": {
    "kind": "predicate",
    "path": "event.user_id",
    "op": "eq",
    "value": "blake-discord-user-id"
  },
  "expiry": {
    "max_fires": 1,
    "expires_at": "2026-05-21T16:00:00Z"
  },
  "actions": [
    {
      "kind": "response.send",
      "sink": {"kind": "agent_chat"},
      "content": "Blake, reminder: talk about x."
    }
  ]
}
```

### When A User Joins And Requester Is Present

```json
{
  "schema": "clankcord.automation.v0",
  "name": "remind me when blake and i overlap",
  "owner": {
    "kind": "agent",
    "user_id": "requesting-user-id",
    "source_job_id": "job_abc123"
  },
  "scope": {
    "guild_id": "553018603226529802",
    "voice_channel_id": "1204188344993447956"
  },
  "trigger": {
    "kind": "event",
    "event_kinds": ["participant_joined"]
  },
  "condition": {
    "kind": "all",
    "conditions": [
      {"kind": "predicate", "path": "room.participants.blake-discord-user-id.present", "op": "eq", "value": true},
      {"kind": "predicate", "path": "room.participants.requesting-user-id.present", "op": "eq", "value": true}
    ]
  },
  "expiry": {"max_fires": 1},
  "actions": [
    {
      "kind": "response.send",
      "sink": {"kind": "dm", "id": "requesting-user-id"},
      "content": "You and Blake are both here. Reminder: talk about x."
    }
  ]
}
```

Stored automation conditions can use `room.participants.<discord-user-id>.present`.
That participant map is built from the current voice occupants when the automation is
evaluated.

Before registering overlap rules, verify the current room occupants with:

```sh
clankcord rooms occupants <voice-channel-id> --guild <guild-id>
```

### Research Later When A Job Completes

```json
{
  "schema": "clankcord.automation.v0",
  "name": "research after transcript completes",
  "owner": {
    "kind": "agent",
    "user_id": "requesting-user-id",
    "source_job_id": "job_abc123"
  },
  "scope": {
    "guild_id": "553018603226529802",
    "voice_channel_id": "1204188344993447956"
  },
  "trigger": {
    "kind": "job",
    "job_kinds": ["refine_transcript"],
    "states": ["complete"]
  },
  "condition": {"kind": "true"},
  "expiry": {"max_fires": 1},
  "actions": [
    {
      "kind": "agent_task.start",
      "prompt": "Fact-check the completed transcript job and publish the most useful corrections in agent chat."
    }
  ]
}
```

## Common Failure Modes

- Missing `owner.source_job_id` for agent-owned automation.
- Missing `scope.guild_id` or `scope.voice_channel_id`.
- Using singular fields like `event_kind`, `job_kind`, or `state` where the schema requires arrays: `event_kinds`, `job_kinds`, `states`.
- Using room names instead of Discord IDs in durable specs.
- Forgetting that default expiry is one shot. Set `max_fires` higher only when recurring behavior is intended.
- Writing a condition path that does not exist in the trigger context. Inspect `clankcord status`, `clankcord timeline tail`, or `clankcord jobs get` first.
- Using `sound.play` before the sound adapter job exists.

## Agent Workflow

1. Resolve names to durable IDs.
2. Inspect the context shape if condition paths are not obvious.
3. Write the smallest automation that captures the user's intent.
4. Validate with `clankcord automations validate --stdin`.
5. Create with `clankcord automations create --stdin`.
6. Submit a visible response explaining what was registered.
