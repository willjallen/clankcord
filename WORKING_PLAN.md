# Working Plan

## v1.0.0 — the overhaul (landed)

The runtime restructure shipped in two passes.

Structure: JobBus submission with the dispatcher wake built into
`TimelineStore::create_job` (the intake channel is deleted); the exhaustive
`model/job/spec.rs` policy table with a macro-declared JobKind; ports below
domain and adapters (DiscordApi, Transcriber, WakeDetector, the voice DTOs);
Ctx free-function handlers replacing the 30-file impl-Runtime surface;
views and the mux planner out of the persistence layer; the engine module
owning scheduler, dispatcher, and one exhaustive payload route; app/ as the
composition layer (service, HTTP, ops telemetry); model/ as the durable
vocabulary; the flat runtime re-export surface deleted so imports name real
homes; clippy clean across all targets with documented allows only.

Behavior: durable wake circuit with a leased half-open slot; job_schedules
as the single recurring-work clock; typed AgentTaskOutcome with the
two-phase agent machine and the reply delivered as a child job; member_sync;
honest interrupted-task recovery; the Thread session mis-scoping fix; the
in-memory AgentSession shadow deleted. The v1_0_0 migration bridges a live
v0.13.0 database, including the v8→v9 payload blob re-encode.

Voice: serenity stops at the connection layer — gateway events arrive as
ports::voice DTOs, cache/http walks are transport functions returning plain
facts, and the bot registry names its boundary (VoiceBotState vs private
transport handles, accessed as client.state.*).

## v1.1.0 — staged work

1. **Voice registry extraction.** Introduce a VoiceTransport trait over the
   connection layer's control surface (join/leave/play/mute/deafen, the
   collect/probe/resolve transport queries, client lifecycle), move
   VoiceBotState and the live/capture/session files to
   `runtime/domain/voice/capture`, and reduce `adapters/discord/voice` to
   the serenity/songbird connection. The state split and DTO seams landed
   in v1.0.0 make this a mechanical move; it waits on live voice
   verification. Follow-ups behind it: capture-session lifecycle state in
   store rows (so wake activation reads live liveness instead of
   flush-cadence snapshots) and WAV writes off the 20ms tick path.
2. **Single-key event schema.** Events still carry `event_kind`+`kind` and
   voice events duplicate snake_case/camelCase fields. A data migration
   over all timeline events plus every producer and reader; ship alone.
3. **Dashboard payload consolidations.** The five JSON compaction
   implementations have diverged into different truncation contracts
   (Null vs `{"truncated": true}` markers, key omission, limits); merging
   them changes payload shapes the dashboard frontend reads, so it ships
   with frontend verification. Same posture for replacing the automation
   spec's hand-rolled JSON validator with serde: it changes the documented
   `clankcord automations spec` error surface.
4. **Parameter structs** for the signatures carrying
   `#[allow(clippy::too_many_arguments)]`.
