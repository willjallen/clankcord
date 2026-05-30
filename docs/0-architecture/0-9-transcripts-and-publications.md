# Transcripts And Publications

Transcript work turns speech timeline events into materialized windows and Discord publication state. The speech timeline remains the base record. Windows and publications are durable views over that record, with artifacts written under the timeline store root.

```text
speech_segment events
      |
      v
transcript window
      |
      +--> transcript artifact
      +--> optional Discord publication
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

## Materialization

`materialize_transcript` creates a window and renders the selected speech into a transcript artifact. The transcript and metadata are written under the publication directory.

```text
durable/publications/<publication_id>/transcript.draft.txt
durable/publications/<publication_id>/metadata.json
```

The publication payload records the publication id, window id, guild, voice channel, publish mode, creator, draft path, Discord thread id, Discord message ids, and state. Creating the publication appends `publication_created`.

When Discord publication is requested, runtime creates `transcript_publication`. That job creates a managed forum thread, chunks the transcript artifact into Discord messages, stores the thread and message ids, and moves the publication state to `draft_published` or `live_draft_published`.

```text
materialize_transcript
      |
      +--> create window
      +--> render transcript text
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

Rendering reads selected speech events and formats them by timestamp and speaker label. JSON rendering includes the window and selected events. The command surface controls output size: compact JSON is the default view, `--verbose` expands selected records, and `--file ... --format json` writes large windows to disk for agent inspection.

## Search And Context

Transcript search, conversation lists, context resolution, and participant traces are read views over timeline events, windows, and room or member state. `transcripts search` can search one room or all channels in a guild. `conversations list` groups stored conversation views. `context resolve` creates or returns a focused window for references such as "the last thing" or "an hour ago". `participants trace` walks voice and speech events for a user across channels and can include speech snippets when requested.

These surfaces are designed for agents as well as operators. They read durable state and return compact JSON unless the caller asks for more detail.
