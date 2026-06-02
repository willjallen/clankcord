# Adapters

Adapters are Clankcord's boundary with external systems. They speak HTTP, Discord gateway and voice protocols, Discord text and forum APIs, local file and audio mechanics, STT providers, wake-detection providers, and the Codex process interface. Runtime domain code owns policy, orchestration, and job execution. Adapters translate external events into runtime requests and expose typed API functions for domain handlers.

The boundary is narrow by design. Intake adapters receive outside events and submit typed runtime jobs. Effect adapters expose focused APIs for the concrete outside-system operations. The runtime owns routing, retries, state transitions, confirmations, timeline authority, and follow-up job creation. The adapter owns the mechanics of the outside system.

```text
external system
      |
      v
adapter
      |
      +--> submit typed job to runtime intake
      |
      +--> expose typed API called by domain jobs
```

## Adapter APIs

The runtime executor routes every claimed job to a domain handler. When a handler needs Discord IO, it calls the typed Discord runtime API. In the live service, `DiscordRuntimeApi` delegates voice operations and status snapshots to `LiveVoiceAdapter`, and delegates Discord text and forum operations to the gateway API modules.

Discord IO jobs remain ordinary durable jobs. Their payloads, states, dependencies, outputs, and failures are stored like any other work. Their execution semantics live in `runtime/domain/**`; the adapter API performs the external operation requested by that domain handler.

```text
discord_voice_join
discord_voice_leave
discord_voice_mute
discord_voice_deafen
discord_voice_play_audio
discord_voice_status_snapshot
discord_text_send
discord_forum_thread_create
discord_typing_indicator
```

## HTTP

HTTP is an Axum surface over `RuntimeHandle`. It serves health, status, debug, timeline, transcript, job, automation, confirmation, member, context, participant, response, and dashboard routes. Reads render views from durable state. Mutations parse boundary JSON and submit runtime jobs or runtime-control jobs.

The CLI uses HTTP when it is talking to a running service. That keeps command-line calls, dashboard operations, and Discord-triggered work on the same runtime path after the boundary request is parsed.

## Discord Gateway

The Discord gateway adapter owns text gateway mechanics. It starts the Serenity client, receives messages and interactions, registers slash commands, handles component buttons, sends concrete Discord messages, and creates managed forum threads. Gateway code also acknowledges or defers Discord interactions when the protocol requires it.

Once the gateway has translated a protocol event into a runtime request, the runtime takes over. Slash commands become `discord_slash_command` jobs. Text messages become `discord_text_message` jobs. Confirmation buttons become runtime-control jobs. Response delivery becomes `discord_text_send` only after the `text_delivery` parent resolves the target. The domain handler for `discord_text_send` calls the Discord text API to perform the post. The `discord_typing_indicator` domain handler resolves the same Discord target used by the agent session; when that target is concrete, the gateway adapter sends the typing request and maintains the process-local heartbeat until the paired stop action cancels it. Discord API errors carry status and Discord error codes into runtime handlers, and managed-thread operations treat `Unknown Channel`, `Missing Access`, and `Missing Permissions` as external thread-unavailable signals for the owning agent session.

Plain `discord_text_send` payloads use normal JSON requests. Long plain-text payloads are split into markdown-aware Discord messages: the adapter prefers block boundaries, keeps list blocks together when they fit, and balances fenced code blocks by closing and reopening fences across message boundaries. Attachment payloads use Discord's multipart message endpoint. The domain payload carries the local zip path, filename, size, and checksum; the gateway adapter reads the file at send time, builds `payload_json` for the message content and components, adds `files[n]` parts, and records the Discord message id returned by the API.

## Discord Voice

Discord voice integration owns Songbird wiring, voice-state tracking, RTP packet capture, per-user buffering, silence handling, WAV artifact creation, wake-probe artifact creation, audio playback, and voice connection mechanics. The live voice adapter holds the process-local handles it needs: the job sink, the timeline store used for voice-state recording, live Discord voice clients, active capture sessions, and the speaker profile cache.

By the time the adapter submits an `audio_segment` job, the WAV artifact exists and is ready for transcription intake. The payload carries path, checksum, timing, speaker identity, capture run, and audio format metadata. Wake probes follow the same pattern with rolling WAV artifacts and stream metadata.

## STT, Wake Detection, And Codex

STT and wake detection are provider boundaries. `audio_segment` jobs create transcription slots from ready WAV artifacts, `transcription_mux_plan` jobs reserve provider batches in Postgres, and `transcription_mux` jobs call the STT adapter for the active named transcription source before accepted speech is written into the timeline. STT timeouts, connection failures, rate limits, and server errors requeue the mux job with backoff; the retry path keeps the slot payload tied to the original segment so transcript timestamps stay tied to the captured speech. Wake probes call the wake provider, append `wake_detected` events for positive detections, and schedule wake activation through runtime jobs.

Codex integration is a process adapter. Runtime agent code builds the prompt, chooses the working directory, sets environment variables, passes the prior Codex session id when present, captures stdout, stderr, JSONL output, timeout state, and usage metadata, then stores typed process results on the agent task. Agent task Codex invocations run with the session workspace as the working directory, pass model, reasoning-effort, fast-mode, sandbox, and approval settings from `config.toml`, skip the Git repository trust check for that workspace, and ignore user Codex config. This keeps runtime-owned work independent of optional user MCP server configuration while preserving explicit Clankcord deployment settings. MCP token authentication reports are recorded as `agent_mcp_token_warning` timeline events. Discord authority lives in Clankcord response commands, `text_delivery`, and domain-executed Discord text jobs.
