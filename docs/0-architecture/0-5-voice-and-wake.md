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

Voice status sync reconciles adapter memory with Discord voice state before writing durable bot and session status. A ready bot that is not joining is checked against Discord for its current guild voice state, using the bot's observed guild or the configured guilds when adapter memory has no guild. When Discord reports that the bot has no voice state, the adapter clears the bot location, finishes the live capture session, and submits final audio segment jobs. The runtime sync then closes capture sessions, capture runs, and assignments that no longer have a matching bot location and capture session.

Voice bots are audio capture workers. Agent work begins later, when wake activation, commands, DMs, or managed thread messages resolve to an agent session and create `agent_task` work.

## Auto Placement

Runtime maintenance evaluates room placement policy through `automation_evaluation`. When pool auto-join is enabled, a configured room with `auto_join = true` receives an available voice bot once its live human participant count reaches `pool.auto_join_min_participants`. An available bot is ready, has no active assignment, and has no current Discord voice channel.

An assigned voice bot leaves an empty room after the room has had no human participants for longer than `pool.auto_leave_empty_seconds`. This empty-room rule applies to every assignment. Automatic placement also releases a room with one human participant when that participant has been deafened for at least `pool.auto_leave_single_deafened_seconds`. A room released by automatic policy receives an auto-join suppression marker for `pool.auto_rejoin_cooldown_seconds` before policy can place a bot there again.

Manual joins and leaves from Discord commands, dashboard actions, CLI, and HTTP create ordinary `room_agent_placement` jobs and set a room override for `pool.manual_override_seconds`. A manual join keeps the bot assigned while the room has a human participant. A manual leave keeps automatic policy out of the room for the override window. After the override expires, placement policy evaluates the room from current voice state.

## Room Placement

Room placement is the explicit job path that moves a voice bot into or out of a configured room. `/join`, `/leave`, CLI commands, HTTP commands, and agent-issued commands create `room_agent_placement` jobs. Active work for the same room suppresses duplicate placement jobs.

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
voice.capture.flush_interval_seconds 0.2 seconds
transcription.silence_ms             1000 ms
transcription.max_segment_ms         8000 ms
transcription.minimum_utterance_ms    350 ms
wake.probe_minimum_ms                 250 ms
wake.probe_window_ms                 2000 ms
wake.probe_interval_ms                500 ms
wake.activation.active_capture_poll_ms 200 ms
stt.retry_backoff_initial_seconds       5 seconds
stt.retry_backoff_max_seconds         300 seconds
```

## Speech And Wake Jobs

An `audio_segment` job validates the WAV artifact and checksum, calls the STT adapter, handles empty or low-confidence provider results, appends `speech_segment` events for accepted speech, and updates room occupancy with the latest speech time. STT timeouts, connection failures, rate limits, and server errors requeue the same job with capped exponential backoff and no attempt limit. Local artifact integrity failures are terminal because the job does not have a valid audio artifact to submit. Speech segment `startedAt` and `endedAt` come from the original segment payload, so late STT completion inserts transcript events at the time the speech occurred while `created_at` records the later insertion time. Wake activation idle timing follows the speaker PCM timestamp, while the later silence-triggered flush controls WAV and STT job creation. The payload contains path, checksum, duration, speaker identity, capture run, audio format, sample rate, channel count, and sample width. Audio bytes remain in the referenced WAV file.

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

`wake_activation` collects the user's request after the wake phrase. The first activation is scheduled from a `wake_detected` event. Activation plays a wake cue through `discord_voice_playback(wake)`, watches the speaker's post-wake speech window, reads committed live capture stats to wait for buffered speaker audio, closes the request window, plays an acknowledgement cue, waits for every same-speaker `audio_segment` overlapping that closed window to finish STT, and creates `agent_session_start` or `agent_task` when usable request text exists.

The normal close path is voice-driven. After the minimum post-wake window has elapsed, wake activation follows the speaker's committed capture stats and speech segments. The window closes once the latest activating-speaker PCM timestamp has been idle for `speaker_idle_seconds`. The closed request time is persisted in the `wake_activation_window_closed` event. The agent request waits on the closed window's overlapping audio jobs for as long as STT needs, including retryable failed audio that maintenance returns to the queue. Audio segments for a speaker with an active wake activation receive priority inside the audio lane when they overlap the active or closed wake window; wake probes keep their own lane and priority model. The maximum activation window remains a long emergency bound around malformed capture state or a permanently active speaker stream.

`/wake` uses the same activation path. The slash-command ingress appends a manual `wake_detected` event for the invoking user's current voice room, then schedules `wake_activation` from that event. Follow-up, replacement, cue playback, window closing, and agent dispatch follow the normal wake activation rules.

Follow-up wakes can amend an activation before work has spawned. Inside the preempt window, replacement logic can cancel still-cancellable activation work and schedule preempt cue playback. When the request window closes without usable text, the runtime records `wake_activation_no_request`.

The default timing model is:

```text
lookback                         30 seconds
minimum post-wake window          5 seconds
speaker idle                      2 seconds
max activation window         86400 seconds
additive preempt                 10 seconds
independent activation threshold 45 seconds
request-audio poll              200 ms
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
