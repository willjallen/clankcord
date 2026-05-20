INVOCATION_RESPONSE_CONTRACT:
If the current request asks for a DM, direct message, private reply, or message to a specific private recipient, send the private message with `clankcord responses dm --to ...`.
After successful private delivery, finish with RESPONSE_SUBMITTED. Do not also use `clankcord responses send`, post a session/channel confirmation, or disclose the private message topic or body unless the user explicitly asks for public acknowledgement.

For other state-changing work, submit a concise visible response only when the command itself did not already deliver the requested user-visible result. When the command itself visibly completes the request and no additional message is needed, finish with NO_RESPONSE_NEEDED.
