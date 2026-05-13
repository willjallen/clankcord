# General

- Jobs are the canonical unit of work. Do not move laterally across the codebase and build spider webs. Work moves through the runtime in one direction.
- There is one canonical `Job` object. Constructors may create jobs, but separate job-like records such as `NewJob` are not part of the model.
- When one task conceptually becomes another task, or branches into another task, create a new job. Do not mutate the original job into a different concern.
- Jobs may form a bounded tree: a root job may have children, and those children may have children. No deeper lineage is allowed.
- A job that needs child work before it can finish enters `waiting`; child completion resolves the parent. This is not a general workflow language.
- Job payloads are strongly typed native Rust data. JSON is only a boundary wire format.
- Core runtime code operates on native Rust types. JSON may be parsed at ingress and rendered at egress, but runtime domains must not use `serde_json::Value` as their working data model.
- Job-specific details belong in typed payload or metadata structs, such as `AgentTaskPayload` or agent-task metadata. Do not flatten every possible field onto every job.
- Specialized job metadata is a single typed detail slot, not one optional field per concern.
- The timeline/sqlite database is the canonical history object and is internal to the runtime.
- The runtime should be minimal, directional, and easy to reason about.
- Boundary modules translate between the outside world and runtime APIs. They do not own orchestration.
- `mod.rs` files are maps: module declarations, narrow re-exports, and no authoritative implementation.
- `src/runtime/core/` is the minimal runtime engine: state, lifecycle, intake, dispatch, effect traits, and job maintenance. Do not put domain execution, views, rooms, sessions, agents, or adapter concerns in core.
- Job-consuming or job-emitting runtime domains live under `src/runtime/domain/`: `audio_segments`, `interactions`, `confirmations`, `publication`, `responses`, and similar. Domain modules receive canonical typed jobs; they do not own adapter capture loops or transport state.
- Runtime support/data modules such as `rooms`, `sessions`, `bots`, `views`, `timeline`, `jobs`, and `agents` live at top-level under `src/runtime/`.
- Do not add generic `handlers` or `pipeline` buckets. They hide ownership and imply the wrong mental model.
- Job graph transitions are flexible: any job handler may complete one typed job and emit any next typed job that fits the workflow.
- `src/runtime/views/` owns rendered read APIs for HTTP/CLI/agent tools. Views can read runtime/timeline state, but they are not canonical state.
- `src/runtime/rooms/`, `src/runtime/sessions/`, `src/runtime/bots/`, and `src/runtime/runtime_config.rs` own their domain data and helpers.
- `src/runtime/agents/` owns generic Codex invocation infrastructure. Interaction routing and agent-task execution may call it, but core must not know Codex exists.
- CLI code has one parser surface at the root `clawcord` command. It is an agent tool surface organized by capability: `messages`, `transcripts`, `timeline`, `context`, `participants`, `jobs`, and `rooms`.
- State-changing CLI commands lower to typed `router_command` jobs; read commands query rendered timeline/runtime views. Commands that need terminal output should use a dependent stdout sink job instead of doing runtime work inside command modules.
- `clawcord start` starts the persistent runtime process: HTTP API, Discord listener, room management, job manager, maintainer loop, and live capture loops.
- The persistent process is a runtime service. It owns runtime construction, adapter construction, the async job intake queue, live capture cycles, and maintainer cycles.
- HTTP is a bound adapter over a `RuntimeHandle`. It may parse wire JSON, render HTTP responses, submit jobs, and serve read views. It must not construct the runtime, spawn core lifetime loops, or expose duplicate mutation endpoints that bypass runtime intake.
- Runtime automations are first-class runtime components with a shared typed context and typed output. They are closed over runtime-owned state: timeline, jobs, config, and runtime indexes derived from those sources. Their only mutation is emitting canonical `Job` objects.
- Runtime automations live under `src/runtime/automations/`; do not add one-off automation files directly under `src/runtime/`.
- If automation needs external information, it emits a job to fetch it. The handler may call an adapter, and the result returns through ordinary job state and timeline events before a later automation pass can use it.

## Adapters

- Adapters speak to the outside world: HTTP, CLI, Discord events, DMs, agent chat channels, slash commands, voice connections, local files, model providers, and external tools.
- Intake adapters may create strongly typed jobs and submit them to the runtime intake queue.
- Effect adapters expose small library-like APIs the runtime calls when a job handler needs an external side effect.
- Effect adapters receive typed request objects and return typed result objects. Do not pass `&mut Runtime` into an adapter.
- Adapters must not own job routing, retries, job state transitions, timeline authority, confirmation flow, or follow-up job creation.
- Adapters are not process hosts. They are attached to the runtime service and can be replaced without changing runtime lifetime rules.
- Codex process integration lives under `src/adapters/codex/`. It is a small CLI/process boundary: build command arguments, pass prompts, capture stdout/stderr/final text, enforce timeouts, and return typed process results. It must not know Clawcord jobs, Discord, timeline writes, retries, or routing.
- Discord voice implementation lives under `src/adapters/discord/voice/`. Songbird wiring, voice-state tracking, packet capture, per-user buffering, silence handling, WAV artifact creation, and Discord voice connection mechanics belong there.
- Audio segment jobs must reference fully processed per-speaker audio artifacts. They must not carry raw PCM. By the time Discord voice submits an `audio_segment` job, the WAV file exists, has a checksum, and is ready for STT.
- STT is an adapter under `src/adapters/`, because it talks to an external model/provider. Runtime domain code may call the STT adapter while fulfilling an audio-segment job.
- The Discord voice adapter must not hold a runtime pointer. It may submit jobs through runtime intake and expose snapshot/effect APIs that the runtime service calls.
- `src/runtime/domain/audio_segments` receives typed audio segment jobs and performs runtime-owned fulfillment: artifact validation, STT, timeline writes, and follow-up job emission.

## Runtime Intake

- The runtime has the single arrival point for jobs.
- All intake enters as typed jobs.
- Intake is asynchronous: callers submit to the runtime handle and receive the accepted/job-created result without owning handler execution.
- Adapters that already have a canonical `Job` submit it through the runtime job sink. They do not persist jobs directly.
- Jobs must contain the data required to begin handling. If more work or more data is needed, the runtime creates another associated job.
- Boundary convenience calls, such as HTTP confirmation approval or retry, lower to `RuntimeControl` jobs. They are not permission to call handler methods directly.

## Runtime Job Handling

- Runtime domain modules own job fulfillment: routing, state transitions, retries, cancellation, confirmations, timeline writes, audio segment transcription, transcript refinement, command interpretation, agent-task dispatch, and follow-up job creation.
- A domain executor may call an adapter to perform an external effect, but the adapter is not the executor.
- `src/runtime/agents/` is the runtime agent harness over the Codex adapter. It owns session allocation, session reuse, context packet construction, preflight, active invocation tracking, and conversion between typed runtime requests and Codex adapter calls.
- Automations decide by reading runtime state and writing jobs. Domain executors do work. Adapters perform effects only when called by runtime code. Results return through jobs and timeline events.
- Speech workflows are flexible job graphs, not fixed linear pipelines. Speech capture, transcription, detection, command interpretation, execution, and response are common concerns, but they are not hard-coded phases.
- A domain executor may call a provider or agent, but it advances the workflow by completing its job and emitting the next canonical job or jobs. One job can produce any other job.
- The job chain is explicit:
  - `src/runtime/service.rs` owns process lifetime, adapter construction, the async intake queue, and recurring runtime ticks.
  - `src/runtime/core/execution/intake.rs` accepts canonical `Job` values and persists them to the timeline.
  - `src/runtime/core/execution/dispatcher.rs` selects due jobs, marks them running, completes/fails them, and resolves waiting parents.
  - `src/runtime/core/execution/routes.rs` maps executable job payloads to typed runtime behavior.
  - `src/runtime/core/execution/effects.rs` defines the narrow effect API that adapters implement for external side effects.
- Do not add new job runners under adapters, command modules, or feature folders. Add a job kind, a typed payload, and a `runtime/domain` executor.
- Examples:
  - Voice audio arrives through `src/adapters/discord/voice` as a typed `audio_segment` job that references a ready WAV artifact.
  - An audio segment job may write transcript events and emit router-command jobs, agent-task jobs, response jobs, or nothing.
  - A later detection job may be introduced if routing grows enough to need a separate job boundary, but do not add it before it removes real complexity.
  - A router command job may complete a built-in control directly or create an agent-task/refinement child job.
  - Room agent placement automation may create a room-agent placement job; that handler calls the voice adapter to join or leave.
  - A confirmation job creates a router command child after approval.
  - An agent-task job may call the Codex agent subsystem and then the Discord adapter to publish its result.

## Timeline

- The sqlite timeline is the single canonical history object.
- Timeline writes happen inside runtime domain modules.
- External consumers can query rendered views, but rendered JSON is not the canonical internal object.
