# Database Architecture

Clankcord stores durable runtime state in Postgres through a projection-first model. Hot operational queries read narrow indexed columns, then fetch typed payload blobs after SQL has selected the small row set needed by the caller. The database is the execution substrate for scheduling, timeline reads, runtime views, retention, and diagnostics.

```text
runtime operation
      |
      v
SQL predicate over projected columns
      |
      v
B-tree range scan or point lookup
      |
      v
small row set
      |
      +--> typed blob decode
      +--> JSON event render
      +--> artifact file read
```

The durable store is designed around predictable latency as table size grows. A query that touches a fixed number of index pages and returns a bounded number of rows has stable operational behavior even when the relation grows from megabytes to gigabytes. A query that scans historical rows, fetches large payloads, or decodes data before filtering scales with retained history and eventually turns database size into user-visible latency through heap reads, TOAST fetches, decompression, allocator work, and Rust deserialization.

## Storage Model

Postgres stores table rows in heap pages and stores B-tree indexes in separate page trees. A heap page is an 8 KiB unit of cache, IO, and vacuum work. Narrow rows fit many tuples per page, which improves cache density and reduces the number of pages touched during point lookups, range scans, updates, and autovacuum passes. Wide `jsonb` and `bytea` values can move into TOAST storage, adding extra page fetches and decompression work when a query reads them.

Clankcord keeps scheduling and filtering data in projected columns. Jobs expose `kind`, `state`, `terminal`, `failed`, `ephemeral`, `scope_kind`, `guild_id`, `scope_id`, lifecycle timestamps, lineage, lane, ordering key, source job id, stream id, speaker id, and segment end time in the `jobs` table. The complete typed `Job` lives in `job_payloads.payload_blob`. Timeline events project scope, kind, time, capture run, conversation, speaker, text, and forgotten state while retaining the JSON event payload. Agent sessions project route, Discord thread, lifecycle cap, retirement state, and resume lineage. Automations project scope, state, idempotency, expiry, and fire limits. Both store typed records in payload blobs.

```text
narrow projection table
  stable SQL predicates
  index keys
  scheduler state
  view filters

wide payload storage
  typed Rust record
  complete metadata
  domain-specific details
  decoded after selection
```

The projection is the database contract. Rust owns typed domain records. Postgres owns selection, ordering, concurrency, and durability. The boundary keeps hot paths on compact data and places rich payload decoding at the end of the query pipeline.

## Latency Shape

Operational reads target `O(log N + K)` behavior: a shallow B-tree walk over `N` rows plus `K` selected rows. The upper B-tree pages normally stay hot in shared buffers and the operating system page cache, so the practical cost is dominated by leaf pages and the small heap or payload rows returned. This is the database equivalent of keeping the working set in cache and keeping result size explicit.

```text
projection-first path
  index root page
    -> internal page
      -> leaf page range
        -> K heap rows
          -> K payload blobs when needed

history-scaled path
  many heap pages
    -> many TOAST fetches
      -> many Rust decodes
        -> discard most rows
```

The scheduler, room status views, command interaction context, dashboard health views, and transcript range readers are hot paths. They select by projected columns and apply limits before reading payload blobs. Historical inspection commands read broader ranges through explicit operator actions with bounded request limits.

## Index Contracts

Indexes are shaped around access patterns. The leftmost columns of each B-tree match the equality predicates and ordering requirements of the query that uses it. Partial indexes include predicates that are stated directly in the query text, because the Postgres planner uses SQL predicates rather than Rust enum semantics.

```text
queued scheduler claim
  WHERE state = 'queued'
    AND kind = $kind
    AND ready_at_ms <= $now
  ORDER BY ready_at_ms, created_at_ms, job_id

index
  jobs(kind, ready_at_ms, created_at_ms, job_id)
  WHERE state = 'queued'

global scheduler clock query
  WHERE state = 'queued'
  ORDER BY ready_at_ms, created_at_ms, job_id

index
  jobs(ready_at_ms, created_at_ms, job_id, kind)
  WHERE state = 'queued'
```

Boolean projections such as `terminal`, `failed`, and `ephemeral` exist because they make partial indexes and operational filters planner-visible. The state machine lives in Rust. Postgres can use a partial index when the SQL predicate carries the same projected boolean fact, such as `terminal = FALSE` for active-job indexes or `ephemeral = FALSE` for visible-job indexes.

Command interaction context uses scoped partial indexes. Cancellable-job context reads visible, active, cancellable rows for one voice scope ordered by recent update time. Recent agent-task context reads scoped `agent_task` rows through partial indexes, with requester-owned rows queried first and the remaining result slots filled from non-requester rows. Both paths bound payload decoding by the command context limit.

Timeline event indexes follow the same rule. Room-time reads use scope and timestamp columns in index order. Kind-filtered reads add `event_kind` after the room scope. Capture-run, conversation, and speaker reads use their projected identifiers first, then event time. Range predicates operate on stored timestamp columns so the planner can use ordinary B-tree ordering without expression indexes.

## Scheduler State

The job scheduler is a database-backed queue. The `jobs` table is the queue projection, `job_payloads` is the typed body, and `job_dependencies` is the parent-child graph. Claiming work is a row-lock operation over due queued rows.

```text
scheduler pass
      |
      +--> resolve waiting parents from dependency rows
      |
      +--> read active ordering keys
      |
      +--> claim due queued jobs
              |
              +--> SELECT projected rows + payload
              +--> FOR UPDATE SKIP LOCKED
              +--> mark selected jobs running
              +--> update payload blobs
```

`FOR UPDATE SKIP LOCKED` lets multiple workers claim different queued rows while keeping claim latency bounded by the current due set. Ordering keys serialize work that races on an external or logical resource: one agent session, one voice session, one text target, one wake stream, or runtime maintenance. The database stores the active ordering projection so the scheduler can block conflicting work before spawning handlers.

Waiting parents stay in the job table with dependency edges to children. The resolver reads waiting parents and child summaries from projected rows before loading full payloads for parents that resume or resolve. Parent-child architecture is durable orchestration. Latency analysis names the concrete operation on the path, such as lock contention, blob fetch and decode, storage contention, provider calls, adapter locks, scheduler ordering, artifact IO, or configured timers.

## Timeline Reads

Timeline events are append-heavy and range-read-heavy. Event ingestion writes one row with projected time, scope, kind, speaker, capture run, conversation, text, forgotten state, and JSON payload. Views read ordered ranges for tails, transcript rendering, publication windows, automation triggers, participant traces, and diagnostics.

```text
timeline_events
  sequence
  event_id
  scope_kind
  guild_id
  scope_id
  event_kind
  started_at_ms
  ended_at_ms
  created_at_ms
  capture_run_id
  conversation_id
  speaker_user_id
  text
  forgotten
  payload_json
```

The primary timeline access pattern is a room-scoped time range. Room-time indexes are defined over `(scope_kind, scope_id, started_at_ms, sequence)` with optional kind, capture-run, conversation, and speaker indexes for narrower views. `started_at_ms` and `ended_at_ms` are required columns. Instant events store the same value in both columns. The range loader uses `ended_at_ms > range_start` and `started_at_ms < range_end`, then orders by `started_at_ms`, `sequence`, and `event_id`. Draft transcript search loads speech and transcript event ranges, then matches event text in Rust. Refined transcript search reads authoritative span metadata from Postgres and text artifacts from durable publication storage.

Forgotten events remain rows with `forgotten = TRUE`. That keeps sequence, audit, and retention behavior explicit while ordinary timeline readers filter them with a projected boolean. Forget and retention operations update the projection first, then append privacy-relevant events that describe the operation.

## Blob Contracts

Typed job blobs carry an explicit storage envelope. The envelope identifies the job record family and payload version before the bincode body. The decoder checks the envelope at the storage boundary and fails when the durable bytes diverge from the current contract.

```text
payload_blob
  magic bytes
  little-endian payload version
  bincode body
```

Job payload blobs use the `CLANKJOB` envelope. Automation payload blobs use `CLANKAUT`. Agent session payload blobs use `CLANKAGS`. The header is a hard boundary between durable bytes and Rust structs. Startup, migrations, and runtime reads get a deterministic failure point before domain code executes a typed record.

Payload blobs are loaded by identity or after a projection query over the corresponding record table. Fields that affect scheduling, routing, lifecycle, retention, idempotency, or user-facing query filters are projected into columns.

## JSONB Boundaries

JSONB stores timeline event payloads, runtime snapshots, room controls, voice state snapshots, occupancy snapshots, publications, windows, authoritative spans, and member payloads. JSONB is used when the caller renders or preserves an external-facing record. Queryable facts inside those records are also projected as columns when they participate in hot filters, ordering, locking, or joins.

Hot scheduling paths use projected columns. JSON operators appear in narrow places where the relation is naturally small or the query is operationally bounded, such as selecting ready voice bots from `bot_states`. Larger relations expose their hot facts as columns.

## Retention And Table Growth

Retention keeps operational tables within their intended working set. Ephemeral terminal jobs carry `gc_after_ms` and are deleted through a targeted partial index. The broader retention sweep applies a seven-day cutoff to draft speech and transcript events and a thirty-day creation-time cutoff to terminal job rows through the terminal-retention index. Draft transcript events and source audio expire through forgotten-state marking and artifact deletion. Publication artifacts remain durable publication state.

```text
ephemeral job
  terminal = TRUE
  gc_after_ms <= now
      -> delete job row
      -> cascade job payload and dependency edges

terminal retained job
  terminal = TRUE
  created_at_ms < cutoff
      -> delete job row
      -> cascade job payload and dependency edges

old draft transcript event
  event_kind in speech/transcript
  forgotten = FALSE
  started_at_ms < cutoff
      -> delete source audio artifact
      -> set forgotten = TRUE
      -> append retention_retired
```

Queued, running, waiting, confirmation-pending, and cancel-requested jobs are durable coordination state while they remain in the job table. Maintenance deletions cascade through `job_payloads` and `job_dependencies` through the foreign keys defined in the schema.

## Concurrency And Maintenance

Postgres uses MVCC, so updates create new tuple versions and leave old versions for vacuum. High-churn tables such as `jobs`, `voice_states`, `capture_sessions`, `assignments`, `room_controls`, and `runtime_status` are designed around narrow updates and bounded row sets. Keeping payload fetches separate from projection updates reduces write amplification on scheduler paths and keeps autovacuum work focused on compact rows.

Locking is deliberate and local. Scheduler claims lock queued job rows. Voice assignment claims lock candidate bot rows while checking active assignments. Voice-state updates lock a single `(guild_id, user_id)` row before writing the current state and deriving transition events. These locks represent concrete shared resources and keep contention visible in Postgres diagnostics.

Operator views expose pool usage, table sizes, row counts, table activity, lock counts, dead tuples, scans, writes, temp files, and deadlocks. These metrics identify concrete causes of latency: lock waits, heap scans, index scans over large ranges, payload fetch volume, storage growth, dead tuple pressure, temp-file sorts, and connection contention.

## Schema Discipline

`timeline/schema.rs` defines the current table and index contract, creates the schema, applies registered migrations, and asserts invariants at startup. The assertion treats stale columns and stale indexes as contract violations. The database shape is a hard runtime dependency.

Schema changes follow the same projection-first rule. A field becomes a column when it affects scheduling, routing, joins, retention, status views, dashboard diagnostics, or bounded user-facing filters. A field remains inside a payload when it is meaningful after the record has already been selected. This keeps the relational shape small, queryable, and tied to real runtime access patterns.
