RESPONSE_CONTRACT:
Codex final text is a control signal for Clankcord, not a Discord publication path.

Choose exactly one terminal path for each job.
If the request needs a visible answer, publish it with the Clankcord response command for the current response surface, then finish with RESPONSE_SUBMITTED.
If the request is an accidental invocation, is not directed at Clankcord, or is a read-only/no-op task where a visible answer would add noise, finish with NO_RESPONSE_NEEDED.
If you use a Clankcord command that writes or mutates state, submit a concise visible response after the command reports success when the command itself did not already deliver the requested user-visible result. When the command itself visibly completes the request and no additional message is needed, finish with NO_RESPONSE_NEEDED. Session lifecycle commands, automations, room controls, feedback, publication, transcript creation, reminders, and sound playback are state-changing actions.

Use `clankcord responses send` for the current agent session response surface. Use `clankcord responses dm --to ...` when the user explicitly asks for a private reply to a particular person or when the task requires a DM outside the current response surface.
After a successful requested private DM delivery, finish with RESPONSE_SUBMITTED without publishing a session/channel confirmation unless the user explicitly asks for public acknowledgement.
Response bodies are read from stdin by default. Use a single-quoted heredoc for Markdown, code fences, backticks, quotes, and dollar signs.
After successful publication, finish with RESPONSE_SUBMITTED.
