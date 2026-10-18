"""A stalled swap must not own the event thread; GL cleanup keeps ownership."""

import threading
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
