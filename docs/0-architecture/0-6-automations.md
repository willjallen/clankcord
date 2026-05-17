# Automations

Automations are durable runtime rules. A stored automation names who owns it, where it applies, when it evaluates, which condition gates firing, whether a delayed recheck is required, when it expires, and which runtime actions to emit. When an automation fires, it creates ordinary jobs. Those jobs then move through the same scheduler, dependency resolver, adapter paths, and timeline views as any other work.

```text
stored automation
      |
      +--> trigger selects runtime context
      +--> condition evaluates that context
      +--> action lowers into Job values
      |
      v
normal job scheduling and timeline events
```

The schema is `clankcord.automation.v0`. The CLI accepts JSON on stdin through `clankcord automations create`, validates JSON through `clankcord automations validate`, and prints the reference contract through `clankcord automations spec`. `--file` is available when the JSON already lives in a UTF-8 file. Agents use the spec command before creating future, conditional, or recurring behavior.

## Stored Specs

A stored spec has required fields for `name`, `owner`, `scope`, `trigger`, `condition`, and `actions`. `schema` defaults to `clankcord.automation.v0`. `expiry.max_fires` defaults to `1`, so automations are one-shot unless the creator asks for recurring behavior.

```json
{
  "schema": "clankcord.automation.v0",
  "name": "short human-readable name",
  "idempotency_key": "optional stable dedupe key",
  "owner": {"kind": "system"},
  "scope": {
    "scope_kind": "voice_channel",
    "guild_id": "discord-guild-id",
    "scope_id": "discord-voice-channel-id"
  },
  "trigger": {
    "kind": "event",
    "event_kinds": ["participant_joined"]
  },
  "condition": {"kind": "true"},
  "delay": {
    "seconds": 300,
    "condition": {"kind": "true"}
  },
  "expiry": {"max_fires": 1},
  "actions": [
    {
      "kind": "response.send",
      "sink": {"kind": "agent_chat"},
      "content": "Message to publish"
    }
  ]
}
```

The JSON boundary accepts snake_case and the documented camelCase aliases for common non-scope fields such as `guildId`, `sourceJobId`, `idempotencyKey`, `maxFires`, and `expiresAt`. The scope identity fields are `scope_kind` and `scope_id`. Generated JSON uses snake_case.

## Ownership And Scope

Supported owners are `agent`, `user`, and `system`. Agent-owned automations require `source_job_id`, may include `user_id`, lock the source job while being created, and dedupe active automations for the same source job within the same scope. User-owned automations require `user_id`. System-owned automations carry system authority.

Scope binds the automation to one runtime scope. Stored automations currently execute in `voice_channel` scopes, so `scope_id` is the Discord voice channel id. Specs use durable Discord ids, so an agent that depends on a named person or room resolves that name through `clankcord members resolve` or room inspection before storing the JSON.

```json
{
  "scope_kind": "voice_channel",
  "guild_id": "553018603226529802",
  "scope_id": "1204188344993447956"
}
```

## Triggers, Context, And Conditions

Triggers select the runtime contexts an automation evaluates. A `tick` trigger evaluates when its interval is due. An `event` trigger evaluates matching timeline events after the automation cursor. A `job` trigger evaluates scoped jobs of selected kinds and states. `room_state_changed` is a shortcut over room state, occupancy, participant join, and participant leave activity.

Each condition evaluates an object containing the automation record, runtime clock, current room status, current live occupants, current participant map, event-time room snapshots, selected event, and selected job. The participant map is keyed by Discord user id, which makes overlap and presence checks direct. Voice transition events include `event_room.before` and `event_room.after` snapshots for the scoped room so conditions can describe who was present when the transition was recorded.

```json
{
  "automation": {"...": "automation record"},
  "runtime": {"now": "2026-05-14T16:00:00Z"},
  "room": {
    "status": {"...": "room status snapshot"},
    "liveOccupants": [],
    "participants": {}
  },
  "event_room": {
    "before": {"liveOccupants": [], "participants": {}},
    "after": {"liveOccupants": [], "participants": {}}
  },
  "event": {"...": "timeline event or null"},
  "job": {"...": "job record or null"}
}
```

Conditions are typed expression trees: `true`, `all`, `any`, `not`, and `predicate`. Predicate paths use dot notation with numeric array indexes. Operators are `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `contains`, `matches`, `present`, and `empty`. Presence checks can refer directly to the participant map.

```json
{
  "kind": "predicate",
  "path": "room.participants.218519280235446272.present",
  "op": "eq",
  "value": true
}
```

## Actions

Actions lower automation decisions into runtime work. `response.send` creates `text_delivery`. `agent_task.start` creates a `command` job for agent work and accepts an optional `text_target`. `transcript.start_live` creates a `command` job for live transcript materialization. `sound.play` validates as a schema action and currently records `automation_action_failed`.

Action results return through ordinary job state and timeline events. A fired automation records `automation_fired` for each emitted job. An action failure records `automation_action_failed`, and the runner continues evaluating the rest of the action set.

## Execution

`automation_evaluation` is the background job that runs automation passes. It calls `Runtime::run_automations`, which prunes expired room-control markers in Postgres, loads runtime config, rooms, room controls, active jobs, voice projections, and active automation records from the timeline store, and then runs stored durable automations. Stored execution is cursor-based. Expired records are marked `expired`. Trigger contexts are loaded after the automation cursor. The first matching context fires actions or records a pending delayed recheck. A delayed recheck stores the selected event or job and due time on the automation record, then a later pass rebuilds context with the original trigger payload and current room state before firing. Firing marks the automation evaluated and fired, increments `fire_count`, persists emitted jobs through the timeline store, and records events tying those jobs back to the automation.

The authoring workflow is deliberately explicit: resolve durable ids, inspect the context shape, write JSON, validate it, create it, and publish a visible response describing the registered behavior. Validation errors identify the path that violates the spec, including owner/source fields, scope ids, trigger field shapes, expiry timestamps, job kind and state names, action content, sink ids, and condition paths.

## Room Placement

The room placement automation is disabled at its evaluation entry point and emits no placement work. Room placement work is created by explicit command flows such as `/join`, `/leave`, CLI commands, HTTP commands, and agent-issued commands. Those flows create `room_agent_placement` jobs, and the join and leave mechanics then proceed through normal runtime jobs and the Discord voice adapter.
