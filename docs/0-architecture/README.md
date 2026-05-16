# Architecture

The architecture chapters describe how the Rust runtime moves work through Clankcord. The sequence is deliberately narrative: a request enters as a job, the runtime persists and schedules it, domain handlers create follow-up work, adapters perform external effects, and the timeline becomes the durable record that views and agents read later.

```text
request
  -> job
  -> runtime service
  -> timeline store
  -> domain handler
  -> adapter effect or child job
  -> output, event, artifact, or rendered view
```

[Jobs](0-0-jobs.md) establishes the execution model. [Runtime Service](0-1-runtime-service.md) explains the long-lived process and its loops. [Timeline Store](0-2-timeline-store.md) covers the durable store, artifacts, and views. [Adapters](0-3-adapters.md) defines the outside-world boundary.

The remaining chapters follow the main product flows. [Voice And Wake](0-4-voice-and-wake.md) covers voice bot placement, capture, wake probes, activation windows, and cues. [Agents And Sessions](0-5-agents-and-sessions.md) covers persisted agent routing, managed threads, DMs, and Codex invocation. [Automations](0-6-automations.md) covers stored rules and built-in placement. [Command Surfaces](0-7-command-surfaces.md) covers CLI, HTTP, Discord ingress, confirmations, and responses. [Transcripts And Publications](0-8-transcripts-and-publications.md), [Agent Runtime Contract](0-9-agent-runtime-contract.md), and [Privacy And Retention](0-10-privacy-and-retention.md) cover materialized memory, agent process behavior, and privacy-sensitive controls.
