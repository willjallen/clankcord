# Privacy And Retention

Privacy controls are runtime state and timeline operations. Clankcord exposes room listening state, pause and deafen controls, confirmation-gated forget, retention sweeps, and publication boundaries through the same job and event model as the rest of the system.

```text
room controls
      |
      +--> pause, resume, deafen
      |
      v
timeline memory
      |
      +--> confirmation-gated forget
      +--> retention sweep
      +--> publication records
```

## Visible State

Room status renders the current voice mode, assigned voice bot, capture run, retention policy, room controls, occupancy payload, live publications, active jobs, active session, and pool capacity. The dashboard and CLI read those status views instead of inferring state from raw files.

The user-facing state is carried through durable or rendered fields. `control.listeningPaused` shows an active room pause marker. `livePublications` shows live draft transcript publications in Discord. `activeJobs` shows queued, running, and waiting work affecting the room. `retentionPolicy` shows draft transcript, source audio, and job metadata retention windows. Status answers what Clankcord is doing in the room: whether a voice bot is assigned, whether listening is paused, whether transcript publication is active, and which jobs are changing state.

## Pause, Resume, And Deafen

`pause_listening` sets `listening_paused_until` on the Postgres room-control record and appends `listening_paused`. The built-in room-placement automation reads that marker from the timeline store, treats the room as undesired for capture while the marker is active, and emits leave work for an assigned voice bot.

`resume_listening` clears `listening_paused_until`, appends `listening_resumed`, and creates an undeafen cue playback job for the active room when applicable.

`deafen_listening` plays the deafen cue and sets the room pause marker for the manual leave cooldown duration. Voice capture drops packets while an active session is in `deafened_paused` mode, and room placement can release the voice bot according to the active controls.

Room control timestamps are stored in the `room_controls` table and pruned when they expire.

```text
auto_join_suppressed_until
manual_hold_until
listening_paused_until
```

## Forget

`forget_window` is a sensitive command, so it enters the confirmation flow. The confirmation job builds a preview from recent speech and transcript events, sends a DM confirmation card, enters `confirmation_pending`, and waits for approve or cancel through runtime control. Approval creates the confirmed command child.

The applied forget operation loads `speech_segment` and `transcript` events for the selected guild, channel, and time range. It marks those events forgotten, removes referenced source audio files when they exist, and appends `forget_applied`.

```text
forget_window
      |
      +--> confirmation_required
              |
              +--> DM confirmation card
              +--> approval runtime_control
              +--> command(forget_window)
                      |
                      +--> mark events forgotten
                      +--> remove source audio files
                      +--> append forget_applied
```

Timeline queries and transcript views use the store's forgotten-state filter. The `forget_applied` event records the selected window, requester, event count, and deleted audio paths.

## Retention

Retention sweep is store maintenance. The sweep marks old draft speech and transcript events as forgotten, deletes referenced source audio files, appends `retention_retired` events per affected channel, and removes old job rows after their retention window.

The default capture-run retention policy is:

```text
draft_transcript_events   7d
source_audio              7d
job_metadata              30d
```

Retention uses the same local delete and forgotten-state mechanism as explicit forget. Publication artifacts under `durable/publications/` remain publication state and are handled through transcript publication and refinement policy.

## Publication Boundary

Draft local speech and source audio are internal memory until a transcript is published or a response is sent to Discord. Publication creates Discord-visible messages, stores Discord thread and message ids, and records publication artifacts. After publication, the local durable record can be withdrawn, refined, or marked with state transitions, while Discord visibility follows the messages that were posted and any copies outside Clankcord.

```text
local timeline memory
      |
      +--> forget and retention
      |
      +--> materialize transcript
              |
              +--> draft artifact
              +--> optional Discord publication
              +--> optional refinement
```

Confirmations, room controls, retention events, and publication records make privacy-relevant actions inspectable through the same timeline and job surfaces used for normal runtime behavior.
