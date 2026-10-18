"""Compositor-confirmed presentation, separate from decoder/swap throughput."""

import ctypes as C
import logging
import os
import time

import glfw

from ...proxy.media.native import library

log = logging.getLogger(__name__)


class PresentationEvent(C.Structure):
    _fields_ = [
        (name, C.c_uint64)
        for name in (
            "frame_id",
            "submit_ns",
            "decode_submit_ns",
            "gpu_ready_ns",
            "presented_ns",
            "refresh_ns",
            "sequence",
            "flags",
            "discarded",
        )
    ]


def p95(values):
    values = sorted(values)
    return (
        round(values[min(len(values) - 1, int(len(values) * 0.95))], 3)
        if values
        else None
    )


class Presentation:
    @classmethod
    def create(cls, window):
        if (
            not hasattr(glfw, "get_platform")
            or glfw.get_platform() != glfw.PLATFORM_WAYLAND
        ):
            log.info(
                "Presentation feedback unavailable on this GLFW platform; measuring submissions only"
            )
            return None
        lib = library()
        handle = lib.iss_presentation_new(
            glfw.get_wayland_display(), glfw.get_wayland_window(window)
        )
        if not handle:
            log.info(
                "Compositor presentation feedback unavailable; measuring submissions only"
            )
            return None
        return cls(lib, handle)

    def __init__(self, lib, handle):
        self.lib, self.handle = lib, handle
        self.clock_id = lib.iss_presentation_clock(handle)
        self._events = (PresentationEvent * 2048)()
        self._last_presented_ns = None
        self._last_refresh_ns = None
        self._last_request_at = 0
        self._pacing = os.environ.get("ISS_NATIVE_PRESENT_PACING", "0") == "1"
        log.info(
            "Wayland presentation feedback active (clock_id=%d, pacing=%s)",
            self.clock_id,
            "on" if self._pacing else "off",
        )

    def request(self, frame_id, frame):
        self._last_request_at = time.monotonic()
        self.lib.iss_presentation_request(
            self.handle,
            frame_id,
            getattr(frame, "decode_submit_ns", 0) or 0,
            getattr(frame, "gpu_ready_observed_ns", 0) or 0,
        )

    def can_submit(self):
        # A frame-ready callback paces drawing independently of (later) actual
        # presentation feedback, preserving pipeline overlap. Keep decode/network and
        # main-thread input running while the render worker waits. A bounded
        # retry avoids indefinitely waiting for feedback from an occluded or
        # otherwise stalled compositor surface.
        return (
            not self._pacing
            or not self.lib.iss_presentation_frame_pending(self.handle)
            or time.monotonic() - self._last_request_at >= 0.25
        )

    def pump(self):
        if self.lib.iss_presentation_pump(self.handle) < 0:
            raise RuntimeError("Wayland presentation event dispatch failed")

    def read(self):
        count = self.lib.iss_presentation_read(
            self.handle, self._events, len(self._events)
        )
        return [
            {name: getattr(event, name) for name, _ in PresentationEvent._fields_}
            for event in self._events[:count]
        ]

    def profile(self, elapsed):
        events = self.read()
        displayed = sorted(
            (event for event in events if not event["discarded"]),
            key=lambda event: event["presented_ns"],
        )
        gaps, submit, decode, ready = [], [], [], []
        late = 0
        for event in displayed:
            timestamp = event["presented_ns"]
            if self._last_presented_ns is not None:
                if timestamp > self._last_presented_ns:
                    gaps.append((timestamp - self._last_presented_ns) / 1e6)
                else:
                    late += 1
            self._last_presented_ns = max(timestamp, self._last_presented_ns or 0)
            self._last_refresh_ns = event["refresh_ns"]
            # Python/C submission and completion observations use MONOTONIC.
            # Other compositor clock domains still support FPS/gap reporting,
            # but cannot be subtracted from our client timestamps.
            if self.clock_id == time.CLOCK_MONOTONIC:
                for key, target in (
                    ("submit_ns", submit),
                    ("decode_submit_ns", decode),
                    ("gpu_ready_ns", ready),
                ):
                    if event[key] and timestamp >= event[key]:
                        target.append((timestamp - event[key]) / 1e6)
        return {
            "displayed_fps": round(len(displayed) / elapsed, 2),
            "displayed": len(displayed),
            "discarded": sum(bool(event["discarded"]) for event in events),
            "gap_p95_ms": p95(gaps),
            "gap_max_ms": round(max(gaps), 3) if gaps else None,
            "submit_to_display_p95_ms": p95(submit),
            "decode_to_display_p95_ms": p95(decode),
            "ready_to_display_p95_ms": p95(ready),
            "refresh_ms": self._last_refresh_ns / 1e6
            if self._last_refresh_ns
            else None,
            "flags": displayed[-1]["flags"] if displayed else None,
            "clock_id": self.clock_id,
            "late_feedback": late,
            "feedback_dropped": self.lib.iss_presentation_dropped(self.handle),
        }

    def close(self):
        if self.handle:
            self.lib.iss_presentation_free(self.handle)
            self.handle = None
