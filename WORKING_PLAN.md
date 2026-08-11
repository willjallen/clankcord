# Working Plan

Guiding constraint for all work below: laminar, not chaotic; hierarchical, not
spiderwebs. Data and control flow one direction through the layers
(model → store → engine/domain → views/app). No upward calls, no bidirectional
imports, no parallel re-derivations of one truth.

## Cleanup sequence

Source: the 2026-08-10 src/ audit (45 verified findings). One numbered item ≈
one commit; every commit builds clean, passes clippy, and keeps the full suite
green. Tests file by purpose category; regression pins cite fix commits.

### Wave 1 — loud failures, canonical predicates

- [x] 1. `JobState::is_failed()` beside `is_terminal`/`is_cancellable`; delete
  the operations.rs substring heuristic and its OR-fallback over the persisted
  column; dashboard SQL filters on the persisted `failed`/`terminal` columns;
  one definition of "active" everywhere.
- [x] 2. Dispatcher finalizers propagate store-read errors instead of writing a
  stale in-memory snapshot back over the row (dispatcher.rs ×4,
  tasks.rs `unwrap_or(job)`).
- [x] 3. `due_job_kinds` fails loud on an unparseable kind; scheduler drain
  report becomes a typed struct (deletes the untyped `exhausted`/
  `totalScheduled` control flow and the dead fallback re-derivation);
  job-lane capacity clamps defined once.
- [x] 4. Transcript publication fails loud when the draft artifact our own
  store recorded is missing; `read_json_file` deleted (sole caller inlined,
  loud).
- [x] 5. Infallible internal serialization loses its `json!({})` fallbacks;
  control_state propagates `append_event` errors in all four functions.

### Wave 2 — durable contract

- [x] 6. `removed_boolean_slot` deleted from the payload encoding — a
  compatibility shim on a durable contract. If the current blob version is
  still unreleased, amend it in place through the existing migration; else
  bump the blob version with a new migration. Wire-mirror tests updated to
  the chosen path.

### Wave 3 — hierarchy restoration

- [x] 7. Codex wire-format interpretation (parse_codex_trace and friends)
  moves from views/operations.rs to adapters/codex; the always-Null
  rate-limit stub and its dead caller machinery are deleted.
- [x] 8. Durable write paths leave views: job retry into domain; history
  forget/materialize into domain; engine/routes and interaction commands
  import downward only.
- [x] 9. Automation evaluation context built from domain/store facts, not the
  room status view payload.
- [x] 10. Automations persistence (`impl TimelineStore` + raw SQL CRUD) moves
  from domain/automations/spec.rs into store.
- [x] 11. Audio-segment retry policy stops executing inside store; store keeps
  dumb primitives, domain applies policy.
- [x] 12. Job-payload field projection becomes one exhaustive match owned
  beside the payload enum (no wildcard defaults); the missing MemberSync
  `source_job_id` arm is added.
- [x] 13. Persisted types owned by domain (RoomConfig, VoiceBotStatus,
  route-key computation used by model) move into model; imports point one
  way.
- [x] 14. STT/wake ports become real (adapters implement the traits, domain
  consumes them, app injects) or the traits die — no documented fiction.
  The codex adapter surface gets the same decision.
- [x] 15. errors.rs Discord knowledge moves into adapters/discord; the
  crate-root error surface stops encoding one adapter's wire format.
- [x] 16. store/util.rs dissolved to owning layers (time, fs/hash, WAV decode,
  event-payload helpers); verbatim duplicates with src/util.rs collapsed;
  dead pub items (`overlaps`) deleted; single-call-site helpers inlined.
- [x] 17. Transcription execution moves to domain/transcription; voice/capture
  keeps capture.

### Wave 4 — one dispatch spine

- [x] 18. Routing/blocking/executor declarations collapse into the single
  exhaustive payload match in engine; routes and dispatcher derive from it;
  the false compiler-enforcement doc comment goes away with the hazard it
  described.

### Wave 5 — views reseam

- [ ] 19. One timeline-event search SQL builder and one term normalization,
  shared by transcript and dashboard search.
- [ ] 20. One agent-session rollup implementation with one status precedence.
- [ ] 21. Health capability kind-to-bucket mapping defined once; the
  lean/detailed failure-summary pair unified.
- [ ] 22. operations.rs / dashboard.rs reseamed by responsibility (health,
  Postgres diagnostics, agents, timeline); imports between view modules run
  one direction.

### Wave 6 — type discipline

- [ ] 23. Job timestamps typed end-to-end (no ISO-String fields on the core
  record, no parse/format round-trips); time helper API loses its Option
  ceremony; migration if the column representation changes.
- [ ] 24. CommandArguments fully typed: activation and friends become real
  fields; typed→JSON→typed round-trips and alias-key probing deleted.
- [ ] 25. LiveVoiceSession.mode becomes an enum.
- [ ] 26. Artifact IO (WAV encode, sha256, fs::write) leaves the live-session
  mutex critical section on the voice tick path.

### Wave 7 — remainders

- [ ] 27. Dead weight: ChildResolution's unread `child: Job` clone,
  TranscriptionSlotRecord's unread decoded fields, honest
  large_enum_variant justifications (box AudioPipelineOutcome::SegmentReady),
  agent-task history SQL triplication, shared audio-payload struct where it
  pays.
- [ ] 28. docs/ updated to describe the moved code.

## Deferred register (unchanged, out of scope for the cleanup sequence)

1. **Voice registry inversion.** The last structural gap. live.rs still
   lives beside the connection layer because the bot registry map holds
   transport handles. The boundary inside it is already explicit
   (VoiceBotState vs private serenity handles, all access via
   client.state.*; gateway events arrive as ports::voice DTOs; cache/http
   walks are transport functions). Finishing it: a VoiceTransport trait
   over the control surface (join/leave/play/mute/deafen, the
   collect/probe/resolve queries, client lifecycle), the client map moving
   behind it, and live/capture/session relocating to domain/voice/capture.
   Verify against live Discord voice before shipping. Behind it: capture
   lifecycle state into store rows so wake activation reads live liveness
   instead of flush-cadence snapshots. (WAV writes off the tick path is
   now item 26 above.)
2. **Single-key event schema.** Events carry `event_kind`+`kind`; voice
   events duplicate snake_case/camelCase fields. Needs a data migration
   over all timeline events plus every producer and reader.
3. **Dashboard payload consolidations.** The five JSON compaction
   implementations diverged into different truncation contracts; merging
   changes payload shapes the dashboard frontend reads — do it with the
   frontend open. Same for replacing the automation spec's hand-rolled
   JSON validator with serde, which changes the documented
   `clankcord automations spec` error surface.
4. **Parameter structs** for the signatures carrying
   `#[allow(clippy::too_many_arguments)]`.
