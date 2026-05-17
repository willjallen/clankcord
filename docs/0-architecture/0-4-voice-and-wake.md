# Voice And Wake

Voice work is shared between the Discord voice adapter and runtime domain jobs. The adapter owns live Discord clients, Songbird connections, capture buffers, WAV artifact creation, wake-probe files, and audio playback. Runtime handlers own room placement policy, job orchestration, timeline writes, STT fulfillment, wake activation policy, and the follow-up jobs that connect voice input to agent work.

The split is visible in the data path. Discord sends packets into a live capture session. Capture turns those packets into per-speaker artifacts and durable jobs. Runtime jobs validate artifacts, call providers, append timeline events, schedule activation, and route visible responses back through text delivery.

```text
Discord voice packets
      |
      v
LiveCaptureSession
      |
      +--> per-speaker audio_segment job
      +--> rolling wake_probe job
      |
      v
runtime handlers
      |
      +--> STT and speech_segment events
      +--> wake_detected events
      +--> wake_activation jobs
      +--> agent sessions and responses
```

## Voice Bot Pool

Dedicated Discord voice bot tokens form the capture pool. Each bot reports `VoiceBotStatus` with readiness, Discord user identity, current Discord location, gateway status, receive backend, and the latest adapter error. `VoiceBotStatus` describes the bot process and observed Discord state.

`VoiceAssignment` is the durable room-to-bot binding. It records the guild, voice channel, selected voice bot, Discord bot user id, capture run, reason, and lifecycle state. Active assignment states are `joining`, `capturing`, and `leaving`; terminal states are `ended` and `failed`. Placement, capacity checks, status rendering, and automation availability use active assignments as the source of room ownership.

A room capture has `VoiceCaptureSessionStatus`, which records the live adapter's capture observation: guild, channel, bot identity, capture run, assignment id, participants, capture stats, artifact status, and lifecycle timestamps. Capture sessions describe packet capture. Assignments describe room ownership.

Voice bots are audio capture workers. Agent work begins later, when wake activation, commands, DMs, or managed thread messages resolve to an agent session and create `agent_task` work.

## Room Placement

Room placement decides whether a configured room needs a voice bot. The built-in placement automation reads configured rooms, room controls, active assignments, active capture sessions, active join and leave work, available bots, and duplicate voice-bot sessions. When a room needs a transition, it emits `room_agent_placement`. Active work for the same room suppresses duplicate placement jobs.

Join starts by selecting a room and claiming an assignment in Postgres. The claim transaction selects a ready unassigned bot, creates the capture run, creates the `VoiceAssignment` in `joining`, appends `voice_bot_assigned`, and returns the selected assignment. The placement parent then creates a `discord_voice_join` child. The child creates the live capture session and joins Discord. The session debug notes record when Discord reports the bot's voice state and when the voice driver is ready, so the durable session row shows the gap between visible room presence and playable audio. When the child completes, the parent resumes, records the adapter's bot and session observations, marks the assignment `capturing`, commits occupancy state, optionally plays the join cue, and completes.

```text
room_agent_placement(join)
      |
      +--> claim VoiceAssignment in Postgres
      +--> create capture run
      +--> discord_voice_join
              |
              +--> join Discord
              +--> create live capture session
      |
      +--> mark assignment capturing
      +--> commit durable session state
      +--> optional join cue
```

Leave follows the same durable pattern. The placement parent marks the active assignment `leaving`, can suppress immediate auto-join, creates leave cue playback for active sessions, creates a `discord_voice_leave` child, then resumes to close the capture run, submit final audio segments, mark sessions and assignments ended, and complete.

```text
room_agent_placement(leave)
      |
      +--> mark assignment leaving
      +--> leave cue playback
      +--> discord_voice_leave
      |
      +--> close capture run
      +--> submit final audio_segment jobs
      +--> mark session and assignment ended
```

When a voice transition is slow, inspect the concrete operation on the path: adapter locks, Songbird join or leave calls, audio playback, WAV writes, STT and wake provider calls, Postgres contention, scheduler ordering keys, lane capacity, or configured timers.

Join cue analysis uses the capture-session debug notes for `botVoiceStateAt`, `joinStartedAt`, and `joinReadyAt`, then uses playback job lifecycle timestamps for the durable handoff into cue playback. A first-packet playback marker is the missing measurement for packet-egress timing.

## Capture Pipeline

`LiveCaptureSession` receives Discord voice packets. It filters voice bot users, resolves speaker profiles, records voice-state updates for human users, buffers per-speaker PCM, preserves decode-loss frames as silence where appropriate, commits live capture stats for active sessions, and flushes ready speaker buffers by maximum segment duration or silence timeout.

Each flush writes a per-speaker WAV artifact and emits an `audio_segment` job. The same capture path writes rolling wake-probe WAV artifacts and emits `wake_probe` jobs. By the time either job reaches the scheduler, the referenced artifact exists and the payload carries enough metadata to verify and interpret it.

```text
RTP packet
      |
      v
per-speaker PCM buffer
      |
      +--> speech flush -> WAV -> audio_segment
      |
      +--> wake window  -> WAV -> wake_probe
```

The default capture settings come from config.

```text
transcription.silenceMs          1000 ms
transcription.maxSegmentMs       8000 ms
transcription.minimumUtteranceMs 350 ms
wake.probeMinimumMs              500 ms
wake.probeWindowMs               2500 ms
wake.probeIntervalMs             500 ms
```

## Speech And Wake Jobs

An `audio_segment` job validates the WAV artifact and checksum, calls the STT adapter, handles empty or low-confidence provider results, appends `speech_segment` events for accepted speech, and updates room occupancy with the latest speech time. The payload contains path, checksum, duration, speaker identity, capture run, audio format, sample rate, channel count, and sample width. Audio bytes remain in the referenced WAV file.

A `wake_probe` job validates its wake artifact and checksum, calls the wake adapter, and completes with `no_wake`, `duplicate_wake`, or `wake_detected` data. A positive detection appends a `wake_detected` timeline event and schedules wake activation. Overlapping speaker and probe time suppress duplicate wake events.

Wake probes carry stream identity and timing in stable fields.

```text
stream_id = guild:channel:capture_run:speaker
probe_index
reset_stream
probe_start_time
probe_end_time
source_audio_path
audio_checksum
```

## Wake Activation

`wake_activation` collects the user's request after the wake phrase. The first activation is scheduled from a `wake_detected` event. Activation plays a wake cue through `discord_voice_playback(wake)`, watches the speaker's post-wake speech window, reads committed live capture stats to wait for buffered speaker audio, waits for pending `audio_segment` STT work, closes the request window, plays an acknowledgement cue, and creates `agent_session_start` or `agent_task` when usable request text exists.

`/wake` uses the same activation path. The slash-command ingress appends a manual `wake_detected` event for the invoking user's current voice room, then schedules `wake_activation` from that event. Follow-up, replacement, cue playback, window closing, and agent dispatch follow the normal wake activation rules.

Follow-up wakes can amend an activation before work has spawned. Inside the preempt window, replacement logic can cancel still-cancellable activation work and schedule preempt cue playback. When the request window closes without usable text, the runtime records `wake_activation_no_request`.

The default timing model is:

```text
lookback                         30 seconds
minimum post-wake window          5 seconds
speaker idle                      2 seconds
max activation window            60 seconds
STT settle deadline             120 seconds
additive preempt                 10 seconds
independent activation threshold 45 seconds
```

## Playback

Voice playback is a parent job that creates mute and play-audio children. Cue assets resolve from `voice.sound.dir` in `config.toml`.

```text
discord_voice_playback(cue)
      |
      +--> discord_voice_mute(false)
      +--> discord_voice_play_audio(cue)
```

Cue names map to files:

```text
join      clanky-join.wav
leave     clanky-leave.wav
wake      clanky-wake.wav
ack       clanky-ack.wav
preempt   clanky-preempt.wav
deafen    clanky-deafen.wav
undeafen  clanky-deafen.wav
```
