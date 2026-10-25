# Experimental Linux GPU engine

This branch adds `--frontend native`: a Rust-owned FFmpeg/VAAPI decoder and
EGL/GLES compositor. Compressed packets cross the Python ABI; decoded pixels
remain GPU surfaces through DMA-BUF import, color conversion and composition.
An independent Rust socket receiver drains encrypted UDP packets during the
handshake and live session into a buffer bounded at 16,384 packets / 32 MiB.
It prevents GIL stalls from overflowing the kernel receive queue; overflow is
counted in the session's drop telemetry. Packet-capture mode retains the existing
socket path so its recorder continues to see every application receive.
Native AVC decoding also runs on a separate bounded worker, so waiting for
GPU completion cannot stop RTP decryption/assembly. Configuration epochs discard
queued work from before a resize/restart; codec shutdown waits for that worker.

The Rust protocol core now handles RSA1/SRP authentication, SRTP/SRTCP,
enc1103 control cryptography and HEVC depacketization for the native frontend.
The existing Python session still orchestrates negotiation, access-unit order,
loss recovery, input, audio and clipboard. This is not a complete Rust client;
see [NATIVE_PORT.md](../NATIVE_PORT.md) for the standalone migration gates.

Status: experimental. A completion-wait trial brought 4K submission throughput
close to decoder output (about 56–58 fps in short motion intervals), eliminating
most premature surface replacement. A longer motion run still developed HEVC
reference errors and recovery pauses. Sustained 4K60 and actual display
presentation remain acceptance requirements; passing unit tests does not establish either.

After feedback/receive-worker changes, a continuous-motion HEVC trial on the M1
host and Intel Linux viewer collected 35 consecutive five-second intervals:
assembled picture AUs averaged 57.07 fps; clean decoder output and swap
submissions both averaged 56.74 fps (submission range 54.4–58.0 fps). The native
receive buffer reported zero drops. Reference errors still occurred, and swap
submission is not proof of physical presentation. This is progress toward
sustained 4K60, not a passing 4K60 acceptance test. Both alternate H.264 and
two-quality-tier offers were compared; the default remains single-picture HEVC
with one quality tier.

Wayland presentation feedback exposed a gap between submission and display:
an unpaced continuous-motion sample (10 five-second intervals) averaged
54.33 submitted fps but 45.86 compositor-confirmed displayed fps, with 422
discarded updates. Experimental frame-callback pacing reduced discards in
steady intervals, but the full 26-interval sample averaged only 47.70 displayed
fps, included stalls, and felt worse to the user. These runs were not a
controlled A/B comparison: visibility and decoder recovery affected results.
Pacing is therefore opt-in and remains disabled by default. Presentation
feedback is measurement infrastructure, not evidence of a 4K60 improvement.

The default native offer requests one HEVC 4:4:4 picture per frame, avoiding the
cross-tile reference structure of Apple's four-tile stream. Single-picture
HEVC omits DONL; the client detects that layout from aggregation packet lengths
and carries it into fragment/single-NAL parsing. LTR acknowledgment is disabled
when there is no DONL to echo. A short, bounded NACK/reorder window applies to
this stream as well as AVC. Unchanged desktops naturally produce fewer updates.
NACK requests cover up to 1,024 recent missing packets (large intra pictures
can span hundreds). Missing packets remain tracked for at most 75 ms, with
up to three NACK attempts separated by 20 ms; received packets cancel retries.
Tracking is bounded to 1,024 packets per source and 32 SSRC generations, and
shared receive/transmit state is protected by a lock. Retry wakeups do not
accelerate heartbeat, receiver-report, cursor-poll or profile cadence.
The control snapshot's `rtp_repair` counters distinguish observed gaps, packets
that returned within the window, expiry, eviction and NACK attempts/errors.
`returned_after_nack` alone cannot distinguish retransmission from reordering.
Healthy access units still flush immediately; retries do not extend the repair
window or delay clean frames. Receiver reports carry measured interval/cumulative loss
and RTP-clock jitter, including late/duplicate reception, rather than zero
placeholders. Jitter includes client packet-processing scheduling delay.
The offer keeps RTCP feedback enabled independently of LTR acknowledgments;
disabling DONL/LTR must not disable packet-loss feedback for a single picture.
For native single-picture decoding, compressed IDR size alone does not prevent
recovery: simpler content can produce a much smaller valid IDR. Transport
completeness and decoder error signals remain active.

## Build on Linux

Requirements: Rust/Cargo supporting edition 2024, a C compiler, pkg-config,
FFmpeg development libraries, libva, EGL, GLES3, Wayland client libraries,
OpenSSL development libraries, `wayland-scanner` and the `wayland-protocols` XML files. The runtime also requires
a VAAPI driver supporting the selected codec, a usable render device and
`EGL_EXT_image_dma_buf_import` / `GL_OES_EGL_image_external_essl3`.

On Arch, the build packages are `rust base-devel pkgconf ffmpeg libva libglvnd wayland wayland-protocols openssl`.
Install the appropriate GPU driver separately; Intel GPUs commonly use
`intel-media-driver`. GPU support must be confirmed by a real decode, not just
an advertised profile.

```sh
cd native
cargo test --locked
cargo build --release --locked
cd ..
cargo test --locked --manifest-path protocol/Cargo.toml
cargo build --release --locked --manifest-path protocol/Cargo.toml
cargo build --release --locked --manifest-path client/Cargo.toml
python -m venv .venv
.venv/bin/pip install -e '.[test]'
./launch-performance.sh
```

Choose **native GPU**, **Auto** decoder and **3840 × 2160**, with display scale
**200%**, for a 1920×1080 logical desktop backed by 3840×2160 pixels. Enter the
Mac password in the local form. For CLI use, run `.venv/bin/iss --help`; pass
`--frontend native --codec hevc --advertise 1920x1080@2` alongside connection
arguments. Passwords should be entered interactively or using `--password-stdin`.

The ABI links system FFmpeg. PyAV wheels may use a separate bundled FFmpeg and
may lack VAAPI. For the older desktop frontend to use the same system hardware
decoder, build PyAV from source, for example:

```sh
.venv/bin/pip install --force-reinstall --no-binary av 'av==19.0.1'
```

This step is not required for native decoding, which calls system FFmpeg
directly. Both native codec probes decode synthetic packets and require actual
GPU output. Native hardware failures are explicit; use the existing desktop or
browser frontend for fallback. The original installation can be kept separately.

Optional environment variables:

| Variable | Purpose |
| --- | --- |
| `ISS_NATIVE_LIBRARY` | Override the native `.so` path. |
| `ISS_BACKGROUND_FPS=5` | Background presentation/output-sync limit, 1–60. Foreground resumes immediately without an added pacer. |
| `ISS_ADAPTIVE_FPS=0` | Disable the default Rust activity budget for comparison. |
| `ISS_VAAPI_DEVICE` | Render node; default `/dev/dri/renderD128`. |
| `ISS_TILES_PER_FRAME=4` | Compare Apple's tiled stream; experimental reference/recovery behavior. |
| `ISS_LOG_LIBAV=1` | Verbose libav diagnostics; can produce substantial output during corruption. |
| `ISS_CTRL_AS_CMD=0` | Disable Ctrl-to-Command mapping. |
| `ISS_NATIVE_VSYNC=1` | Enable extra application VSync; default off for latency. |
| `ISS_SINGLE_VIDEO_STREAM=0` | Restore the extra host quality tier; native defaults to one stream. |
| `ISS_NATIVE_SYNC_DECODE=0` | Compare asynchronous GPU output; the current trial waits for completion on the decoder worker before publishing. |
| `ISS_VIDEO_RTCP_MUX=0` | Compare legacy video feedback on port 5900; native sends video RTCP on video port 5901. |
| `ISS_NATIVE_PRESENT_PACING=1` | Opt into experimental frame-callback pacing. Default off after a user-reported responsiveness regression. |
| `ISS_NATIVE_PROTOCOL=0` | Compare the Python reference authentication/transport; the native viewer defaults to Rust. |
| `ISS_PROTOCOL_VERIFY=1` | Compare live Rust SRTP results against the Python reference. Extra development overhead; never logs payloads/keys. |

`SIGUSR1` dumps Python thread stacks for a stalled native viewer. The GUI logs
the child's exit status or signal. Remote desktop pixels, packet captures,
credentials, logs and build products must not be committed.

Adaptive mode keeps the transport connected and decodes all compressed reference
pictures. It samples background presentation/output synchronization at 5 Hz;
minimizing stops drawing, releases compositor surfaces and samples outputs at
1 Hz for progress. Foreground rendering retains the unpaced path. Main-thread
event waits are interruptible by input/focus events, with 200 ms idle/background
timeouts and 500 ms minimized timeouts. A static startup picture no longer
causes recovery-request storms when the Mac sends no subsequent video.

This is client-side resource control, not a 5 fps host encoder setting. Network
traffic and reference decoding can remain at the source rate while remote
content moves. Activity snapshots explicitly report `host_fps_controlled=false`.
Tests cover 600 simulated incoming frames selecting 50 background outputs,
immediate focus restoration, decoder-reference continuity and idle recovery
suppression. A live quiet-desktop check is separate from sustained motion/FPS
validation. Unused Wayland frame-pacing callbacks are no longer requested when
experimental pacing is off, preventing their accumulation behind occluded windows.

An unattended M1 3840×2160 trial measured 4.5 background render submissions/s
while decoding 52.8 frames/s, with an empty decode queue and no dropped feedback
registrations. The window was occluded, so displayed FPS was zero; this measures
the client work cap, not physical presentation. Startup HEVC reference errors
still occurred and remain a separate recovery/stability issue.

## Build on an Apple silicon Mac for an x86 Linux viewer

An ordinary macOS Rust build produces a macOS library. Use an **amd64 Linux
container** on the Mac to produce a Linux `.so` against matching FFmpeg majors.
Apple's `container` runtime can provide that environment. The development setup
uses a persistent Arch container named `isharescreen-build`, a repository bind
mount at `/work`, 2 CPUs and 3 GiB of RAM. Keep local GPU validation on Linux.

With the build container already provisioned, use:

```sh
scripts/build-on-mini.sh mini /Users/your-user/Work/iShareScreen-performance
```

The script syncs source, runs Rust tests and a release build remotely, then
downloads the shared library. It excludes Git metadata, local state, virtual
environments and build output. It never transfers passwords. Provisioning an
Arch build container requires `rust ffmpeg libva libglvnd pkgconf wayland wayland-protocols` plus the
compiler toolchain from `base-devel`. Python tests additionally need the project
dependencies; a test venv can be retained in the container.

## Offline 4K validation

Generate synthetic content on the build server (this is not a desktop recording):

```sh
ffmpeg -f lavfi -i testsrc2=size=3840x2160:rate=60 -frames:v 180 \
  -vf format=yuv444p -c:v libx265 -preset ultrafast \
  -x265-params pools=2:frame-threads=1:bframes=0:repeat-headers=1:keyint=120 \
  -pix_fmt yuv444p -y test-4k444.h265
```

Copy it to `native/target/` on the Linux viewer and run:

```sh
.venv/bin/python scripts/validate-native.py native/target/test-4k444.h265
```

The validator checks hardware surface type, decode error flags, completed GPU
work and sampled colors/orientation against software decoding. Readback is
deliberately confined to validation. Its throughput excludes network transport,
host encoding and visible display scanout, so it is **not** end-to-end fps or
input latency. It assumes low-delay HEVC with repeated startup parameters.

An Intel Ice Lake trial decoded/composited 180 synthetic 4K 4:4:4 frames without
errors at about 96 fps, with a 12.7 ms pipeline p95 and negligible sampled color
error. Treat these as one-machine measurements. Earlier live four-tile tests
negotiated 4K GPU decode but developed reference errors/freezes. Live stability,
continuous-motion 60 fps, resizing and long-session acceptance are separate
requirements and must be measured before calling this production ready.

## Ownership and limits

Rust owns opaque decoder/frame/renderer handles. FFmpeg and graphics ABI details
stay in `bridge.c`. A frame retains its hardware allocation; each imported
surface has a separate frame reference, EGL image, texture and completion fence.
The compositor bounds in-flight leases at 16 and waits before reusing capacity.
Only completed GPU work permits surface release. Generation checks reject old
decoder output; geometry checks reject tiles outside the canvas. VAAPI allocation
padding is cropped using exported dimensions.

GLFW window events and input remain on the main thread. The EGL context and all
composition/swap/cleanup calls belong to one render worker, so a driver stall
does not prevent window event dispatch. A bounded four-surface tail selects the
newest GPU-completed picture rather than blocking on unfinished work. This
skips stale presentation while compressed references continue decoding.

## Frame budgets and measurements

60 fps permits 16.67 ms between distinct displayed frames; 120 fps permits
8.33 ms. Pipeline stages overlap, so total frame latency is a separate budget.
The native profile reports **submitted_fps**, not confirmed display FPS, and
**decoder_output_fps**, not GPU completion rate. On Wayland, a separate
`native presentation` profile reports compositor-confirmed displayed FPS,
discarded updates, presentation gaps, and client timestamps to presentation.
The feedback flags and clock ID are recorded: cross-clock latency subtraction
is never performed. Unavailable feedback is reported explicitly; other platforms
continue measuring submissions only. Duplicating an unchanged frame
must not inflate these measurements.
`clean_frame_fps` comes from each backend's clean-frame counter: HEVC counts
clean decoder output; AVC counts clean frames evaluated by its consumer. It
does not prove physical display presentation.
`assembled_fps` counts complete picture access units handed to the decoder,
allowing comparison of incoming stream throughput with decoder output and swap
submission. It excludes parameter-only groups and includes pictures that later
fail decoding.

The profile also counts published/taken/superseded surfaces and polls with no
ready surface. `output_age_p95_ms` measures decoder output to renderer selection;
`ready_observed_p95_ms` measures output to the first successful completion query
or synchronization; `decode_to_select_p95_ms` starts at decode submission.
Observation timestamps include scheduling delay; they are not exact hardware
completion timestamps. An unsupported readiness query uses synchronized upload
without inventing a completion timestamp. None of these measure host capture,
network transit or monitor scanout.

Wayland callbacks use a private event queue, pumped after GLFW events on main.
Rendering requests feedback only for new video content before swapping. A
separate optional frame-ready callback paces drawing while allowing multiple commits
to remain in the presentation pipeline; waiting for actual presentation would
serialize that pipeline. After 250 ms a bounded retry can proceed if the frame
callback stalls. Neither the event
thread nor the packet/decoder workers wait for presentation. Pending feedback
objects (256) and completed event records (2,048) are bounded, with overflow
reported. The render worker stops before proxies and the private queue are
freed, and all observer objects are freed before GLFW destroys its display.
Pacing is disabled by default: despite reducing compositor discards in steady
intervals, the first live trial included stalls and felt worse to the user.
Display timing remains enabled independently of pacing.

Run `.venv/bin/python scripts/validate-presentation.py` for a short visible
feedback/cleanup check without a remote desktop. It confirms all 20 synthetic
commits receive presentation/discard outcomes and also exercises destruction
with pending feedback; it is not a 4K60 performance acceptance test.

The temporary macOS motion benchmark provides continuous changes and exits
automatically. Run `benchmarks/macos/script/build_and_run.sh --verify 180`
on the host after the viewer connects for a three-minute test. Duration defaults
to 60 seconds and is bounded to 1–600 seconds; desktop idle time is not an FPS test.

The initial scope is Linux, VAAPI, one SDR display. HDR, other acceleration APIs,
multi-display window management and a standalone Rust protocol/UI port remain
follow-up work. A full port should preserve the Python implementation as a
behavioral reference and use sanitized protocol fixtures. Profile packet handling
before deciding whether replacing it improves latency on the target machine.
