"""A stalled swap must not own the event thread; GL cleanup keeps ownership."""

import threading
import time
from types import SimpleNamespace as S

from isharescreen.frontend.native import render_loop


def test_ready_queue_keeps_newer_pending_work_without_waiting_for_it():
    queue = render_loop.ReadySurfaces()
    older = S(generation=1, ready=True)
    ready = S(generation=1, ready=True)
    pending = S(generation=1, ready=False)
    for frame in (older, ready, pending):
        queue.add(frame)
    assert queue.take() is ready
    assert queue.published == 3
    assert queue.taken == 1
    assert queue.superseded == 1
    assert queue.take() is None
    assert queue.not_ready == 1
    pending.ready = True
    assert queue.take() is pending


def test_ready_queue_bounds_retained_frames_and_rejects_old_generation():
    queue = render_loop.ReadySurfaces()
    for _ in range(20):
        queue.add(S(generation=1, ready=False))
    assert len(queue.frames) == 4
    current = S(generation=2, ready=True)
    queue.add(current)
    queue.add(S(generation=1, ready=True))
    assert queue.take() is current
    assert not queue.frames


def test_swap_wait_does_not_block_view_updates_and_cleanup_stays_on_gpu_thread(
    monkeypatch,
):
    swapping = threading.Event()
    release = threading.Event()
    calls = []
    monkeypatch.setattr(render_loop.glfw, "get_framebuffer_size", lambda _: (1280, 720))
    monkeypatch.setattr(
        render_loop.glfw,
        "make_context_current",
        lambda window: calls.append(("context", threading.get_ident(), window)),
    )
    monkeypatch.setattr(render_loop.glfw, "swap_interval", lambda _: None)

    def swap(_):
        swapping.set()
        release.wait(2)

    monkeypatch.setattr(render_loop.glfw, "swap_buffers", swap)
    frame = S(gpu_surface=True, generation=1, height=2160, ready=True)

    class Renderer:
        def __init__(self, *_):
            self.generation = 1

        def upload(self, *_):
            return True

        def draw(self, *_):
            pass

        def close(self):
            calls.append(("close", threading.get_ident(), None))

    session = S(
        is_connected=True,
        canvas_dims=(3840, 2160),
        num_tiles=1,
        _decoder=S(good_counts=[1], decode_queue_depth=0),
        get_frame=lambda _: frame,
        wait_for_fresh_tile=lambda _: None,
    )
    render = render_loop.RenderLoop("window", session, Renderer)
    render.start()
    try:
        assert swapping.wait(2)
        # Main can publish resize/expose state while the driver is blocked.
        render.update_view(1920, 1080, redraw=True)
        assert render.viewport[:2] == (1920, 1080)
        assert render.thread.is_alive()
    finally:
        render.stop_event.set()
        release.set()
        assert render.stop()
    assert render.error is None
    assert ("close", render.thread.ident, None) in calls
    assert calls[0] == ("context", threading.get_ident(), None)


def test_pending_presentation_does_not_consume_frames_or_block_main(monkeypatch):
    waiting = threading.Event()
    monkeypatch.setattr(render_loop.glfw, "get_framebuffer_size", lambda _: (1280, 720))
    monkeypatch.setattr(render_loop.glfw, "make_context_current", lambda _: None)
    monkeypatch.setattr(render_loop.glfw, "swap_interval", lambda _: None)

    def unexpected(*_):
        raise AssertionError(
            "pending presentation must retain completed frames for later"
        )

    def wait(_):
        waiting.set()
        threading.Event().wait(0.001)

    session = S(
        is_connected=True,
        canvas_dims=(3840, 2160),
        num_tiles=1,
        _decoder=S(good_counts=[1], decode_queue_depth=0),
        get_frame=unexpected,
        wait_for_fresh_tile=wait,
    )
    observer = S(can_submit=lambda: False)
    render = render_loop.RenderLoop(
        "window", session, unexpected, presentation=observer
    )
    render.start()
    try:
        assert waiting.wait(2)
        render.update_view(1920, 1080, redraw=True)
        assert render.viewport[:2] == (1920, 1080)
    finally:
        assert render.stop()
    assert render.error is None


def test_background_wait_ignores_frame_arrivals_and_focus_restore_wakes_immediately(monkeypatch):
    monkeypatch.setattr(render_loop.glfw, "get_framebuffer_size", lambda _: (1280,720))
    monkeypatch.setattr(render_loop.glfw, "make_context_current", lambda _: None)
    monkeypatch.setattr(render_loop.glfw, "swap_interval", lambda _: None)
    first, resumed = threading.Event(), threading.Event()
    swaps, reads = [], []
    def swap(_):
        swaps.append(time.monotonic())
        (first if len(swaps) == 1 else resumed).set()
    monkeypatch.setattr(render_loop.glfw, "swap_buffers", swap)
    class Budget:
        focused = False
        last = 0
        def window(self, focused, visible): self.focused = focused
        def submitted(self): self.last = time.monotonic()
        def render_delay(self): return 0 if self.focused else max(0,self.last+0.2-time.monotonic())
    frame = S(gpu_surface=True,generation=1,height=2160,ready=True)
    def get_frame(_):
        reads.append(1)
        return frame if len(reads) <= 2 else None
    class Renderer:
        generation = 1
        def __init__(self,*_): pass
        def upload(self,*_): return True
        def draw(self,*_): pass
        def close(self): pass
    session = S(is_connected=True,canvas_dims=(3840,2160),num_tiles=1,
                _decoder=S(good_counts=[1],decode_queue_depth=0),get_frame=get_frame,
                wait_for_fresh_tile=lambda timeout: threading.Event().wait(min(timeout,0.01)))
    render=render_loop.RenderLoop("window",session,Renderer,activity=Budget())
    render.update_view(1280,720,focused=False)
    render.start()
    try:
        assert first.wait(1)
        time.sleep(0.06)
        assert len(reads) == 1  # fresh frames don't turn background waits into 60 Hz polling
        render.update_view(1280,720,focused=True)
        assert resumed.wait(0.1)  # does not wait for the remaining 200 ms budget
    finally:
        assert render.stop()
    assert render.error is None


def test_minimizing_releases_compositor_and_restore_keeps_session(monkeypatch):
    monkeypatch.setattr(render_loop.glfw, "get_framebuffer_size", lambda _: (1280, 720))
    monkeypatch.setattr(render_loop.glfw, "make_context_current", lambda _: None)
    monkeypatch.setattr(render_loop.glfw, "swap_interval", lambda _: None)
    presented, released, restored = (threading.Event() for _ in range(3))
    created = []
    swaps = []

    def swap(_):
        swaps.append(1)
        (restored if len(created) > 1 else presented).set()
        time.sleep(0.001)

    monkeypatch.setattr(render_loop.glfw, "swap_buffers", swap)

    class Renderer:
        generation = 1

        def __init__(self, *_):
            created.append(self)

        def upload(self, *_):
            return True

        def draw(self, *_):
            pass

        def close(self):
            released.set()

    frame = S(gpu_surface=True, generation=1, height=2160, ready=True)
    session = S(
        is_connected=True, canvas_dims=(3840, 2160), num_tiles=1,
        _decoder=S(good_counts=[1], decode_queue_depth=0),
        get_frame=lambda _: frame, wait_for_fresh_tile=lambda _: None,
    )
    render = render_loop.RenderLoop("window", session, Renderer)
    render.start()
    try:
        assert presented.wait(1)
        render.update_view(1280, 720, visible=False, focused=False)
        assert released.wait(1)
        count = len(swaps)
        time.sleep(0.03)
        assert len(swaps) == count
        assert session.is_connected
        render.update_view(1280, 720, visible=True, focused=True)
        assert restored.wait(0.1)
        assert session.is_connected
    finally:
        assert render.stop()
    assert render.error is None
