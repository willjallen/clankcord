# Working Plan: Generic Runtime Scope Hard Cut

## Objective

Replace legacy voice-as-canonical routing with a generic runtime scope model. Voice remains a first-class scope and voice-specific systems keep explicit voice names. Generic job, session, timeline, HTTP, CLI, dashboard, prompt, and documentation surfaces stop treating `voice_channel_id` as the route identity.

## Non-Negotiables

- Hard cut only for runtime contracts and routes. The 0.3.0 schema migration is intentional and rewrites legacy durable rows into the new projection.
- Keep hot queue paths indexed directly on projected columns. Do not introduce joins or JSON filtering for scheduler, scope listing, or ephemeral cleanup paths.
- Keep voice terminology where the code is actually about Discord voice state, capture, wake/audio, or voice adapter behavior.
- Put tests under `clankcord/tests/`.
- Update docs after the Rust model is implemented.

## Canonical Model

Use a generic scope for routing and storage:

```text
scope_kind: voice_channel | dm | text_channel | thread | runtime
guild_id: optional Discord guild id
scope_id: Discord channel/thread/user id, or a runtime sentinel for runtime work
```

Voice-specific payloads and tables retain `voice_channel_id` where the value is literally a Discord voice channel.

## Performance Contract

Keep these jobs indexes structurally intact because they drive scheduler and cleanup hot paths:

```text
idx_jobs_due_kind(kind, ready_at_ms, created_at_ms, job_id) WHERE state = 'queued'
idx_jobs_active_ordering(ordering_key) WHERE terminal = false AND ordering_key <> ''
idx_jobs_ephemeral_gc(gc_after_ms, job_id) WHERE ephemeral = true AND terminal = true
```

Replace visible/listing scope indexes with exact-scope indexes on:

```text
scope_kind, scope_id, updated_at_ms DESC, job_id
scope_kind, scope_id, state, kind, updated_at_ms DESC
scope_kind, scope_id, kind, updated_at_ms DESC
```

Add guild-oriented indexes only for actual guild-wide queries. Do not place nullable `guild_id` into every exact-scope hot index by default.

## Phases

### 1. Inventory and Cut Boundaries

Status: complete

- Identify all generic uses of `voice_channel_id`.
- Separate actual voice-domain fields from generic routing fields.
- Identify route, dashboard, CLI, docs, tests, and prompt consumers.

### 2. Runtime Scope Type

Status: complete

- Introduce a Rust scope type for generic runtime routing.
- Convert generic job and agent-session records to use scope data.
- Remove DM/thread/text usage that populates fake voice fields.
- Keep voice fields inside voice payloads.

### 3. Pg Schema and Store

Status: complete

- Hard-cut generic tables from `voice_channel_id` to `scope_kind` and `scope_id`.
- Update expected schema, table creation, indexes, projections, inserts, updates, and queries.
- Keep scheduler claim, ordering, and ephemeral GC query plans direct and indexed.

### 4. Domain Routing

Status: complete

- Convert text delivery and typing indicator routing to shared scope/session resolution.
- Convert command request targeting away from `target_voice_channel_id`.
- Convert agent-session resume/start/title-refresh payloads where their field is generic.

### 5. HTTP, CLI, Dashboard

Status: complete

- Drop `/v1/voice/*` routes.
- Move endpoints to `/v1/*`.
- Fold live occupancy into `/v1/status`.
- Rename response fields such as `liveVoiceOccupancy` to generic names.
- Update CLI and dashboard callers.

### 6. Prompts, Env, Docs

Status: complete

- Rename generic agent prompt/env fields to scope terminology.
- Keep explicit voice env only when the session scope is voice.
- Update maintained docs under `docs/`.
- Leave old docs alone unless they are still referenced as current docs.

### 7. Tests and Verification

Status: complete

- Update and add tests under `clankcord/tests/`.
- Run focused tests while refactoring.
- Run the broad Rust test suite before finishing.
- Check docs for stale `/v1/voice` and fake generic `voice_channel_id` usage.

## Current Notes

- Production code compiles with `RuntimeScope` for generic jobs, agent session records, timeline projections, automations, and route-facing HTTP/CLI payloads.
- v0.2.0 remains registered as the payload-envelope migration template. v0.3.0 performs the generic scope projection migration and rewrites affected durable blobs.
- Focused automation tests pass after the automation scope cut.
- Maintained docs under `docs/0-architecture` and `docs/1-reference` describe `scope_kind/scope_id`; `docs/old-docs` is left as archival material.
- Verification complete: formatting, focused automation/session/job/slash/dashboard tests, and the broad Rust suite pass.
- Dashboard/debug job read models expose generic scope data directly. `voice_channel_id` remains only in voice-domain payloads and events.
- Verification refreshed after the dashboard honesty pass: `cargo test` passes.
