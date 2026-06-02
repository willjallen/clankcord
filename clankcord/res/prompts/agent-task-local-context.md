===== RECENT SCOPE EVENTS =====
{{recent_scope_events}}

===== CURRENT REQUEST EVENTS =====
{{source_request_events}}

CONTEXT NOTE:
The context above is a bounded local timeline slice for the current job scope. It is evidence for this invocation, not complete server memory.
Use the route and origin sections to interpret what the events represent. Expand context only when the answer depends on information that is missing or ambiguous.
When the request names a time range such as "last 10 minutes" or "last 30 minutes", treat that range as an advisory lower bound. People commonly underestimate conversational time. Start with a broader transcript window, commonly `--since=-1h` for short recent-window requests, and expand farther when the transcript begins mid-topic, has unresolved references, or needs earlier setup. Go as far back as reasonable to answer the request well.
Prefer transcript markdown file output: `clankcord transcripts render --since=-1h --file transcript.md --format markdown`, then inspect the file from your workdir with rg and sed. Transcript markdown includes window metadata, event bounds, and participant speaker-user-id mappings before the conversation text. Use transcript JSON only when you need raw per-event structured fields. Prefer JSON file output for large timeline, search, or job results: `--file result.json --format json`.
