# Clankcord Overview

The `docs/` tree is the entry point for the repository. It describes the system that exists in Rust today. The Rust implementation remains the final authority for behavior, names, states, and contracts.

Clankcord is a local Rust runtime for Discord voice memory and Codex-backed assistance. Discord voice rooms, Discord text surfaces, the CLI, HTTP routes, schedules, automations, and agent tools all enter the same runtime. Once work crosses that boundary, it becomes a typed job. Jobs write timeline history, create dependent jobs, call ports for external effects, and leave durable records that can be inspected later.

```text
Discord / CLI / HTTP / schedules / agent tools
        |
        v
typed runtime jobs  (born at JobBus::submit or JobDecision::WaitFor)
        |
        v
Postgres-backed timeline
        |
        +--> domain handlers (free functions over Ctx) choose the next work
        +--> adapters implement ports for Discord, STT, wake, and Codex effects
        |
        v
rendered CLI, HTTP, dashboard, and Discord views
```

## The Layers

The source tree is the layering. Every arrow points downward; `app/` is the only layer that names concrete adapters.

```text
src/
├── config.rs, time.rs, util.rs   leaves: configuration, the instant and
│                                 duration vocabulary, shared helpers
├── model/                 the durable vocabulary: job kind/payload/output/
│                          record, the JobSpec policy table, runtime scopes,
│                          rooms, voice status, agent session records,
│                          automation specs
├── store/                 Postgres: row mechanics, schema, migrations
├── ports/                 boundary vocabulary: the DiscordApi trait the
│                          engine dispatches through, plus the STT, wake,
│                          and voice DTOs adapters produce
├── engine/                the job engine: JobBus submission, the scheduler
│                          (due kinds -> spec lookup), the one exhaustive
│                          payload route and its finalization policy,
│                          schedule evaluation
├── domain/                job handlers as free functions over Ctx: agents,
│                          automations, ingress, interactions, maintenance,
│                          messaging, rooms, transcription (planning and
│                          execution), transcripts, voice (placement,
│                          playback, capture)
├── views/                 read models over the store, split by
│                          responsibility: render, search, agents,
│                          diagnostics, health, timeline surfaces, and the
│                          dashboard composition on top
├── adapters/              protocol mechanics: Discord gateway, voice
│                          transport and its failure classification, codex
│                          subprocess and trace interpretation, STT and
│                          wake transport
├── app/                   composition roots: the service process, the HTTP
│                          API and dashboard, CLI manuals, process telemetry
└── dashboard/             the operator frontend assets
```

The persistent service (`app/service.rs`) starts an HTTP API, a Discord text gateway client, a pool of dedicated Discord voice clients, live capture loops, and one dispatch loop. The voice side buffers per-speaker PCM, writes ready WAV artifacts, and emits `wake_probe` and `audio_segment` jobs. Runtime jobs then handle wake activation, durable mux planning, muxed transcription, transcript publication, room placement, response delivery, agent sessions, and member synchronization; schedules mint the recurring maintenance, evaluation, and sweep jobs.

Postgres is the durable store. It holds room state, voice bot state, voice assignments, capture sessions, capture runs, timeline events, transcription slots, jobs, dependency edges, job schedules, the wake-provider circuit, automations, agent sessions, transcript windows, publications, and query projections. Files are durable artifacts referenced by those records.

## The Mental Model

Clankcord is job-centric. A job is born in exactly two places: `JobBus::submit` — a durable insert that wakes the dispatcher in the same call — and `JobDecision::WaitFor`, which creates child rows and parks the parent transactionally. The dispatch loop claims due rows with `FOR UPDATE SKIP LOCKED` under per-lane semaphores and ordering-key exclusion, routes each payload to its handler, and records the returned decision. Everything the engine knows about a kind — executor class, lane, ordering key, parent-resume policy, garbage collection, dashboard category — lives in one exhaustive declaration, `model/job/spec.rs`. A kind cannot be silently unschedulable: the scheduler iterates the kinds Postgres reports as due.

Recurring work has one clock: the `job_schedules` table. A row names the kind to mint, the payload to build it from, and the cadence; the engine claims due rows atomically (the reschedule happens in the claiming update, so a schedule fires exactly once per window) and submits through the bus. Adding an automated task — a conversation summarizer, a memory backend sync, any cron-style unit — is: declare the `JobKind` with its spec and handler, teach `engine::schedules::schedule_job` its payload, and upsert a schedule row.

The agent task is an explicit two-phase machine persisted in its metadata: `Dispatch` runs the codex subprocess; `AwaitDelivery` waits, bounded, for the reply. The reply travels codex → `clankcord responses send` → `POST /v1/responses` → a `TextDelivery` job created as a **child** of the agent task, so lineage covers the causal chain and the ordinary parent/child resolution drives completion. `AgentTaskOutcome` records how the task ended as a typed fact; terminal rows are never rewritten, and a restart marks interrupted tasks `interrupted` on the evidence of their delivery children.

Recovery and debugging start with those durable records. When work is slow or broken, inspect the job, its state, its dependency edges, its typed outcome, and nearby timeline events. Latency comes from concrete operations: a provider call, a Discord API call, an adapter lock, a WAV write, a Postgres query, scheduler ordering, a ready-time delay, or another measurable operation.

Domain code owns policy; adapters own external mechanics and hand results across in ports DTOs. The STT drop thresholds, provider-failure retry classification, and the wake circuit breaker live in domain (`domain/transcription`, `domain/voice/capture/wake_circuit`); the wake circuit's state is a Postgres row, so an open circuit survives restart and its half-open probe slot is a leased claim rather than a process flag.

## Reading Order

1. [Jobs](0-architecture/0-0-jobs.md)
2. [Runtime Service](0-architecture/0-1-runtime-service.md)
3. [Timeline Store](0-architecture/0-2-timeline-store.md)
4. [Database Architecture](0-architecture/0-3-database-architecture.md)
5. [Adapters](0-architecture/0-4-adapters.md)
6. [Voice And Wake](0-architecture/0-5-voice-and-wake.md)
7. [Agents And Sessions](0-architecture/0-6-agents-and-sessions.md)
8. [Automations](0-architecture/0-7-automations.md)
9. [Command Surfaces](0-architecture/0-8-command-surfaces.md)
10. [Transcripts And Publications](0-architecture/0-9-transcripts-and-publications.md)
11. [Agent Runtime Contract](0-architecture/0-10-agent-runtime-contract.md)
12. [Privacy And Retention](0-architecture/0-11-privacy-and-retention.md)
13. [Scheduled Work](0-architecture/0-12-scheduled-work.md)

Reference documents live under [1-reference](1-reference/README.md).

## Boundaries

Boundary validation belongs at HTTP, CLI JSON, Discord, file, provider, and process edges. After data becomes a typed Rust job or runtime record, that type is the internal contract.

New durable execution enters through the bus. New external effects enter through port methods implemented by adapters. New recurring work enters through a schedule row. New query surfaces render views from the timeline. The architecture has one durable authority, one runtime job model, one recurring-work clock, and explicit edges to external systems.
