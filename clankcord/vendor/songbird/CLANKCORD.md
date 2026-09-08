# Songbird receive patch

This directory contains Songbird 0.6.0 from crates.io, corresponding to upstream
commit `3f77b5fa6de3f7354fdd7ef0dfa861518d304cdd`. The published crate checksum is
`5b9bf4ecb5083685b79a8fc60222f12e4f036e2817c5c341cd2b9ff2b429d237`.
The upstream ISC license and `resources/loop.wav` test fixture come from the
same release. Cargo resolves Songbird through the root `[patch.crates-io]` entry,
and the Docker build copies this directory before fetching dependencies.

The receive patch changes four production files:

- `src/driver/tasks/ws.rs`: Speaking registers SSRC identity; ClientDisconnect
  continues to emit its event and update the DAVE roster, while receive identity
  remains associated with the transport.
- `src/driver/tasks/message/udp_rx.rs`: the tracker stores SSRC-to-user bindings.
- `src/driver/tasks/udp_rx/mod.rs`: the five-second sweep prunes idle decoders.
- `src/driver/tasks/udp_rx/ssrc_state.rs`: real packets refresh decoder expiry.

Discord's ClientDisconnect payload identifies a user, without an SSRC or device
session. A departing device can emit this event while another device belonging
to the same user is sending audio. Receive bindings therefore live until the bot's
voice transport is destroyed or Speaking assigns that SSRC to another user.
Multiple SSRCs can identify one user. The identity map grows with distinct SSRCs
observed during that transport; decoder memory is reclaimed after the configured
packet-idle timeout (60 seconds by default). DAVE decryption and MLS membership
validation retain their upstream behavior.

Regression tests live in `tests/` and are included by the corresponding source
modules to exercise private receive internals. They drive the websocket Speaking
and ClientDisconnect handlers, advance the actual UDP cleanup loop across several
sweeps, and check both disconnect orders and same-SSRC reconnects. The receiver
lifetime test verifies transport-decrypted RTP refreshes decoder expiry, idle
decoders are reclaimed, and resumed streams retain their identity.

Run from the Clankcord crate root:

```sh
cargo test --locked -p songbird --lib --features receive voice_handoff
cargo test --locked -p clankcord --lib handoff_tests
cargo test --locked -p clankcord --test voice_capture
```

Upstream release and current source checks on 2026-09-07 show 0.6.0 as the latest
release and the user-keyed cleanup still present. When adopting an upstream
release with handoff-safe receive cleanup, run these regression scenarios against
it before removing this patch and workspace member. Sources:
[release](https://github.com/serenity-rs/songbird/releases/tag/v0.6.0),
[websocket handler](https://github.com/serenity-rs/songbird/blob/3f77b5fa6de3f7354fdd7ef0dfa861518d304cdd/src/driver/tasks/ws.rs),
[UDP receiver](https://github.com/serenity-rs/songbird/blob/3f77b5fa6de3f7354fdd7ef0dfa861518d304cdd/src/driver/tasks/udp_rx/mod.rs).
