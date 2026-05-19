# Jobs

Jobs are Clankcord's durable execution record. Whenever the runtime needs to remember work, schedule it, retry it, wait on an external system, recover it after a restart, or explain it to an operator, that work is represented as a job. The job row carries the projected state that the scheduler needs, while the typed payload and typed output preserve the domain contract that Rust handlers execute.

```text
incoming request
      |
      v
typed Job payload
      |
      v
durable job row
      |
      v
scheduler claims due work
      |
      v
runtime handler or adapter
      |
      +--> complete with typed output
      +--> fail with typed failure
      +--> wait for time or live state
      +--> wait for child jobs
```

The same model carries very different kinds of work. A configured room can produce a `room_agent_placement` job, which creates a `discord_voice_join` child when a bot needs to enter voice. A response command becomes `text_delivery`, which resolves the target and creates `discord_text_send`. A wake phrase becomes a wake event, a `wake_activation`, cue playback, an agent session, an `agent_task`, Discord typing start and stop jobs, and response delivery. Each step leaves behind a record with a payload, state transition, dependency edge, output, failure, event, or artifact path.

## Record Shape

A job has identity, scope, lifecycle, lineage, scheduling, and result data. The scope identifies the guild, voice channel, requester, route, target, and source job where those concepts apply. The lifecycle records creation, readiness, running, waiting, completion, cancellation, and failure. The lineage fields connect each child to its parent and root so runtime views can show the full chain that produced a result.

The row uses stable projections for scheduling and filtering. The payload blob holds the typed Rust `Job` value. That split matters operationally: Postgres can claim ready work through indexed columns while handlers still recover the exact domain payload they were written for. JSON appears at the boundary and in rendered views; execution moves through Rust enums and structs.

Job states describe lifecycle, not domain-specific causes. Runtime code uses generic terminal states such as `complete`, `failed`, `failed_timeout`, and `cancelled`; job kind, typed output, `metadata.error`, and detail metadata record why that lifecycle transition happened. An agent dispatch failure is an `agent_task` in `failed` state with the dispatch cause recorded in `metadata.error` and `metadata.agent_task.dispatch_error`.

Job kinds cover capture, wake, Discord ingress, text delivery, concrete Discord IO, forum thread creation and rename, Discord typing indicators, agent session startup, agent session sunset, agent session resume, agent tasks, agent thread title refresh, transcript work, confirmations, runtime commands, room placement, voice join and leave, playback, mute and deafen control, runtime control, and runtime background work. High-volume internal kinds such as `audio_segment`, `wake_probe`, `runtime_maintenance`, `voice_status_sync`, `automation_evaluation`, `agent_session_retirement`, `agent_thread_title_refresh`, stale-job sweeps, and ephemeral job garbage collection use the same scheduler and are hidden from normal user-facing job lists unless the caller asks for ephemeral detail.

## Decisions

Handlers finish a dispatch pass by returning a `JobDecision`. The dispatcher applies the state transition centrally, which gives every job kind the same completion, failure, waiting, and child-spawn behavior.

```text
Complete(output)
    stores typed output and marks the job complete

Fail(failure)
    stores typed failure metadata and marks the job failed

Wait
    leaves the job waiting for time, live state, or already-running work

WaitFor(children)
    persists child jobs, records dependency edges, and marks the parent waiting
```

Many handlers finish immediately. Others create children because the parent needs an adapter or another domain operation to complete first. Room placement waits for join, leave, and cue playback children. Text delivery waits for session-thread creation when needed and then for the concrete Discord send. Agent tasks wait for Discord typing start before launching Codex and wait for Discord typing stop before final result handling. Transcript publication waits for forum-thread creation and message chunks. Agent thread title refresh waits for a Discord forum thread rename child. Voice playback waits for mute and play-audio children. Voice status sync waits for a Discord voice status snapshot child before committing durable runtime state. Confirmations, agent session startup, agent session resume, agent tasks, typing indicator jobs, title refresh, and publication jobs resume because child output determines the parent result.

## Dependency Resolution

Dependencies are stored in `job_dependencies`. A dependency connects a parent to a child, records the dependency kind, and carries the `parent_resumes` policy that tells the resolver how to handle terminal children. `create_child_job` attaches the child to the parent, inserts the required edge, and moves the parent into waiting state while the child work remains active.

```text
parent claimed
      |
      v
handler emits children
      |
      v
parent waits
      |
      v
children run as ordinary jobs
      |
      v
children reach terminal states
      |
      v
resolver requeues or resolves the parent
```

The store rejects self-dependencies and rejects an edge that would connect through an existing path back to the parent. The job row also stores `parent_job_id`, `root_job_id`, and `lineage_depth`, giving CLI, HTTP, and dashboard views a cheap way to render the family tree before they inspect dependency records.

Some parents need their handler invoked again after child completion. The resolver requeues those parents so the handler can inspect child output and commit the final domain state. Other parents can be resolved by aggregation: complete children complete the parent, cancelled children cancel the parent, and failed children fail the parent with a summary of child states.

## Scheduling

The scheduler drains durable work in passes. A pass first resolves waiting parents whose children have reached terminal states. It then finds queued jobs whose ready time has arrived, claims them according to execution policy, and spawns workers. When a pass resolves or schedules anything, the dispatcher immediately runs another pass. When ready work is exhausted, the dispatcher sleeps until a notification arrives or the next ready time is due. If due queued work remains after a drain pass, the dispatcher uses a short bounded retry interval and drains again.

Execution policy chooses where a job runs. Runtime-exclusive and runtime-snapshot jobs execute domain code. Runtime-snapshot domain handlers call typed adapter APIs when a job requires Discord IO. Blocking snapshot jobs run provider, process, file, STT, wake, refinement, and Codex work outside async workers. Runtime maintenance is runtime-domain work: `runtime_maintenance` schedules the next tick and submits ordinary background jobs; those jobs then run through the same lanes, ordering keys, dependencies, outputs, and failures as any other work.

Lanes bound concurrency by class of work. Wake probes have separate capacity from audio transcription. Agent tasks have separate capacity from Discord text sends. Voice control has its own lane. Ordering keys serialize work that would race while allowing unrelated work to proceed.

An `agent_task` keeps its `agent:session:<agent_session_id>` ordering key active while it is waiting on typing start or stop children. This keeps one Codex turn at a time bound to a session even though Discord typing is handled by separate Discord text jobs.

```text
wake_probe                wake:stream:<stream_id>
agent_task                agent:session:<agent_session_id>
agent_thread_title_refresh agent:session:<agent_session_id>
agent session sunset      agent:session:<agent_session_id>
voice wake/agent route    agent:route:voice:<guild>:<channel>
DM text ingress           agent:route:dm:<user_id>
session text delivery     text:session_route:<guild>:<channel>
discord_text_send         discord:text:<target-kind>:<target-id>
discord_forum_thread_rename discord:thread:<thread_id>
discord_typing_indicator  discord:typing:source:<source_job_id> or discord:typing:<target-kind>:<target-id>
voice playback/mute/deafen voice:session:<session_id>
discord_voice_join        voice:bot:<bot_id>
room_agent_placement      room:placement:<guild>:<room>
runtime background work   runtime:maintenance
```

Lane capacity and scheduler batch size come from `config.toml`.

```text
[jobs.concurrency]
wake
audio
voice_control
discord_text
refinement
agent
maintenance
general_async

[jobs.batch]
wake
audio
voice_control
discord_text
refinement
agent
maintenance
general_async
```

The audio lane is the local STT backpressure boundary. Its concurrency is set to the provider's transcription capacity, while the wake lane is configured separately for wake probes. `audio_segment` jobs that overlap an active wake activation for the same room and speaker are claimed before ordinary room transcription inside the audio lane. That priority changes claim order only; it does not raise STT concurrency.

STT timeouts, connection failures, rate limits, and server errors requeue `audio_segment` jobs with `next_run_at` using the configured STT retry backoff. Runtime maintenance also requeues retryable failed audio segment jobs so older transient failures return to the durable queue. The segment payload keeps the original speech timestamps, and completed transcript events are inserted at those timestamps even when the job finishes much later.

Every scheduling pass reads active ordering keys from the durable job table before claiming work. Latency analysis starts with the concrete thing on the path: lane capacity, ordering key contention, ready time, provider latency, adapter locks, database contention, artifact encoding, API calls, or configured timers.

Durable job latency fields have precise lifecycle meanings. `created_at_ms` is job insertion time. `ready_at_ms` is the current ready time derived from `next_run_at` or creation time. `started_at_ms` is the first successful claim. `completed_at_ms` is terminal completion. Jobs that resume after children or timers can have a current ready time later than their first start. Debug latency views mark those rows as phase-contaminated for ready-delay and queue metrics and report exclusion counts. Lifetime latency is `created_at_ms` to `completed_at_ms`; start-wall latency is `started_at_ms` to `completed_at_ms` and includes child waits, configured sleeps, provider calls, adapter calls, and scheduler resumes.

## Example Flows

Room placement shows the parent/child model around Discord voice IO. The placement parent chooses the room and bot, creates or closes capture state, and waits while adapter-shaped children perform the Discord operations.

```text
room_agent_placement(join)
      |
      +--> create capture run
      +--> reserve selected bot as joining
      +--> discord_voice_join
              |
              +--> adapter joins Discord
              +--> adapter creates live capture session
              +--> child completes with DiscordVoiceJoinOutput
      |
      +--> parent resumes
      +--> commit session, bot, and occupancy state
      +--> optional join cue playback
      +--> parent completes
```

Text delivery shows target resolution. The parent owns the abstract response target; the child owns the concrete Discord post.

```text
text_delivery
      |
      +--> resolve session, agent-chat, channel, or DM target
      +--> discord_text_send
              |
              +--> adapter chunks or posts a Discord message
              +--> child stores Discord message ids
      |
      +--> parent records delivery metadata
```

Wake handling demonstrates how live audio becomes durable orchestration. Discord PCM is buffered into wake probes, a positive detection schedules activation, activation waits for the post-wake request window, and the agent response returns through text delivery.

```text
Discord PCM
  -> wake_probe
  -> wake_detected event
  -> wake_activation
  -> wake cue playback
  -> request window closes
  -> ack cue playback
  -> agent_session_start or agent_task
  -> text_delivery
  -> discord_text_send
```

The job graph is the operational record for a run. It explains what started, what waited, what external work happened, which durable facts changed, and where a failure entered the chain.
