# Scheduled Work

Every recurring unit of work in the runtime is one row in the `job_schedules` table. There is no other cadence clock: maintenance no longer reschedules itself and no handler fans out recurring children from a definitions list. Stored automations keep a cursor over the events they have seen — that cursor is content position, not cadence; the evaluation job that advances it is minted by a schedule row like everything else.

## The Table

A schedule row declares:

- `schedule_id` — a stable name (`runtime_maintenance`, `ephemeral_job_gc`, …).
- `kind` — the `JobKind` the schedule mints.
- `payload_json` — the inputs the job is built from (for example `{"guild_id": "…"}` for `member_sync`).
- `interval_ms` — the cadence, floored at one second.
- `enabled` — a schedule can be paused without being deleted.
- `next_due_at_ms`, `last_submitted_at_ms`, `last_job_id` — the durable cadence position and the audit trail back into the jobs table.

## Evaluation

The dispatch loop calls `engine::schedules::run_due_schedules` on every pass. Due rows are claimed in a single atomic UPDATE that also advances `next_due_at_ms` to `now + interval_ms`, so a schedule fires exactly once per due window and an outage yields one catch-up run rather than a backlog. Claimed rows become jobs through `engine::schedules::schedule_job` and enter the system through the bus like all other work. The loop parks until the earlier of the next queued job's ready time and the next enabled schedule's due time, so a due schedule wakes an otherwise idle process.

Only kinds declared in `schedule_job` are schedulable; asking for anything else fails loudly at submission and is recorded on the run report.

## Built-in Schedules

`engine::schedules::ensure_default_schedules` runs at boot and declares six rows at the runtime maintenance interval: `runtime_maintenance` (retention passes, transcription slot recovery, thread-title refresh evaluation), `voice_status_sync`, `automation_evaluation`, `agent_session_retirement`, `stale_wake_probe_sweep`, and `ephemeral_job_gc`. Declaration is idempotent: an existing row keeps its cadence position and adopts the configured interval.

## Adding an Automated Task

A future automated task — a conversation summarizer, a memory backend sync, any cron-style job — is three declarations:

1. A `JobKind` with its payload, constructor, and entry in the exhaustive `model/job/spec.rs` policy table. The compiler enforces completeness; a missing spec entry does not build.
2. A handler routed from the execution match, written as a free function over `Ctx`.
3. A schedule row, upserted at boot or by an operator: `store.upsert_job_schedule(id, kind, payload, interval_ms, enabled)`, plus the payload-construction arm in `engine::schedules::schedule_job`.

The task then inherits everything jobs already have: durable state and outputs, dependency edges, dashboard visibility, retry and recovery semantics, and the timeline audit trail. `member_sync` is the model citizen: demand-driven submission from the members views when the table is stale, deduplicated per guild by ordering key, and equally usable as a periodic schedule row.
