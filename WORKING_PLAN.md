# Working Plan

## v1.0.0 — the overhaul (landed)

The runtime restructure shipped: JobBus submission funnel with structural
dispatcher wake, the exhaustive JobSpec policy table, ports below domain and
adapters, Ctx free-function handlers replacing the impl-Runtime god object,
views and the mux planner out of the persistence layer, the durable wake
circuit, member_sync, first-class job_schedules as the single recurring-work
clock, typed agent task outcomes with named phases and child-linked reply
delivery, and the v1_0_0 schema migration (v8→v9 blob re-encode, kind purges,
resolution_policy drop) bridging a live v0.13.0 database.

## v1.1.0 — deferred hard cuts

Each item below was audited, designed, and deliberately deferred rather than
landed untested. All are hard cuts; none require compatibility shims.

1. **Voice capture relocation.** live.rs orchestration and the capture
   pipeline move to `runtime/domain/voice/capture`, with the adapter reduced
   to serenity/songbird wiring. Requires two port seams so no adapters→domain
   edge appears: a `VoiceEvents` trait (domain implements; the connection
   layer drives it from songbird callbacks) and a `VoiceControl` trait (the
   connection layer implements; domain calls join/leave/play/mute/deafen).
   Capture-session lifecycle state then moves to store rows so wake
   activation reads live liveness instead of flush-cadence snapshots, and
   probe/segment WAV writes leave the 20ms tick path. This is the remaining
   split-brain called out by the architecture audit; it needs live voice
   verification, which is why it did not ship blind in v1.0.0.
2. **Single-key event schema.** Events still carry `event_kind`+`kind` and
   voice events duplicate snake_case/camelCase fields. Cutting to single
   keys touches every producer and reader plus a data migration over all
   timeline events; ship as its own migration.
3. **Sweep leftovers.** One JSON compaction implementation for the views
   (five copies remain), a `store/artifacts.rs` seam for publication and
   agent prompt/result files, deletion of the built-and-discarded in-memory
   `AgentSession` status struct, and the automations spec's duplicate
   ~590-line JSON validator (serde becomes the boundary parser).
