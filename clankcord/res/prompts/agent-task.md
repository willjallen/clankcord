JOB:
job_id: {{job_id}}
agent_session_id: {{agent_session_id}}
guild_id: {{guild_id}}
voice_channel_id: {{voice_channel_id}}
requested_by_user_id: {{requested_by_user_id}}
requested_by: {{requested_by}}
request: {{request}}

WORKDIR:
CLANKCORD_AGENT_WORKDIR={{workdir}}

===== PREVIOUS CONTEXT =====
{{previous_context}}

===== QUESTION / ACTIVATION =====
{{question}}

CONTEXT NOTE:
The transcript above is only a compact 5-minute local window. It may omit prior discussion, missing speaker turns, ambiguous references, and broader room history.
If the request appears to depend on anything outside this local window, use Clankcord CLI commands to search or render more user messages before answering.
Prefer explicit file output for large transcript, timeline, search, or job results: `--file result.json --format json`, then inspect the file from your workdir with jq, rg, and sed.
