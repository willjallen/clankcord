# Timeline Store

The timeline store is Clankcord's durable memory. It records runtime config snapshots, events, jobs, dependency edges, voice state, capture runs, transcription slots, agent sessions, automations, transcript windows, publication state, and artifact metadata. Runtime handlers read and write canonical state through this store.

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

The store also owns the artifact root for voice memory and transcript publication. Audio segment jobs refer to ready WAV files with checksums, timing, speaker identity, capture run, and format metadata. Accepted speech is written as `speech_segment` timeline events. Transcript materialization writes durable files under publication directories and stores the paths in publication records.

## Schema Shape

Startup prepares durable storage before runtime config is snapshotted into Postgres. `clankcord_schema_migrations` records applied Clankcord schema migrations by semantic version. The runtime creates this ledger table, ensures the current table set exists, applies pending migrations in version order, creates current indexes, and then checks schema invariants. Discord adapters, job scheduling, HTTP serving, and runtime maintenance start after that gate completes.

Runtime configuration lives in `runtime_config`. Service startup stores the current pool, control, guild, and room config there. Domain code reads those records through store methods, the same way it reads room controls, voice state, jobs, and automation records.

Voice tables describe rooms, room controls, raw Discord voice-state rows, voice bot states, voice assignments, capture runs, capture sessions, and occupancy snapshots. `bot_states` records observed bot health and Discord location. `assignments` records the durable room-to-bot binding and its lifecycle. `capture_sessions` records live capture observations from the Discord adapter. Together they answer which room exists, which control markers are active, which bot is assigned, who is present, which capture run is active, and when a session began or ended.

Timeline and transcript tables store events, conversations, materialized windows, transcription slots, and publications. Speech arrives as `speech_segment` events. Windows select intervals over those events. Publications preserve rendered transcript artifacts.

Jobs use a projected row plus a typed payload blob.

```text
jobs
  indexed scheduling, state, scope, lineage, and filter fields

job_payloads
  versioned typed Job payload and metadata blob

job_dependencies
  parent/child edges and resolution policy
```

That shape lets the scheduler claim due work by SQL projection and lets handlers recover the typed Rust payload. `job_payloads.payload_blob` begins with the `CLANKJOB` envelope and a little-endian payload version, followed by the bincode-encoded `Job`. The envelope is decoded before the typed body, so an unknown payload schema fails at the storage contract boundary. Waiting resolution is also storage-driven: the resolver reads waiting parents, summarizes terminal children, and either requeues parents that need domain-specific resume behavior or resolves the parent from child outcomes.

## Schema Migrations

Timeline migrations are Rust modules under `timeline/migrations/` named for the Clankcord version that activates them. Version `0.2.0` is implemented in `v0_2_0.rs`. Registered durable changes use the same naming pattern, such as `v0_7_0.rs` or `v1_0_0.rs`.

At startup the runtime reads the highest applied version from `clankcord_schema_migrations` and compares it with the running binary version from `Cargo.toml`. An empty ledger is treated as the `0.1.0` baseline. Registered migrations with versions greater than the durable version and less than or equal to the running binary version are applied in semantic-version order. Each migration runs in its own database transaction and inserts its ledger row after the data rewrite succeeds.

The job payload blob version is tied to the running Cargo version by a compile-time assertion in the job record module. The current mapping records `CLANKJOB` version 7 for Clankcord `0.9.0`; changing either value without updating the mapping fails compilation.

The `0.2.0` migration rewrites pre-`0.2.0` job payload blobs into the current `CLANKJOB` envelope and re-upserts job projections through the current Rust job contract. It also normalizes pre-`0.2.0` job projection states that are represented differently by the current runtime.

The `0.4.0` migration enforces the database hard-cut performance contracts. It sets timeline event start and end times to `NOT NULL` after asserting existing rows already carry both projected times. It also asserts that automation and agent-session payload blobs use the current storage envelopes.

The `0.5.0` migration removes the obsolete terminal-job retention index. Retention is policy-driven: transcript events, source audio artifacts, and non-ephemeral job metadata each use the configured capture-run policy, while ephemeral jobs keep their `gc_after_ms` lifecycle.

The `0.6.0` migration rewrites version-3 `CLANKJOB` payload blobs into the current job payload envelope. It records Codex agent invocation reasoning effort and fast-mode metadata on agent-task jobs. Existing version-3 agent-task metadata is decoded through the previous Rust shape, converted into the current `Job` value, and re-encoded under the current envelope version.

The `0.7.0` migration rewrites version-4 `CLANKJOB` payload blobs into version 5. Version 5 adds first-class response attachment metadata to text-delivery and Discord-text-send payloads. Existing version-4 text response payloads are decoded through the previous Rust shape, receive empty attachment vectors, and are re-encoded under the new envelope version.

The `0.8.0` migration rewrites version-5 `CLANKJOB` payload blobs into version 6. Version 6 adds named transcription sources and transcription mux jobs, creates durable transcription slots, removes refinement jobs, and records provider identity on speech events.

The `0.9.0` migration rewrites version-6 `CLANKJOB` payload blobs into version 7. Version 7 adds the durable transcription mux planner job and the slot indexes used to schedule queued, planned, and muxing transcription work from Postgres.

Automations and agent sessions follow the same projection-and-envelope pattern. Automations have queryable projections for expiry, scope, and state, with typed payload bytes under the `CLANKAUT` envelope. Agent sessions have queryable projections for routing, lifecycle cap, retirement, resume lineage, and state, with typed payload bytes under the `CLANKAGS` envelope.

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
      transcript.draft.txt
      speaker_alignment.json
      elevenlabs.raw.json
      metadata.json
```

Job payloads and timeline events store the paths and checksums needed to interpret these files. Users and agents normally read them through Clankcord commands and rendered views, which keeps the storage layout under runtime ownership.

## Events And Views

Timeline events are JSONB records with stable projections for room, non-null start and end time, event kind, capture run, speaker, and text. Speech, wake detections, Discord text ingress, slash commands, feedback submissions, agent-session creation, automation firing, job creation, occupancy changes, participant transitions, forget, retention, and publication all enter through this event stream.

The store loads ranges by guild, channel, time window, event kinds, capture run, and forgotten-state filtering. Timeline tails, transcript rendering, conversation lists, participant traces, context resolution, and dashboard diagnostics are all derived from these stored events and the records around them.

Rendered views are projections. A conversation is a view over timeline state. A transcript window is a materialized selection over events and spans. The dashboard combines jobs, events, sessions, automations, publications, and artifacts into an operator view. These views can change shape as presentation needs change; the stored facts remain the authority they render from.

## Runtime Execution

Postgres stores orchestration facts and durable state, including runtime config, room controls, voice bot state, voice assignments, capture sessions, jobs, events, automations, agent sessions, and publication state. Active Discord voice clients, packet buffers, playback handles, and gateway clients belong in the service process. The runtime works by reading durable facts through the store, executing a typed handler, and committing the resulting job, control, assignment, session, event, or artifact state back into the store.
