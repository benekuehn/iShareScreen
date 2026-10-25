# Standalone native client

The Linux viewer now runs as one Rust/C executable. Python remains available
as a reference client and fixture generator; it is not part of the standalone
session at runtime.

## Run

```sh
./launch-native.sh
./launch-native.sh --background-fps 5 --reconnect
./launch-native.sh --auto-quit-secs 20
./launch-native.sh --auth-check
./launch-native.sh --negotiate-check
```

The launcher uses this checkout's saved endpoint and login-keyring credential.
`--host`, `--user`, `--port`, `--auth srp|nonsrp` and `--password-stdin`
override the connection. Passwords never appear in arguments or settings.
`--no-audio` disables host audio/playback; `ISS_CLIP=0` disables pasteboard sync.
The check options authenticate/negotiate and exit without opening a viewer.
`--reconnect` opts into fresh sessions after errors with backoff capped at eight
seconds; closing a healthy window exits. Otherwise errors exit with a message.

## Implemented session

- Native RSA1/SRP authentication and verified server proofs, RFB/display/media
  negotiation, encrypted control and SRTP/SRTCP cryptography.
- UDP reception before negotiation, with peer filtering. Early closed host
  ports cannot permanently stop the receiver. Datagrams, pending picture groups
  and compressed decode jobs each have a 32 MiB byte limit plus count limits.
- Sequence-ordered HEVC pictures, rollover, duplicates, 75 ms repair, at most
  three spaced NACK attempts, compact feedback, measured loss/jitter and SR
  timing. Incomplete pictures never enter the reference chain; overload starts
  a new recovery epoch instead of continuing after dropped compressed frames.
- Separate event, network, decode, render and audio owners. Input wakes socket
  waits immediately; idle networking has no millisecond polling loop. Worker
  errors reach the CLI. Shutdown deadlines prevent a wedged driver leaving an
  unresponsive window behind.
- VAAPI decode, DMA-BUF/EGL composition, bounded latest-frame presentation,
  Wayland presentation feedback and process CPU telemetry. No full-frame CPU
  pixel copies or extra foreground frame pacer.
- Pointer, wheel, keyboard, layout-aware text and focus-loss key release;
  bounded cursor caching and bounded, multi-record UTF-8 pasteboard sync.
- Native FDK AAC-ELD-SBR decoding and PortAudio playback on a separate worker.
  Playback opens after decoded audio arrives; the audio queue is bounded.
- New source/parameter sets require an intra picture. Decoded geometry recreates
  the GPU canvas and updates pointer mapping. The remote canvas stays fixed
  (4K by default); resizing the local window scales that canvas.

Background presentation/output synchronization is capped at 5 Hz, configurable
with `--background-fps` or `ISS_BACKGROUND_FPS`. Focus restores the full-speed
path immediately. Minimizing stops drawing, releases composition surfaces and
samples decoder output at 1 Hz. The connection stays open. Compressed references
still need decoding: this does not change the Mac's capture rate. Moving remote
content can still consume network/decode resources; static content naturally
produces fewer host updates.

## Build and verification

```sh
scripts/build-on-mini.sh mini /Users/bene/Work/iShareScreen-performance
ISS_TEST_RUST_PROTOCOL=1 .venv/bin/python -m pytest -o addopts='' -q

# Optional offline codec test: no playback device or remote host.
cc client/tests/audio_roundtrip.c client/audio.c -lfdk-aac -ldl -lm -o /tmp/iss-audio-check
/tmp/iss-audio-check
```

The mini builds x86 Linux artifacts in `isharescreen-build`. The script copies
and starts that existing build container when needed. Once the build finishes,
it is safe to stop the container to free the mini's memory for screen capture.
The script copies
its native GLFW runtime beside the executable; the launcher sets the search
path. Normal installations can provide GLFW via their package manager instead.
Runtime dependencies include system FFmpeg/VAAPI/EGL, a supported VAAPI driver,
OpenSSL, libfdk-aac, PortAudio and `secret-tool`. Native session code does not
require a Python installation.

Synthetic fixtures independently check RFB/media messages, encrypted pointers,
SRTP rollover/repair, CBC recovery and authentication proofs. Transport tests
cover reordering, missing-picture rejection, markerless startup parameter APs,
byte-budget release and bounded compact NACKs. Cursor/pasteboard tests cover
conversion, cache hits, fragmented records and oversized inputs. The offline
audio test checks a synthetic 440 Hz signal's decoded amplitude and peak; it
also passes AddressSanitizer/UBSan locally.

Live M1 standalone trials negotiated 3840×2160, decoded approximately 53–56 fps
in steady intervals, submitted about 53 frames/s in an initial foreground trial,
and capped background submissions around 4.7/s while decoding 55.6/s. One trial
submitted 776,160 PCM frames to the audio device. These short interoperability
samples are not a sustained performance acceptance result. Displayed FPS is
logged separately from submissions; occluded windows can display zero.

The final background smoke test confirmed 4.5 physically displayed frames/s in
two five-second intervals, with no discarded presentations, while decoding
54–56 frames/s. Process CPU use was 11.6% of one core during those intervals,
including the active session/audio work. Source-rate throttling is still needed
to avoid that reference-decoding cost when moving content is in the background.

Remaining acceptance work includes sustained physically displayed 4K60,
adverse-network recovery, repeated host display/session switches, reconnect
soaks and subjective input/clipboard/audio checks. Host capture-rate control and
dynamic remote-resolution requests remain unimplemented. The reference viewer
is still available for comparison:
`./launch-performance.sh connect --last --preset native-4k`.
