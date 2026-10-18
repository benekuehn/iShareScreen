"""GPU context ownership stays on one worker; GLFW events stay on main."""

from __future__ import annotations

import logging
import os
import threading
import time
from collections import defaultdict
from ...proxy.media.surface_queue import ReadySurfaces

import glfw

log = logging.getLogger(__name__)


def percentile(values, p=0.95):
    if not values:
        return 0.0
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * p))]


class RenderLoop:
    def __init__(self, window, session, renderer_factory):
        self.window = window
        self.session = session
        self.renderer_factory = renderer_factory
        self.stop_event = threading.Event()
        self.viewport = (*glfw.get_framebuffer_size(window), True, 0)
        self.error = None
        self.heartbeat = time.monotonic()
        self.thread = threading.Thread(
            target=self._run, name="native-render", daemon=True
        )

    def update_view(self, width, height, visible=True, *, redraw=False):
        old = self.viewport
        changed = (width, height, visible) != old[:3]
        self.viewport = (width, height, visible, old[3] + int(redraw or changed))

    def start(self):
        glfw.make_context_current(None)
        self.thread.start()

    def stop(self):
        self.stop_event.set()
        self.thread.join(timeout=3)
        return not self.thread.is_alive()

    def _run(self):
        renderer = None
        try:
            glfw.make_context_current(self.window)
            glfw.swap_interval(int(os.environ.get("ISS_NATIVE_VSYNC", "0")))
            canvas = (0, 0)
            slot_height = 0
            pending = defaultdict(ReadySurfaces)
            last_redraw = -1
            profile_started = time.monotonic()
            last_decoded = sum(self.session._decoder.good_counts)
            last_clean = sum(getattr(self.session._decoder, "clean_counts", []))
            last_assembled = getattr(self.session, "video_access_units", 0)
            presents = uploads = 0
            last_surface_stats = {}
            compose_times, swap_times = [], []
            output_ages, ready_delays, submit_ages = [], [], []
            while not self.stop_event.is_set() and self.session.is_connected:
                now = time.monotonic()
                self.heartbeat = now
                width, height, visible, revision = self.viewport
                if not visible or not width or not height:
                    self.stop_event.wait(0.02)
                    continue
                dimensions = self.session.canvas_dims
                if dimensions != canvas:
                    if renderer:
                        renderer.close()
                        renderer = None
                    canvas = dimensions
                    slot_height = 0
                    pending.clear()
                dirty = False
                for ti in range(self.session.num_tiles):
                    frame = self.session.get_frame(ti)
                    frames = pending[ti]
                    if frame is not None:
                        frames.add(frame)
                    frame = frames.take()
                    if frame is None:
                        continue
                    if not getattr(frame, "gpu_surface", False):
                        raise RuntimeError(
                            "Native rendering requires a native GPU decoder"
                        )
                    if renderer and frame.generation < renderer.generation:
                        continue
                    if renderer is None or renderer.generation != frame.generation:
                        if renderer:
                            renderer.close()
                        renderer = self.renderer_factory(*canvas, frame.generation)
                        slot_height = frame.height
                    started = time.monotonic()
                    observed_ns = time.monotonic_ns()
                    output_ns = getattr(frame, "decoder_output_ns", None)
                    ready_ns = getattr(frame, "gpu_ready_observed_ns", None)
                    submit_ns = getattr(frame, "decode_submit_ns", None)
                    if output_ns is not None:
                        output_ages.append((observed_ns - output_ns) / 1e6)
                        if ready_ns is not None:
                            ready_delays.append((ready_ns - output_ns) / 1e6)
                    if submit_ns is not None:
                        submit_ages.append((observed_ns - submit_ns) / 1e6)
                    if renderer.upload(frame, ti, slot_height):
                        uploads += 1
                        dirty = True
                    compose_times.append((time.monotonic() - started) * 1000)
                if renderer and (dirty or revision != last_redraw):
                    renderer.draw(width, height)
                    started = time.monotonic()
                    glfw.swap_buffers(self.window)
                    swap_times.append((time.monotonic() - started) * 1000)
                    presents += 1
                    last_redraw = revision
                else:
                    self.session.wait_for_fresh_tile(0.004)
                elapsed = time.monotonic() - profile_started
                if elapsed >= 5:
                    decoded = sum(self.session._decoder.good_counts)
                    clean = sum(getattr(self.session._decoder, "clean_counts", []))
                    assembled = getattr(self.session, "video_access_units", 0)
                    surface_stats = getattr(
                        self.session._decoder, "presentation_stats", dict
                    )()
                    surface_delta = {
                        key: value - last_surface_stats.get(key, 0)
                        for key, value in surface_stats.items()
                    }
                    log.info(
                        "native profile: canvas=%s submitted_fps=%.1f decoder_output_fps=%.1f clean_frame_fps=%.1f assembled_fps=%.1f "
                        "tile_updates=%d compose_p95_ms=%.2f swap_p95_ms=%.2f "
                        "decode_queue=%d generation=%s surfaces=%s "
                        "output_age_p95_ms=%.2f ready_observed_p95_ms=%.2f "
                        "decode_to_select_p95_ms=%.2f",
                        canvas,
                        presents / elapsed,
                        max(0, decoded - last_decoded) / elapsed,
                        max(0, clean - last_clean) / elapsed,
                        max(0, assembled - last_assembled) / elapsed,
                        uploads,
                        percentile(compose_times),
                        percentile(swap_times),
                        self.session._decoder.decode_queue_depth,
                        renderer.generation if renderer else None,
                        surface_delta,
                        percentile(output_ages),
                        percentile(ready_delays),
                        percentile(submit_ages),
                    )
                    last_surface_stats = surface_stats
                    presents = uploads = 0
                    compose_times.clear()
                    swap_times.clear()
                    output_ages.clear()
                    ready_delays.clear()
                    submit_ages.clear()
                    profile_started = time.monotonic()
                    last_decoded = decoded
                    last_clean = clean
                    last_assembled = assembled
        except BaseException as exc:
            self.error = exc
            log.exception("native rendering failed")
        finally:
            if renderer:
                renderer.close()
            glfw.make_context_current(None)
