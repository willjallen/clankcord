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

Room status renders the current voice mode, active voice assignment, assigned voice bot, capture run, retention policy, room controls, occupancy payload, live publications, active jobs, active capture session, and pool capacity. The dashboard and CLI read those status views from Postgres-backed runtime views.

The user-facing state is carried through durable or rendered fields. `control.listeningPaused` shows an active room pause marker. `livePublications` shows live draft transcript publications in Discord. `activeJobs` shows queued, running, and waiting work affecting the room. `retentionPolicy` shows transcript event, source audio, and job metadata retention windows. Status answers what Clankcord is doing in the room: whether a voice bot is assigned, whether listening is paused, whether transcript publication is active, and which jobs are changing state.

## Pause, Resume, And Deafen

`pause_listening` sets `listening_paused_until` on the Postgres room-control record and appends `listening_paused`.

`resume_listening` clears `listening_paused_until`, appends `listening_resumed`, creates `discord_voice_deafen(false)` for the active session when one exists, and creates an undeafen cue playback job for the active room when applicable.

`deafen_listening` creates `discord_voice_deafen(true)` for the active session when one exists, plays the deafen cue, and sets the room pause marker for the manual leave cooldown duration. Voice capture drops packets while an active session is in `deafened_paused` mode. The voice bot remains in the room until an explicit leave command or Discord-side disconnect ends the session.

The `/deafen` and `/undeafen` Discord slash commands lower to `deafen_listening` and `resume_listening` for the invoking user's current voice room.

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

Retention sweep is store maintenance. The sweep reads capture-run retention policies and applies each policy axis independently. `transcript_events` controls forgotten-state marking for `speech_segment` and `transcript` timeline rows. `source_audio` controls deletion of WAV files under capture-run scratch audio directories. `job_metadata` controls age-based deletion for terminal non-ephemeral job rows. Ephemeral jobs continue to use their `gc_after_ms` lifecycle.

The default capture-run retention policy is:

```text
transcript_events         forever
source_audio              7d
job_metadata              forever
```

The source-audio pass walks capture-run scratch directories during maintenance. Wake probe and audio segment enqueue paths write the WAV artifact and create their jobs. This keeps live capture latency tied to audio file creation and job insertion. `source_audio = forever` leaves the files in place.

`transcript_events = forever` keeps speech and transcript timeline rows visible. A finite `transcript_events` value uses the same forgotten-state mechanism as explicit forget and appends `retention_retired` for the affected channel. `job_metadata = forever` keeps terminal non-ephemeral job rows. A finite `job_metadata` value deletes eligible terminal non-ephemeral jobs through the retention sweep. Publication artifacts under `durable/publications/` remain publication state and are handled through transcript publication policy.

## Publication Boundary

Local speech and source audio are internal memory until a transcript is published or a response is sent to Discord. Publication creates Discord-visible messages, stores Discord thread and message ids, and records publication artifacts. After publication, the local durable record can be withdrawn or marked with state transitions, while Discord visibility follows the messages that were posted and any copies outside Clankcord.

Agent-authored publication surfaces follow the prompt's interpersonal content policy. Visible responses, generated summaries, transcript attachments, and thread titles silently omit Discord memory that disparages, insults, mocks, gossips about, speculates negatively about, accuses, or mentions an identifiable person in a negative light. Mixed outputs omit only the restricted lines or spans and keep the surrounding allowed material. Direct requests for the restricted material itself receive a brief refusal when no useful allowed content remains.

```text
local timeline memory
      |
      +--> forget and retention policy
      |
      +--> materialize transcript
              |
              +--> draft artifact
              +--> optional Discord publication
```

Confirmations, room controls, retention events, and publication records make privacy-relevant actions inspectable through the same timeline and job surfaces used for normal runtime behavior.
