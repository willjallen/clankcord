# Hey Clanky Latency Measurement

Measured from persisted `timeline_events` and `jobs` rows for the last observed
wake request on 2026-05-15 UTC. Production did not persist an exact audio-frame
timestamp, so `discord_voice_play_audio.started_at_ms` is the closest durable
proxy for when each chime became audible.

## Request

- Wake phrase: `Hey Clanky`
- Follow-up request: `Tell me a fact about birds`
- Wake phrase started: `2026-05-15 03:54:34.186 UTC`
- Wake phrase ended: `2026-05-15 03:54:36.592 UTC`
- Request speech started: `2026-05-15 03:54:38.509 UTC`
- Request speech ended: `2026-05-15 03:54:42.221 UTC`

## Wake Chime

- `wake_detected` ended: `03:54:35.186`
- `wake_detected` persisted: `03:54:35.716`
- wake playback parent created: `03:54:35.806`
- wake playback parent started: `03:54:35.883`
- wake mute child created: `03:54:35.951`
- wake mute child started: `03:54:36.045`
- wake mute child completed: `03:54:36.061`
- wake play-audio child created: `03:54:36.178`
- wake play-audio child started: `03:54:36.277`

Wake deltas:

- `wake_detected` end to play-audio start: `1.091s`
- `wake_detected` persist to play-audio start: `561ms`
- wake playback parent creation to play-audio start: `471ms`
- wake phrase start to play-audio start: `2.091s`
- wake phrase end to play-audio start: `-315ms`

The negative final value is expected: wake detection fired from the probe window
before the full STT speech segment for `Hey Clanky` had ended.

## Ack Chime

- request speech ended: `03:54:42.221`
- request speech persisted: `03:54:43.878`
- wake activation policy due time: `03:54:45.221`
- ack playback parent created: `03:54:46.019`
- ack playback parent started: `03:54:46.471`
- command child created: `03:54:46.075`
- command child started: `03:54:46.530`
- agent task child created: `03:54:46.814`
- agent task child started: `03:54:47.279`
- ack mute child created: `03:54:46.985`
- ack mute child started: `03:54:47.243`
- ack mute child completed: `03:54:47.607`
- ack play-audio child created: `03:54:48.840`
- ack play-audio child started: `03:54:49.178`

Ack deltas:

- request speech end to ack play-audio start: `6.957s`
- request speech persist to ack play-audio start: `5.300s`
- policy due time to ack play-audio start: `3.957s`
- policy due time to ack playback parent creation: `798ms`
- ack playback parent creation to parent start: `452ms`
- ack playback parent start to mute child creation: `514ms`
- mute child creation to mute start: `258ms`
- mute adapter duration: `364ms`
- mute complete to play-audio child creation: `1.233s`
- play-audio child creation to play-audio start: `338ms`

Canonical breakdown from the end of user speech to audible ack proxy:

- Speech/STT/capture persistence after speech end: `1.657s`
- Configured silence policy window after speech end: `3.000s`
- Remaining policy wait after transcript persisted: `1.343s`
- Post-policy job dispatch/playback overhead: `3.957s`

The ack was late mostly because, after the intended three-second silence policy
expired, the system still spent almost four seconds before the persisted
play-audio job start. The largest concrete non-policy gap was `1.233s` between
the mute child completing and the play-audio child being created. The Discord
mute operation itself measured `364ms`, so this ack delay was not dominated by
the Discord API.

The agent task started at `03:54:47.279`, which was `5.058s` after request
speech ended and `2.058s` after the policy due time. The ack play-audio child
started `1.899s` after the agent task started, so the audible ack lag was
specifically playback continuation latency rather than agent kickoff latency.
