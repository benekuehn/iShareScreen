# Standalone native client

The destination is a Rust executable that connects from the CLI, handles the
complete session and presents GPU surfaces without starting a Python process.
The existing C FFmpeg/VAAPI/EGL engine remains useful native code. Python stays
as a behavioral reference and fixture generator during migration.

## Implementation gates

- [x] Portable Rust SRTP/SRTCP key derivation, authentication and encryption.
- [x] Per-SSRC rollover and late retransmission handling.
- [x] Rust enc1103 record authentication, counter recovery and CBC chaining.
- [x] Bounded, validated Apple HEVC AP/FU/single-NAL depacketization.
- [x] Verify reference-client adapters on a live Mac session.
- [x] Native RSA1/SRP authentication and server-proof verification.
- [x] Standalone CLI authentication with saved endpoint/keyring lookup.
- [x] Standalone RFB/display/media offer negotiation with a live 3840×2160 answer.
- [x] Shared Rust activity policy with background presentation and immediate focus restore.
- [ ] Move UDP reception, AU ordering, bounded NACK/reorder recovery and RTCP
      receiver statistics together into the Rust session worker.
- [ ] Port mid-session display changes and stream-generation adoption into Rust.
- [ ] Extend the standalone CLI from authentication to the complete streaming session.
- [ ] Bind existing GPU decoder/compositor into the standalone process with
      separate network, decode, render and window-event ownership.
- [ ] Port pointer/keyboard, cursor shapes, audio and clipboard behavior.
- [ ] Verify reconnect, resize, loss recovery and teardown under sustained use.
- [ ] Verify that the complete running client has no Python process/dependency.

These are migration gates, not completed-feature claims. The transitional
reference client can call `protocol/` through a narrow C ABI while its remaining
session logic is ported. `ISS_NATIVE_PROTOCOL=0` retains the Python reference
transport for comparisons; the native viewer selects the Rust transport by default.

`./launch-native.sh --auth-check` runs the compiled Rust executable, loads the
saved endpoint and keyring credential, authenticates with the Mac, verifies its
SRP server proof and exits. It opens no viewer. It is a native authentication
checkpoint, not yet the standalone streaming application. Authentication in
the reference native viewer also calls this Rust implementation through the
C ABI; packet-recording mode retains the reference handshake for its recorder.
Native authentication respects the selected mode and fails closed rather than
automatically falling back after a server-proof failure.

`./launch-native.sh --negotiate-check` additionally performs ClientInit,
ViewerInfo/encoding setup, encrypted control setup, virtual-display configuration
and HEVC offer/answer negotiation before disconnecting. A live M1 trial returned
a 3840×2160 canvas. It still opens no viewer and does not implement the media
receive loop. Synthetic wire fixtures compare the serialized messages with the
reference implementation byte for byte.

The Rust activity budget is already used by the reference native viewer. It
limits background output synchronization and presentation to 5 Hz, keeps
reference decoding intact, releases minimized composition resources, and restores
the foreground path without an added frame pacer. This does not lower the
host's capture rate; source-rate negotiation remains unimplemented.

## Verification

`protocol/tests/vectors.json` contains only generated synthetic keys and packet
content. Its generator uses the existing Python implementation once during
development; the Rust crate and standalone tests need no Python at runtime.
Reference fixtures cover independent SSRCs, sequence wrap, retransmission,
RTP extension headers, encrypted/unencrypted SRTCP, control counter gaps and
CBC state after an authenticated-record rejection.

Live validation on the M1 host reached a 3840×2160 VAAPI/EGL session using
native authentication and transport. An opt-in comparison run checked over
24,000 live video SRTP packets against the reference without mismatches. This
is interoperability evidence, not a display-FPS acceptance test. Audio playback,
input and clipboard have not been ported to the standalone executable yet.

```sh
cargo test --locked --manifest-path protocol/Cargo.toml
scripts/build-on-mini.sh mini /Users/bene/Work/iShareScreen-performance
ISS_TEST_RUST_PROTOCOL=1 .venv/bin/python -m pytest tests/test_rust_protocol.py
```

The Mac mini builds the portable crate directly and the Linux libraries in the
existing `isharescreen-build` container. Native unit/interop checks do not prove
end-to-end display FPS. The separate sustained-4K60 objective remains unproven.
