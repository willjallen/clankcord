# Job-Centric Runtime Refactor

## Goal

Clankcord runtime execution should have one canonical durable unit of work:

```text
Job
```

Anything executable, durable, retryable, waitable, restart-safe, or externally fulfilled
must be represented as a `Job` in the SQLite timeline. The dispatcher should poll the
timeline, claim due jobs, route them to handlers, and then complete, fail, or suspend
those jobs through the same lifecycle.

The current `RuntimeEffect`, `RuntimeEffectResult`, and `RuntimeEffectJob` layer violates
that model. It solved the runtime-lock problem, but it created a second hidden execution
system beside jobs. This refactor removes that hidden system and replaces it with
dependent jobs.

## Hard Rules

- The SQLite timeline is the canonical history and job store.
- The dispatcher polls timeline jobs and routes them to handlers.
- Handlers operate on native Rust types.
- JSON is only allowed at boundaries:
  - CLI input/output.
  - HTTP/dashboard/API input/output.
  - External service request/response bodies.
  - Debug/export views.
- Runtime control flow must not use `serde_json::Value`.
- Any side effect that waits on external work is a job.
- Waiting jobs must not occupy worker slots.
- Dependency graphs are DAGs, not depth-two trees.
- Cycles are rejected.
- Operational limits should be explicit:
  - max descendants per root job.
  - retry limits.
  - expiry.
  - cancellation propagation.

## Current Problem

The room join flow currently looks like this:

```text
Automation
  -> RoomAgentPlacement Job
    -> scheduler claims job
      -> routes.rs
        -> prepare_join_room_effect
          -> RuntimeEffect::JoinRoom
            -> adapter does Discord join
              -> RuntimeEffectResult
                -> commit_claimed_async_effect
                  -> commit_join_room_effect
                    -> complete original job
```

The `RuntimeEffect` layer is effectively a private mini job system. It has its own
request type, result type, commit path, rollback path, and completion renderer. That makes
execution harder to inspect and breaks the rule that timeline jobs are the only durable
execution unit.

## Target Shape

The new flow should be:

```text
Automation
  -> RoomAgentPlacement Job
    -> scheduler claims job
      -> job route table
        -> room placement domain logic
          -> creates DiscordVoiceJoin child Job
          -> parent waits

DiscordVoiceJoin Job
  -> scheduler claims job
    -> job route table
      -> Discord voice adapter handler
        -> adapter joins Discord channel
        -> child completes with typed DiscordVoiceJoinOutput

Dependency resolver
  -> sees child complete
    -> requeues parent RoomAgentPlacement Job
      -> parent resumes
        -> commits runtime session/bot/occupancy state
        -> parent completes
```

The simplified mental model is:

```text
claim job -> handle job -> complete/fail/wait -> wake dependents
```

Everything else is just payload type and handler choice.

## Room Join Lifecycle

```text
clankcord start
    |
    v
RuntimeService::spawn
    |
    +--> intake loop
    |       external submissions -> SQLite timeline jobs
    |
    +--> live voice loop
    |       starts Discord clients
    |       flushes completed audio buffers
    |
    +--> maintainer loop
            |
            v
        run_maintainer_cycle
            |
            +--> executor.schedule_due_jobs()
            |
            +--> sync_voice_adapter_state()
            |
            +--> runtime.run_automations()
            |       |
            |       v
            |   RoomAgentPlacementAutomation::evaluate
            |       |
            |       +--> inspect known rooms
            |       +--> inspect room controls
            |       +--> inspect active sessions
            |       +--> inspect available voice bots
            |       +--> skip if active placement job already exists
            |       |
            |       v
            |   emit RoomAgentPlacement Job
            |       |
            |       v
            |   SQLite timeline
            |       job.kind  = room_agent_placement
            |       job.state = queued
            |
            +--> executor.schedule_due_jobs()
                    |
                    v
                claim queued RoomAgentPlacement job
                    |
                    v
                room placement handler
                    |
                    +--> already assigned?
                    |       |
                    |       v
                    |   complete parent job
                    |
                    +--> no available voice bot?
                    |       |
                    |       v
                    |   complete/fail parent job
                    |
                    +--> otherwise
                            |
                            +--> create capture run
                            +--> reserve selected bot as joining
                            +--> create DiscordVoiceJoin child job
                            +--> parent state = waiting
```

Then the child job runs independently:

```text
DiscordVoiceJoin job
    |
    v
scheduler claims job
    |
    v
Discord voice adapter handler
    |
    +--> find configured Discord voice client
    +--> create adapter capture session
    +--> call Songbird join
    +--> return typed DiscordVoiceJoinOutput
    |
    v
complete child job
    |
    v
dependency resolver wakes parent
```

The parent resumes:

```text
RoomAgentPlacement parent resumes
    |
    v
read typed DiscordVoiceJoinOutput
    |
    +--> copy adapter bot status into runtime
    +--> copy session status into runtime
    +--> set timeline occupancy
    +--> persist status snapshot
    |
    v
complete parent job
```

Failure path:

```text
DiscordVoiceJoin fails
    |
    v
child job failed
    |
    v
parent resumes with failed dependency
    |
    +--> mark bot join failed
    +--> suppress room auto-join temporarily
    +--> close capture run as failed
    |
    v
parent fails or completes with explicit no-join status
```

## Job Decisions

Handlers should return typed decisions:

```rust
enum JobDecision {
    Complete(JobOutput),
    Fail(JobFailure),
    WaitFor(Vec<Job>),
}
```

If a handler needs outside work, it creates child jobs and returns `WaitFor`.
If it can finish immediately, it returns `Complete`.
If it cannot proceed, it returns `Fail`.

Runtime execution should not return `serde_json::Value`.

## Typed Payloads And Outputs

Add adapter-facing job payloads:

```rust
JobPayload::RoomAgentPlacement(RoomAgentPlacementPayload)
JobPayload::DiscordVoiceJoin(DiscordVoiceJoinPayload)
JobPayload::DiscordVoiceLeave(DiscordVoiceLeavePayload)
```

Add matching typed outputs:

```rust
JobOutput::RoomAgentPlacement(RoomAgentPlacementOutput)
JobOutput::DiscordVoiceJoin(DiscordVoiceJoinOutput)
JobOutput::DiscordVoiceLeave(DiscordVoiceLeaveOutput)
```

The room placement parent job owns orchestration:

```text
RoomAgentPlacementPayload
  -> check desired room state
  -> reserve bot/capture run if needed
  -> create DiscordVoiceJoin child job
  -> wait
```

The Discord adapter child job owns the external side effect:

```text
DiscordVoiceJoinPayload
  -> join Discord voice channel
  -> create adapter capture session
  -> complete with DiscordVoiceJoinOutput
```

The parent consumes the child result:

```text
DiscordVoiceJoinOutput
  -> commit bot/session/capture/occupancy state
  -> complete RoomAgentPlacement
```

## Dependency Model

The timeline should support a real job DAG:

```text
parent_job_id
child_job_id
dependency_kind
created_at
resolution_policy
```

Rules:

- Parent jobs become `Waiting` while dependencies are incomplete.
- Child jobs are normal queued jobs.
- Completion or failure of a child wakes affected parents.
- Parent resume receives typed dependency results.
- Children may create further children.
- Cycle creation fails immediately.
- Waiting parents do not consume lane permits.

## Dispatcher Shape

The dispatcher should remain centralized:

```text
schedule_due_jobs
  -> claim due jobs by kind/lane
  -> route to handler
  -> apply JobDecision
  -> wake dependents
```

The route table should map job kinds to handlers:

```text
RuntimeControl       -> runtime control handler
Command              -> command handler
RoomAgentPlacement   -> room placement handler
DiscordVoiceJoin     -> Discord voice adapter handler
DiscordVoiceLeave    -> Discord voice adapter handler
AudioSegment         -> audio handler
WakeActivation       -> wake activation handler
AgentTask            -> agent handler
Response             -> response handler
RefineTranscript     -> refinement handler
```

The adapter should not own runtime lifetime. The dispatcher owns job claiming. The adapter
only fulfills adapter-shaped jobs routed to it.

## Refactor Steps

1. Add typed `JobDecision`.
2. Add typed `JobOutput` and `JobFailure`.
3. Remove `serde_json::Value` from core dispatch completion paths.
4. Add timeline dependency persistence for arbitrary DAGs.
5. Add dependency resume logic.
6. Add `DiscordVoiceJoinPayload` and `DiscordVoiceJoinOutput`.
7. Add `DiscordVoiceLeavePayload` and `DiscordVoiceLeaveOutput`.
8. Convert `RoomAgentPlacement` join from effect-based execution to child-job execution.
9. Convert `RoomAgentPlacement` leave from effect-based execution to child-job execution.
10. Convert command-triggered joins/leaves to use the same job path.
11. Delete `RuntimeEffect`, `RuntimeEffectResult`, and `RuntimeEffectJob`.
12. Delete effect commit/rollback paths.
13. Move JSON rendering to CLI/API/dashboard/debug boundaries.
14. Add tests for parent/child execution, failure, restart, and cycle rejection.

## Required Tests

- Automation emits exactly one room placement job for a room needing a bot.
- Active placement jobs suppress duplicate automation output.
- Room placement parent creates one Discord voice join child and becomes waiting.
- Waiting parent does not block unrelated audio jobs.
- Discord voice join child completion wakes parent.
- Parent resume commits bot/session/occupancy state.
- Discord voice join child failure wakes parent.
- Parent failure rollback marks bot failed, suppresses auto-join, and closes capture run.
- Parent can wait on multiple children.
- Child jobs can create further children.
- Cycle creation is rejected.
- Restart with waiting parent and completed child resumes correctly.
- Restart with waiting parent and failed child resumes correctly.
- JSON is absent from core execution decisions.
- JSON serialization still works at CLI/API/dashboard boundaries.

## Acceptance Criteria

- There is one execution model: jobs.
- The timeline contains every durable unit of work and dependency.
- No runtime effect layer remains.
- External side effects are adapter-facing child jobs.
- Core runtime handlers use native Rust payloads and outputs.
- JSON is only used at ingress, egress, external-service, and debug boundaries.
- The bot join lifecycle can be explained as:

```text
Automation -> Job -> Dispatcher -> Handler -> Child Job -> Adapter -> Child Complete -> Parent Resume
```
