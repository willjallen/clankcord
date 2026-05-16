# Automation Spec

Clankcord automations are durable JSON rules backed by Rust types. A spec is accepted at the CLI boundary, validated into a typed automation record, persisted in Postgres, evaluated by runtime automation passes, and fired by emitting ordinary runtime jobs.

The CLI prints this document from the same file the repository stores here:

```sh
clankcord automations spec
```

The usual authoring flow is to write JSON, validate it, then create it.

```sh
clankcord automations validate < automation.json
clankcord automations create < automation.json
```

Inspection and lifecycle commands use the same automation surface.

```sh
clankcord automations dry-run < automation.json
clankcord automations list --guild <guild-id> --channel <voice-channel-id>
clankcord automations get <automation-id>
clankcord automations cancel <automation-id>
```

## Top-Level Shape

An automation spec names its schema, human-readable name, optional idempotency key, owner, scope, trigger, condition, expiry, and actions. The required fields are `name`, `owner`, `scope`, `trigger`, `condition`, and at least one action. `schema` defaults to `clankcord.automation.v0`. `expiry.max_fires` defaults to `1`, which makes a spec one-shot unless recurring behavior is requested.

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
    "expires_at": "2026-06-01T16:00:00Z"
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

The JSON boundary accepts snake_case and the documented camelCase aliases for common fields such as `guildId`, `voiceChannelId`, `channelId`, `sourceJobId`, `idempotencyKey`, `maxFires`, `expiresAt`, and `textTarget`. Generated JSON uses snake_case.

## Owners

Agent-owned automations tie future work back to the `agent_task` that registered them. `source_job_id` is required for agent ownership. During creation, the runtime locks that source job and dedupes active automations for the same source job and scope.

```json
{
  "kind": "agent",
  "user_id": "discord-user-id-who-asked",
  "source_job_id": "job_that_created_this_automation"
}
```

User-owned automations require a user id.

```json
{"kind": "user", "user_id": "discord-user-id"}
```

System-owned automations carry system authority.

```json
{"kind": "system"}
```

## Scope

Scope binds a stored automation to one Discord guild and one Discord voice channel. Specs use Discord ids. Resolve people, rooms, and channels through Clankcord commands before storing long-lived JSON.

```json
{
  "guild_id": "553018603226529802",
  "voice_channel_id": "1204188344993447956"
}
```

## Triggers

Tick triggers evaluate when the automation loop observes that the interval is due.

```json
{
  "kind": "tick",
  "interval_seconds": 60
}
```

Event triggers evaluate matching timeline events observed after the automation cursor.

```json
{
  "kind": "event",
  "event_kinds": ["participant_joined", "speech_segment"]
}
```

Job triggers evaluate scoped jobs of selected kinds and states.

```json
{
  "kind": "job",
  "job_kinds": ["refine_transcript"],
  "states": ["complete"]
}
```

`room_state_changed` is a shortcut over room state, occupancy, participant join, and participant leave activity.

```json
{"kind": "room_state_changed"}
```

Job kinds parse through `JobKind`; job states parse through `JobState`. Trigger fields that select multiple values use arrays: `event_kinds`, `job_kinds`, and `states`.

## Evaluation Context

Conditions evaluate against a context object built for the trigger. The object always contains the automation record, runtime clock, room payload, and nullable event and job fields. `room.liveOccupants` comes from current voice state. `room.participants` is keyed by Discord user id and adds `present: true` for present users.

```json
{
  "automation": {"...": "automation record"},
  "runtime": {"now": "2026-05-14T16:00:00Z"},
  "room": {
    "status": {"...": "room status snapshot"},
    "liveOccupants": [],
    "participants": {}
  },
  "event": {"...": "timeline event or null"},
  "job": {"...": "job record or null"}
}
```

Participant overlap checks usually read directly from the participant map.

```json
{
  "kind": "predicate",
  "path": "room.participants.218519280235446272.present",
  "op": "eq",
  "value": true
}
```

Use `clankcord rooms occupants`, `clankcord timeline tail`, `clankcord jobs get`, and the dashboard to inspect real context payloads before registering conditions that depend on specific paths.

## Conditions

Conditions are typed expression trees.

The true condition matches every context.

```json
{"kind": "true"}
```

`all` requires every child condition to match.

```json
{
  "kind": "all",
  "conditions": [
    {"kind": "predicate", "path": "event.kind", "op": "eq", "value": "participant_joined"},
    {"kind": "predicate", "path": "event.user_id", "op": "eq", "value": "218519280235446272"}
  ]
}
```

`any` matches when at least one child condition matches.

```json
{
  "kind": "any",
  "conditions": [
    {"kind": "predicate", "path": "event.text", "op": "contains", "value": "floating point"},
    {"kind": "predicate", "path": "event.text", "op": "contains", "value": "IEEE 754"}
  ]
}
```

`not` wraps one child condition.

```json
{
  "kind": "not",
  "condition": {"kind": "predicate", "path": "room.control.listeningPaused", "op": "eq", "value": true}
}
```

Predicates use dot paths with numeric array indexes. The operator set is:

```text
eq, ne, gt, gte, lt, lte, contains, matches, present, empty
```

`present` and `empty` are presence operators. Other operators accept a string, number, bool, or tagged scalar.

```json
{"kind": "number", "value": 3}
```

Untagged JSON scalars are the normal form.

```json
{
  "kind": "predicate",
  "path": "event.speaker_user_id",
  "op": "eq",
  "value": "218519280235446272"
}
```

## Expiry

Expiry controls how long the automation remains active and how many times it can fire.

Default one-shot:

```json
{"max_fires": 1}
```

Time-bounded one-shot:

```json
{
  "max_fires": 1,
  "expires_at": "2026-06-01T16:00:00Z"
}
```

Recurring for a bounded period:

```json
{
  "max_fires": 50,
  "expires_at": "2026-06-30T16:00:00Z"
}
```

`expires_at` is RFC3339. `max_fires` requires a value greater than zero when present.

## Actions

Actions are the bridge from an automation decision to runtime jobs.

`response.send` creates `text_delivery`.

```json
{
  "kind": "response.send",
  "sink": {"kind": "agent_chat"},
  "content": "Blake, reminder: talk about x."
}
```

Supported sinks are agent chat, concrete channel, and DM.

```json
{"kind": "agent_chat"}
```

```json
{"kind": "channel", "id": "discord-channel-id"}
```

```json
{"kind": "dm", "id": "discord-user-id"}
```

`agent_task.start` creates a `command` job for agent work. `text_target` is optional and uses the same target shape as response sinks.

```json
{
  "kind": "agent_task.start",
  "prompt": "Summarize the completed transcript window and post the result.",
  "text_target": {"kind": "agent_chat"}
}
```

`transcript.start_live` creates a `command` job for live transcript materialization.

```json
{
  "kind": "transcript.start_live",
  "title": "Floating point discussion"
}
```

`sound.play` is accepted by the schema and currently records `automation_action_failed`.

```json
{"kind": "sound.play", "name": "fart"}
```

## Examples

Reminder when a user joins:

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
    "expires_at": "2026-06-01T16:00:00Z"
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

Reminder when two users overlap:

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

Agent work after a refinement job completes:

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
      "prompt": "Fact-check the completed transcript job and publish the most useful corrections."
    }
  ]
}
```

## Authoring Workflow

Resolve names to durable Discord ids with `clankcord members resolve` and `clankcord rooms occupants`. Inspect the trigger context with timeline, job, status, or dashboard views. Write the smallest automation that captures the requested future behavior. Validate it, create it, and submit a visible response describing what was registered.

Common validation failures include missing `owner.source_job_id` for agent-owned automations, missing scope ids, singular trigger fields such as `event_kind`, `job_kind`, or `state`, unknown job kinds or states, zero `max_fires`, invalid RFC3339 expiry timestamps, empty action content, missing sink ids for channel and DM targets, and condition paths outside the actual trigger context.
