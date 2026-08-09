# Working Plan

## Remaining work

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
   instead of flush-cadence snapshots, and WAV writes off the 20ms tick
   path.
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
