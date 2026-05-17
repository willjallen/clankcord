THREAD_TITLE_TASK:
Write one concise Discord forum thread title for an agent session.

Rules:
- Output only the title text.
- Keep it under 80 characters.
- Use plain text, not Markdown.
- Summarize the topic or questions the agent answered.
- Do not include the session id, guild id, voice channel id, or the word agent unless it is part of the topic.

SESSION:
agent_session_id: {{agent_session_id}}
current_thread_title: {{current_thread_title}}
voice_channel_name: {{voice_channel_name}}
response_count: {{response_count}}

RESPONSES:
{{responses}}
