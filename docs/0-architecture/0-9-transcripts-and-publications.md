# Transcripts And Publications

Transcript work turns speech timeline events into materialized windows, Discord publication state, refinement jobs, and authoritative text spans. The speech timeline remains the base record. Windows, publications, and refined spans are durable views over that record, with artifacts written under the timeline store root.

```text
speech_segment events
      |
      v
transcript window
      |
      +--> draft artifact
      +--> optional Discord publication
      +--> optional refinement
              |
              v
        authoritative span
```

## Windows

A transcript window selects a guild, voice channel, start time, and end time. Creating a window loads speech and transcript events for that interval, records the first and last event ids, captures covered capture-run ids and voice-bot ids, and stores the resolved selection as a row with JSON payload.

Windows are used by transcript materialization, context resolution, forget operations, and agent tools. The selection can come from a relative reference such as `-10m`, an absolute time range, or a resolved context reference. The stored window keeps both the resolved times and the original selection reference, so later renderers and agents can explain what was selected.

```text
timeline events
      |
      v
window selection
      |
      +--> start and end timestamps
      +--> event id bounds
      +--> capture runs
      +--> voice bot ids
```

## Draft Materialization

`materialize_transcript` creates a window and renders the selected speech into a draft artifact. The draft and metadata are written under the publication directory.

```text
durable/publications/<publication_id>/transcript.draft.txt
durable/publications/<publication_id>/metadata.json
```

The publication payload records the publication id, window id, guild, voice channel, publish mode, creator, draft path, refinement request, Discord thread id, Discord message ids, and state. Creating the publication appends `publication_created`.

When Discord publication is requested, runtime creates `transcript_publication`. That job creates a managed forum thread, chunks the draft artifact into Discord messages, stores the thread and message ids, and moves the publication state to `draft_published` or `live_draft_published`.

```text
materialize_transcript
      |
      +--> create window
      +--> render draft text
      +--> write publication artifacts
      +--> append publication_created
      |
      +--> transcript_publication
              |
              +--> discord_forum_thread_create
              +--> discord_text_send chunks
              +--> publication state updated
```

## Rendering

Rendering reads the selected events and overlays refined spans when they cover part of the requested interval. Draft events remain stored and debuggable. The rendered view uses authoritative span text for covered ranges and draft speech events for gaps.

JSON rendering includes the window, selected events, and authoritative spans. Text rendering formats speech by timestamp and speaker label. The command surface controls output size: compact JSON is the default view, `--verbose` expands selected records, and `--file ... --format json` writes large windows to disk for agent inspection.

## Refinement

Refinement runs as `refine_transcript`. The job resolves the window, exports the covered source audio into one mixed WAV, submits that file to ElevenLabs speech-to-text, aligns provider speakers to Discord speakers, writes refined artifacts, creates an authoritative span, and updates the publication.

The provider call uses ElevenLabs Speech-to-Text:

```text
POST https://api.elevenlabs.io/v1/speech-to-text
```

The runtime sends `model_id` from `ELEVENLABS_STT_MODEL_ID` or `scribe_v2`, enables diarization, requests word timestamps, estimates `num_speakers` from local speaker segments, and includes job, publication, window, guild, and voice-channel metadata. `ELEVENLABS_STT_WEBHOOK_URL` enables webhook mode; synchronous mode waits for the provider response. `ELEVENLABS_STT_TIMEOUT_SECONDS` controls the blocking request timeout.

Refinement artifacts live in the publication directory.

```text
elevenlabs.raw.json
speaker_alignment.json
transcript.refined.txt
```

Speaker alignment compares provider word timings with local Discord speaker segments from the mixed-audio sidecar. The assignment method uses temporal overlap with greedy one-to-one assignment. Assignments below the confidence threshold remain unresolved. The stored alignment includes provider speaker ids, Discord user ids, speaker labels, confidence, unresolved speakers, and creation time.

After writing `transcript.refined.txt` and `speaker_alignment.json`, the store creates an authoritative span and appends `refinement_completed`. The publication becomes `refined`, with paths to the refined transcript, mixed recording, and speaker alignment. A failed refinement marks the job and publication `failed_draft_retained`, preserving the draft publication state for users.

```text
refine_transcript
      |
      +--> export mixed audio
      +--> ElevenLabs STT
      +--> align provider speakers
      +--> write refined artifacts
      +--> create authoritative span
      +--> update publication
```

## Search And Context

Transcript search, conversation lists, context resolution, and participant traces are read views over timeline events, windows, spans, and room or member state. `transcripts search` can search one room or all channels in a guild. `conversations list` groups stored conversation views. `context resolve` creates or returns a focused window for references such as "the last thing" or "an hour ago". `participants trace` walks voice and speech events for a user across channels and can include speech snippets when requested.

These surfaces are designed for agents as well as operators. They read durable state and return compact JSON unless the caller asks for more detail.
