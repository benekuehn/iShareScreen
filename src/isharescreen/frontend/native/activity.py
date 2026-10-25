"""Reference-viewer adapter for the shared Rust activity/power budget."""
import ctypes as C
import os
import time

from ...proxy.protocol.rust_core import library


class Snapshot(C.Structure):
    _fields_ = [(name, C.c_uint32) for name in ("focused", "visible", "mode", "background_fps")] + [
        ("outputs_selected", C.c_uint64), ("outputs_suppressed", C.c_uint64),
    ]


class ActivityBudget:
    @classmethod
    def create(cls):
        return None if os.environ.get("ISS_ADAPTIVE_FPS", "1") == "0" else cls(int(os.environ.get("ISS_BACKGROUND_FPS", "5")))

    def __init__(self, background_fps=5):
        self.background = False
        self.lib = library()
        self.handle = self.lib.iss_activity_new(background_fps)
        if not self.handle:
            raise ValueError("Background FPS must be between 1 and 60")

    def window(self, focused, visible):
        self.background = not (focused and visible)
        self.lib.iss_activity_window(self.handle, int(focused), int(visible), time.monotonic_ns())

    def input(self):
        self.lib.iss_activity_input(self.handle, time.monotonic_ns())

    def render_delay(self):
        return self.lib.iss_activity_tick(self.handle, 0, time.monotonic_ns()) / 1e9

    def event_wait(self):
        return self.lib.iss_activity_tick(self.handle, 1, time.monotonic_ns()) / 1e9

    def take_output(self):
        return bool(self.lib.iss_activity_tick(self.handle, 2, time.monotonic_ns()))

    def submitted(self):
        self.lib.iss_activity_tick(self.handle, 3, time.monotonic_ns())

    def snapshot(self):
        data = Snapshot()
        self.lib.iss_activity_snapshot(self.handle, time.monotonic_ns(), C.byref(data))
        return {
            "mode": ("active", "idle", "background", "hidden")[data.mode],
            "focused": bool(data.focused), "visible": bool(data.visible),
            "background_fps": data.background_fps,
            "outputs_selected": data.outputs_selected,
            "outputs_suppressed": data.outputs_suppressed,
            "host_fps_controlled": False,
        }

    def __del__(self):
        if getattr(self, "handle", None):
            self.lib.iss_activity_free(self.handle)
            self.handle = None
