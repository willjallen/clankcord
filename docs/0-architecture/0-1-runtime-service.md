# Runtime Service

`clankcord start` runs the long-lived service process. The service owns process lifetime, shared handles, live adapters, intake, scheduling, voice capture ticks, maintenance, and HTTP. Durable orchestration still flows through jobs. The service is the host that keeps those loops alive and gives them access to Postgres, the Discord adapters, and runtime configuration.

```text
clankcord start
      |
      v
construct TimelineStore and Runtime
      |
      +--> initialize Postgres schema
      +--> load config, rooms, controls, status, automations
      +--> recover interrupted agent tasks
      +--> build runtime intake and job sink
      +--> build live voice adapter
      +--> seed runtime_maintenance
      |
      v
spawn service loops
      |
      +--> intake
      +--> Discord text gateway
      +--> live voice capture
      +--> dispatcher
      |
      v
HTTP API
```

Startup begins with the timeline store. The store initializes the Postgres schema, then the runtime loads the durable facts that handlers need: configuration-derived maps, configured rooms, voice bot state, active automations, and status snapshots. Room controls remain Postgres records in the timeline store and are read by the handlers and views that need them.

Agent recovery happens during startup because Codex execution crosses a process boundary. A restart can leave an `agent_task` marked `running`. The service inspects interrupted tasks and looks for a text-delivery job submitted by the same source task. A task that already submitted response work can be completed. The remaining interrupted tasks are marked `agent_dispatch_failed`, keeping the interrupted run visible in job inspection.

Once the runtime is constructed, the service creates two handles into the same intake path. `RuntimeHandle` is used by HTTP and direct service callers. `RuntimeJobSink` is used by adapters that submit detached work, such as Discord gateway ingress and live voice capture output. Both handles feed the same channel, and every successful intake wake notifies the dispatcher.

## Intake

The intake loop receives three kinds of submissions: runtime commands, already-built jobs, and runtime-control requests targeting an existing job or confirmation. Command submissions are lowered by runtime domain code into a `command` job or a `confirmation_required` job. Job submissions are persisted as canonical typed jobs. Runtime-control targets are resolved to the target scope and wrapped in a `runtime_control` job.

```text
CLI / HTTP / Discord / voice capture
      |
      v
runtime intake channel
      |
      v
Postgres job row + typed payload blob
      |
      v
dispatcher notification
```

This intake path gives boundary code a narrow contract. Adapters translate external protocol events into typed runtime requests. Domain handlers decide what those requests mean, which jobs exist, and which state transitions are valid.

## Dispatch

The dispatcher runs a hot drain loop. Each drain pass resolves waiting parents with terminal children, claims due queued jobs, and starts the workers allowed by each lane and ordering key. When a worker finishes, it releases its lane permit and wakes the dispatcher again. When ready work is exhausted, the dispatcher sleeps until a notification arrives or the next ready time is reached.

Workers reconstruct a `Runtime` from the shared timeline store when they need domain behavior. That runtime view contains configuration, status snapshots, the automation registry, the agent runtime harness, and the timeline store. Live Discord voice clients remain in the live voice adapter because those are process capabilities, while jobs, room controls, events, automations, sessions, publications, and artifacts remain durable state.

The scheduler uses execution modes to route work through the correct environment. Runtime-exclusive jobs mutate runtime-owned state snapshots and Postgres-backed room controls. Runtime-snapshot jobs work from a reconstructed runtime view. Blocking snapshot jobs cover provider calls, process execution, file work, STT, wake detection, refinement, and Codex. Adapter async jobs perform Discord IO. Runtime maintenance uses a runtime-environment bridge to sync live adapter state into runtime state.

## Live Loops

The Discord text loop starts the gateway client for messages, slash commands, and component interactions. Gateway code handles Discord protocol requirements such as interaction acknowledgements, then submits durable runtime jobs through the job sink. The runtime decides how messages, slash commands, confirmations, and deliveries affect Clankcord state.

The live voice loop ticks every 500 ms by default. It starts missing configured voice clients and asks active capture sessions to flush ready buffers. A flush can produce `audio_segment` jobs for STT and `wake_probe` jobs for wake detection. Those jobs enter through the same sink and scheduler as commands and Discord text work.

Runtime maintenance is represented as `runtime_maintenance`. A maintenance run creates the next maintenance job, syncs live voice state from the adapter, evaluates built-in and stored automations, cancels stale wake probes, fails stale running non-agent jobs, and garbage-collects terminal ephemeral jobs.

```text
runtime_maintenance
      |
      +--> schedule next maintenance run
      +--> sync live voice status
      +--> run automations
      +--> cancel stale wake probes
      +--> fail stale running jobs
      +--> remove retained ephemeral terminal jobs
```

## HTTP

The HTTP adapter attaches after the service loops are spawned. It serves health, status, voice, command, response, automation, timeline, transcript, conversation, context, participant, member, job, confirmation, debug, and dashboard routes over `RuntimeHandle`.

Read routes render views from the timeline store and runtime state. Mutation routes parse boundary JSON and submit jobs or runtime-control requests through runtime intake. The default bind is `0.0.0.0:8091`, configurable through the environment or runtime config.
