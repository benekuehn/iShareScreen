"""GLFW input/window loop with native Rust/FFmpeg/VAAPI/EGL media engine.

The Python loop passes opaque GPU handles to composition; it never maps or
uploads full-frame pixels. Authentication and recovery share the existing
Session implementation, including its single cross-tile decoder context.
"""

from __future__ import annotations

import dataclasses
import faulthandler
import logging
import os
import signal
import threading
import time

import glfw
import numpy as np

from ...proxy.session import Session
from ...proxy.media.native import library, error
from ...proxy.protocol.negotiation import AdvertiseDims
from ..desktop.audio_sink import make_audio_sink
from ..desktop.keymap import GLFW_KEY_TO_X11, glfw_button_to_rfb_bit
from ..desktop.app import _resolve_hidpi_request, _display_scale

log = logging.getLogger(__name__)


class Renderer:
    def __init__(self, width, height, generation):
        self.width, self.height, self.generation = width, height, generation
        self.lib = library()
        self.handle = self.lib.iss_renderer_new(width, height, generation)
        if not self.handle:
            raise RuntimeError(error(self.lib))

    def upload(self, frame, tile_index, slot_height):
        origin = tile_index * slot_height
        rows = min(frame.height, slot_height, self.height - origin)
        result = self.lib.iss_renderer_upload(self.handle, frame.handle, origin, rows)
        if result < 0:
            raise RuntimeError(error(self.lib))
        return result == 0

    def draw(self, width, height):
        if self.lib.iss_renderer_draw(self.handle, width, height) < 0:
            raise RuntimeError(error(self.lib))

    def close(self):
        if self.handle:
            self.lib.iss_renderer_free(self.handle)
            self.handle = None


def create_window(width, height, *, visible=True):
    if not glfw.init():
        raise RuntimeError("GLFW initialization failed")
    glfw.window_hint(glfw.CLIENT_API, glfw.OPENGL_ES_API)
    glfw.window_hint(glfw.CONTEXT_VERSION_MAJOR, 3)
    glfw.window_hint(glfw.CONTEXT_VERSION_MINOR, 0)
    glfw.window_hint(glfw.CONTEXT_CREATION_API, glfw.EGL_CONTEXT_API)
    glfw.window_hint(glfw.VISIBLE, glfw.TRUE if visible else glfw.FALSE)
    window = glfw.create_window(width, height, "iShareScreen — Native GPU", None, None)
    if not window:
        glfw.terminate()
        raise RuntimeError("Could not create an EGL/GLES3 viewer window")
    glfw.make_context_current(window)
    # Wayland's FIFO frame callback can block swap indefinitely when the
    # window is occluded, and a second pacing point adds latency to remote
    # frames. Let the compositor schedule presentation; keep an opt-in for
    # drivers that need application-side VSync.
    glfw.swap_interval(int(os.environ.get("ISS_NATIVE_VSYNC", "0")))
    return window


def run(config, *, auto_quit_secs=0, **_unused):
    faulthandler.enable()
    faulthandler.register(signal.SIGUSR1, all_threads=True)
    os.environ["ISS_NATIVE_GPU"] = "1"
    # Fixed 4K backing / Retina UI is the default for the experimental engine.
    if config.advertise is None:
        config = dataclasses.replace(
            config, advertise=AdvertiseDims(1920, 1080, hidpi_scale=2.0)
        )
    session = Session(config)
    sink = make_audio_sink() if config.audio else None
    if sink:
        sink.start()
        session.set_audio_callback(sink.feed)
    pending_cursor = [None]
    cursor_lock = threading.Lock()
    cursor_handle = None

    def receive_cursor(image):
        with cursor_lock:
            pending_cursor[0] = image

    session.set_cursor_callback(receive_cursor)
    # Negotiate before opening a window: the local connection form already
    # shows progress, and a blocked handshake must not create an unresponsive
    # GLFW window.
    try:
        session.connect()
        window = create_window(1280, 720)
    except BaseException:
        session.close()
        if sink:
            sink.stop()
        raise
    from .render_loop import RenderLoop

    render = RenderLoop(window, session, Renderer)
    buttons = 0
    position = (0, 0)

    def coordinates(x, y):
        ww, wh = glfw.get_window_size(window)
        cw, ch = session.canvas_dims
        fit = min(ww / cw, wh / ch) if cw and ch else 1
        return (
            max(0, min(cw - 1, int((x - (ww - cw * fit) / 2) / fit))),
            max(0, min(ch - 1, int((y - (wh - ch * fit) / 2) / fit))),
        )

    def pointer(_window, x, y):
        nonlocal position
        position = coordinates(x, y)
        session.input.pointer_event(buttons, *position)

    def mouse(_window, button, action, _mods):
        nonlocal buttons
        bit = glfw_button_to_rfb_bit(button)
        buttons = buttons | bit if action == glfw.PRESS else buttons & ~bit
        session.input.pointer_event(buttons, *position)

    def scroll(_window, dx, dy):
        session.input.scroll_event(*position, int(dx * 12), -int(dy * 12))

    held = {}

    def key(_window, key, _scan, action, mods):
        ks = held.get(key) if action == glfw.RELEASE else GLFW_KEY_TO_X11.get(key)
        if os.environ.get("ISS_CTRL_AS_CMD", "1") != "0" and key in (
            glfw.KEY_LEFT_CONTROL,
            glfw.KEY_RIGHT_CONTROL,
        ):
            ks = 0xFFEB if key == glfw.KEY_LEFT_CONTROL else 0xFFEC
        if not ks and mods & (glfw.MOD_CONTROL | glfw.MOD_SUPER | glfw.MOD_ALT):
            if glfw.KEY_A <= key <= glfw.KEY_Z:
                ks = ord("a") + key - glfw.KEY_A
            elif glfw.KEY_0 <= key <= glfw.KEY_9:
                ks = ord("0") + key - glfw.KEY_0
            elif key == glfw.KEY_SPACE:
                ks = ord(" ")
        if ks:
            session.input.key_event(action != glfw.RELEASE, ks)
            if action == glfw.RELEASE:
                held.pop(key, None)
            else:
                held[key] = ks

    def text(_window, cp):
        session.input.key_event(True, cp)
        session.input.key_event(False, cp)

    def focus(_window, focused):
        nonlocal buttons
        if not focused:
            for ks in set(held.values()):
                session.input.key_event(False, ks)
            held.clear()
            buttons = 0
            session.input.pointer_event(0, *position)

    glfw.set_cursor_pos_callback(window, pointer)
    glfw.set_mouse_button_callback(window, mouse)
    glfw.set_scroll_callback(window, scroll)
    glfw.set_key_callback(window, key)
    glfw.set_char_callback(window, text)
    glfw.set_window_focus_callback(window, focus)

    def request_redraw(*_args):
        render.update_view(*glfw.get_framebuffer_size(window), redraw=True)

    glfw.set_framebuffer_size_callback(window, request_redraw)
    glfw.set_window_refresh_callback(window, request_redraw)
    pending_size = None
    pending_since = 0
    last_resize = 0
    diagnostics_stop = threading.Event()

    def diagnose_stall():
        dumped = False
        while not diagnostics_stop.wait(1):
            stalled = time.monotonic() - render.heartbeat > 4
            if stalled and not dumped:
                for handler in logging.getLogger().handlers:
                    if isinstance(handler, logging.FileHandler) and handler.stream:
                        faulthandler.dump_traceback(
                            file=handler.stream, all_threads=True
                        )
                        break
                dumped = True
            elif not stalled:
                dumped = False

    try:
        started = time.monotonic()
        render.start()
        threading.Thread(
            target=diagnose_stall, name="native-stall-trace", daemon=True
        ).start()
        log.info(
            "native session: hardware=VAAPI renderer=EGL/DMA-BUF canvas=%s full-frame CPU copies=0",
            session.canvas_dims,
        )
        while not glfw.window_should_close(window):
            if not session.is_connected:
                break
            now = time.monotonic()
            if auto_quit_secs and now - started >= auto_quit_secs:
                break
            if render.error:
                raise RuntimeError("Native render worker failed") from render.error
            glfw.poll_events()
            render.update_view(
                *glfw.get_framebuffer_size(window),
                visible=not glfw.get_window_attrib(window, glfw.ICONIFIED),
            )
            with cursor_lock:
                image = pending_cursor[0]
                pending_cursor[0] = None
            if image:
                try:
                    pixels = np.frombuffer(image.rgba, dtype=np.uint8).reshape(
                        image.height, image.width, 4
                    )
                    new_cursor = glfw.create_cursor(
                        (image.width, image.height, pixels),
                        image.hotspot_x,
                        image.hotspot_y,
                    )
                except (ValueError, TypeError):
                    log.warning("Ignoring invalid remote cursor", exc_info=True)
                    new_cursor = None
                if new_cursor:
                    glfw.set_cursor(window, new_cursor)
                    if cursor_handle:
                        glfw.destroy_cursor(cursor_handle)
                    cursor_handle = new_cursor
            if config.dynamic_resolution:
                size = glfw.get_window_size(window)
                if size != pending_size:
                    pending_size = size
                    pending_since = now
                if now - pending_since > 0.5 and now - last_resize > 2.5:
                    rw, rh, scale = _resolve_hidpi_request(
                        config.hidpi, *size, _display_scale(window)
                    )
                    if (rw * scale, rh * scale) != session.canvas_dims:
                        session.send_dynamic_resolution(rw, rh, hidpi_scale=scale)
                        last_resize = now
            # GLFW event dispatch and input never wait for GPU work.
            time.sleep(0.002)
    finally:
        diagnostics_stop.set()
        if not render.stop():
            # A wedged driver must not keep the application alive after the
            # user closes it. Do not destroy a context still used by another
            # thread; process teardown releases its sockets and GPU resources.
            log.error("GPU worker did not stop; terminating the viewer")
            os._exit(1)
        session.close()
        if sink:
            sink.stop()
        if cursor_handle:
            glfw.destroy_cursor(cursor_handle)
        glfw.destroy_window(window)
        glfw.terminate()
    return 0
