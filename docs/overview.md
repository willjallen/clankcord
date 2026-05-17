# Clankcord Overview

The `docs/` tree is the entry point for the repository. It describes the system that exists in Rust today. The Rust implementation remains the final authority for behavior, names, states, and contracts.

Clankcord is a local Rust runtime for Discord voice memory and Codex-backed assistance. Discord voice rooms, Discord text surfaces, the CLI, HTTP routes, automations, and agent tools all enter the same runtime. Once work crosses that boundary, it becomes a typed job. Jobs write timeline history, create dependent jobs, call adapters for external effects, and leave durable records that can be inspected later.

```text
Discord / CLI / HTTP / agent tools
        |
        v
typed runtime jobs
        |
        v
Postgres-backed timeline
        |
        +--> runtime domain handlers choose the next work
        +--> adapters perform Discord, STT, wake, Codex, and file effects
        |
        v
rendered CLI, HTTP, dashboard, and Discord views
```

The persistent service starts an HTTP API, a Discord text gateway client, a pool of dedicated Discord voice clients, live capture loops, a hot dispatcher, and recurring runtime maintenance jobs. The voice side buffers per-speaker PCM, writes ready WAV artifacts, and emits `wake_probe` and `audio_segment` jobs. Runtime jobs then handle wake activation, STT, transcript publication, room placement, response delivery, agent sessions, agent typing indicators, agent session retirement, automation evaluation, voice status sync, stale-job sweeps, and ephemeral job garbage collection.

Postgres is the durable store. It holds room state, voice bot state, voice assignments, capture sessions, capture runs, timeline events, jobs, dependency edges, automations, agent sessions, transcript windows, publications, authoritative spans, and query projections. Files are durable artifacts referenced by those records. Audio jobs reference WAV files with checksums. Agent jobs reference prompt, result, and raw-output files. Transcript publications reference draft, refined, mixed-audio, metadata, and speaker-alignment artifacts.

## The Mental Model

Clankcord is job-centric. A task starts as a job; when that task needs another piece of work, it creates another job. A wake probe can create a wake activation. A wake activation can create voice cue playback and an agent task. An agent task creates Discord typing start and stop jobs around the Codex process. A room-placement job can create a Discord voice join. A text-delivery job can create a concrete Discord message send. Each transition is represented by job rows, dependency edges, timeline events, outputs, and artifact paths.

Recovery and debugging start with those durable records. When work is slow or broken, inspect the job, its state, its dependency edges, its output or failure metadata, and nearby timeline events. The parent/child structure gives visibility into the exact transition. Latency comes from concrete operations: a provider call, a Discord API call, an adapter lock, a WAV write, a Postgres query, scheduler ordering, a ready-time delay, or another measurable operation.

Runtime code owns policy. Adapters own external mechanics. A Discord gateway module acknowledges an interaction and submits a slash-command job. A voice adapter joins a channel, captures packets, writes WAVs, and plays cues. Codex integration runs a process and returns stdout, stderr, session metadata, and model output. Runtime domain handlers decide placement, wake activation, agent routing, transcript publication, response delivery, forget, retention, and follow-up work.

## Reading Order

The architecture sequence starts with jobs because every subsystem eventually touches them. Runtime service, timeline store, and database architecture define process lifetime, durability, and storage behavior. Adapter, voice, agent, automation, command, transcript, and privacy chapters then describe the main workflows.

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

Reference documents live under [1-reference](1-reference/README.md). They describe exact JSON and CLI-facing contracts, including the automation spec printed by `clankcord automations spec`.

## Boundaries

Boundary validation belongs at HTTP, CLI JSON, Discord, file, provider, and process edges. After data becomes a typed Rust job or runtime record, that type is the internal contract.

New durable execution enters through jobs. New external effects enter through adapter-shaped jobs or narrow adapter calls from the runtime domain that owns the job. New query surfaces render views from the timeline. The architecture has one durable authority, one runtime job model, and explicit edges to external systems.
