# Adapters

Adapters are Clankcord's boundary with external systems. They speak HTTP, Discord gateway and voice protocols, Discord text and forum APIs, local file and audio mechanics, STT providers, wake-detection providers, and the Codex process interface. Runtime domain code owns policy and orchestration. Adapters translate external events into jobs and fulfill adapter-shaped jobs when the scheduler routes work to them.

The boundary is narrow by design. Intake adapters receive outside events and submit typed runtime jobs. Effect adapters expose a focused API or execute concrete adapter jobs. The runtime owns routing, retries, state transitions, confirmations, timeline authority, and follow-up job creation. The adapter owns the mechanics of the outside system.

```text
external system
      |
      v
adapter
      |
      +--> submit typed job to runtime intake
      |
      +--> fulfill adapter job claimed by scheduler
```

## Adapter Jobs

The runtime executor reaches adapters through the `RuntimeAdapterJobs` trait. In the live service, `Arc<LiveVoiceAdapter>` fulfills Discord voice and text jobs that require live Discord capabilities. It also returns a narrow Discord voice status snapshot when a runtime-owned maintenance job asks for live bot and capture-session state.

The adapter job set includes Discord voice join and leave, mute, audio playback, voice status snapshot, text send, and forum thread creation. These jobs are still ordinary durable jobs. Their payloads, states, dependencies, outputs, and failures are stored like any other work; the executor simply chooses the adapter path for the side effect.

```text
discord_voice_join
discord_voice_leave
discord_voice_mute
discord_voice_play_audio
discord_voice_status_snapshot
discord_text_send
discord_forum_thread_create
```

## HTTP

HTTP is an Axum surface over `RuntimeHandle`. It serves health, status, debug, timeline, transcript, job, automation, confirmation, member, context, participant, response, and dashboard routes. Reads render views from durable state. Mutations parse boundary JSON and submit runtime jobs or runtime-control jobs.

The CLI uses HTTP when it is talking to a running service. That keeps command-line calls, dashboard operations, and Discord-triggered work on the same runtime path after the boundary request is parsed.

## Discord Gateway

The Discord gateway adapter owns text gateway mechanics. It starts the Serenity client, receives messages and interactions, registers slash commands, handles component buttons, sends concrete Discord messages, and creates managed forum threads. Gateway code also acknowledges or defers Discord interactions when the protocol requires it.

Once the gateway has translated a protocol event into a runtime request, the runtime takes over. Slash commands become `discord_slash_command` jobs. Text messages become `discord_text_message` jobs. Confirmation buttons become runtime-control jobs. Response delivery becomes `discord_text_send` only after the `text_delivery` parent resolves the target.

## Discord Voice

Discord voice integration owns Songbird wiring, voice-state tracking, RTP packet capture, per-user buffering, silence handling, WAV artifact creation, wake-probe artifact creation, audio playback, and voice connection mechanics. The live voice adapter holds the process-local handles it needs: the job sink, the timeline store used for maintenance and voice-state recording, live Discord voice clients, active capture sessions, and the speaker profile cache.

By the time the adapter submits an `audio_segment` job, the WAV artifact exists and is ready for STT. The payload carries path, checksum, timing, speaker identity, capture run, and audio format metadata. Wake probes follow the same pattern with rolling WAV artifacts and stream metadata.

## STT, Wake Detection, And Codex

STT and wake detection are provider boundaries. Runtime handlers call the STT adapter while fulfilling `audio_segment` jobs, then write accepted speech into the timeline. Wake probes call the wake provider, append `wake_detected` events for positive detections, and schedule wake activation through runtime jobs.

Codex integration is a process adapter. Runtime agent code builds the prompt, chooses the working directory, sets environment variables, passes the prior Codex session id when present, captures stdout, stderr, JSONL output, timeout state, and usage metadata, then stores typed process results on the agent task. Discord authority lives in Clankcord response commands, `text_delivery`, and Discord text adapter jobs.
