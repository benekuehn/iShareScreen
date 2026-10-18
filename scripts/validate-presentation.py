#!/usr/bin/env python3
"""Visible Wayland presentation/cleanup smoke test; no remote desktop needed."""

import ctypes as C
import json
import time
from types import SimpleNamespace

import glfw

from isharescreen.frontend.native.app import create_window
from isharescreen.frontend.native.presentation import Presentation

window = create_window(320, 180)
glfw.set_window_title(window, "iShareScreen presentation check — closes automatically")
observer = None
try:
    observer = Presentation.create(window)
    if observer is None:
        raise RuntimeError("Wayland presentation feedback is unavailable")
    gl = C.CDLL("libGLESv2.so.2")
    gl.glClearColor.argtypes = [C.c_float] * 4
    gl.glClear.argtypes = [C.c_uint]
    started = time.monotonic()
    for frame_id in range(1, 21):
        while not observer.can_submit():
            glfw.poll_events()
            observer.pump()
            time.sleep(0.001)
        gl.glClearColor(frame_id / 20, 0.1, 0.2, 1)
        gl.glClear(0x4000)
        observer.request(frame_id, SimpleNamespace())
        glfw.swap_buffers(window)
        glfw.poll_events()
        observer.pump()
        time.sleep(0.02)
    for _ in range(20):
        glfw.poll_events()
        observer.pump()
        time.sleep(0.005)
    stats = observer.profile(time.monotonic() - started)
    assert stats["displayed"] > 0, stats
    assert stats["displayed"] + stats["discarded"] == 20, stats
    assert stats["feedback_dropped"] == 0, stats
    assert observer.can_submit()
    # Exercise closing an observer with a feedback request still pending.
    observer.request(21, SimpleNamespace())
    print(json.dumps(stats, indent=2))
finally:
    if observer:
        observer.close()
    glfw.destroy_window(window)
    glfw.terminate()
