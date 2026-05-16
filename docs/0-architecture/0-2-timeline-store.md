# Timeline Store

The timeline store is Clankcord's durable memory. It records runtime config snapshots, events, jobs, dependency edges, voice state, capture runs, agent sessions, automations, transcript windows, publication state, authoritative spans, and artifact metadata. Runtime handlers read and write canonical state through this store.

```text
runtime domain logic
        |
        v
TimelineStore
        |
        +--> Postgres projections
        +--> typed payload blobs
        +--> JSONB timeline events
        +--> runtime config snapshots
        +--> agent session records
        +--> automation records
        +--> artifact files
        |
        v
CLI, HTTP, dashboard, agent, and debug views
```

The store also owns the artifact root for voice memory and transcript publication. Audio segment jobs refer to ready WAV files with checksums, timing, speaker identity, capture run, and format metadata. Accepted speech is written as `speech_segment` timeline events. Transcript materialization and refinement write durable files under publication directories and store the paths in publication records.

## Schema Shape

Startup creates the schema before runtime config is snapshotted into Postgres. The tables group around the same concepts the runtime exposes.

Runtime configuration lives in `runtime_config`. Service startup stores the current pool, control, guild, and room config there. Domain code reads those records through store methods, the same way it reads room controls, voice state, jobs, and automation records.

Voice tables describe rooms, room controls, raw Discord voice-state rows, voice bot states, voice assignments, capture runs, capture sessions, and occupancy snapshots. `bot_states` records observed bot health and Discord location. `assignments` records the durable room-to-bot binding and its lifecycle. `capture_sessions` records live capture observations from the Discord adapter. Together they answer which room exists, which control markers are active, which bot is assigned, who is present, which capture run is active, and when a session began or ended.

Timeline and transcript tables store events, conversations, materialized windows, publications, and authoritative spans. Speech arrives as `speech_segment` events. Windows select intervals over those events. Publications preserve draft and refined artifacts. Authoritative spans let renderers prefer refined text for covered time ranges while retaining the underlying draft events.

Jobs use a projected row plus a typed payload blob.

```text
jobs
  indexed scheduling, state, scope, lineage, and filter fields

job_payloads
  bincode-encoded typed Job payload and metadata

job_dependencies
  parent/child edges and resolution policy
```

That shape lets the scheduler claim due work by SQL projection and lets handlers recover the typed Rust payload. Waiting resolution is also storage-driven: the resolver reads waiting parents, summarizes terminal children, and either requeues parents that need domain-specific resume behavior or resolves the parent from child outcomes.

Automations and agent sessions follow the same pattern. Each has queryable projections for routing, expiry, scope, and state, plus typed payload data that Rust code validates and executes.

## Code Layout

The timeline package separates durable contracts, store primitives, and rendered views.

`timeline/schema.rs` owns table contracts, index contracts, schema creation, and schema invariant checks. It describes the Postgres shape the runtime expects at startup.

`timeline/store/mod.rs` owns the `TimelineStore` handle, constructors, artifact path helpers, and shared store types. Targeted store modules under `timeline/store/` hold read and write primitives for jobs, events, voice state, room controls, members, automations, runtime config, agent sessions, maintenance, and transcripts.

`timeline/views/` owns read-only projection helpers for status, history, jobs, members, and debug output. Views coalesce facts across store modules into HTTP, CLI, dashboard, and agent-facing JSON. Canonical state remains in Postgres and artifact files.

`timeline/util.rs` contains parsing, time, formatting, hashing, audio, and event payload helpers used by the store and views.

## Artifact Root

The artifact tree holds files that are part of durable runtime state.

```text
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
      elevenlabs.raw.json
      metadata.json
```

Job payloads and timeline events store the paths and checksums needed to interpret these files. Users and agents normally read them through Clankcord commands and rendered views, which keeps the storage layout under runtime ownership.

## Events And Views

Timeline events are JSONB records with stable projections for room, time, event kind, capture run, speaker, and text. Speech, wake detections, Discord text ingress, slash commands, agent-session creation, automation firing, job creation, occupancy changes, participant transitions, forget, retention, publication, and refinement all enter through this event stream.

The store loads ranges by guild, channel, time window, event kinds, capture run, and forgotten-state filtering. Timeline tails, transcript rendering, conversation lists, participant traces, context resolution, and dashboard diagnostics are all derived from these stored events and the records around them.

Rendered views are projections. A conversation is a view over timeline state. A transcript window is a materialized selection over events and spans. The dashboard combines jobs, events, sessions, automations, publications, and artifacts into an operator view. These views can change shape as presentation needs change; the stored facts remain the authority they render from.

## Runtime Execution

Postgres stores orchestration facts and durable state, including runtime config, room controls, voice bot state, voice assignments, capture sessions, jobs, events, automations, agent sessions, and publication state. Active Discord voice clients, packet buffers, playback handles, and gateway clients belong in the service process. The runtime works by reading durable facts through the store, executing a typed handler, and committing the resulting job, control, assignment, session, event, or artifact state back into the store.
