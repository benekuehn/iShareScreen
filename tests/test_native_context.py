"""Packet retry and strict hardware probing at the opaque native ABI boundary."""

from types import SimpleNamespace as S

import av
import pytest

from isharescreen.proxy.media import native


def test_surface_readiness_records_observation_once_and_caches_completion():
    states = iter([0, 1])
    frame = native.SurfaceFrame.__new__(native.SurfaceFrame)
    frame.handle = 123
    frame.gpu_ready_observed_ns = None
    frame._lib = S(
        iss_frame_ready=lambda _: next(states), iss_frame_free=lambda _: None
    )
    assert not frame.ready
    assert frame.gpu_ready_observed_ns is None
    assert frame.ready
    observed = frame.gpu_ready_observed_ns
    assert observed is not None
    assert frame.ready
    assert frame.gpu_ready_observed_ns == observed


def test_unsupported_readiness_uses_sync_upload_without_completion_timestamp():
    frame = native.SurfaceFrame.__new__(native.SurfaceFrame)
    frame.handle = 123
    frame.gpu_ready_observed_ns = None
    frame._lib = S(iss_frame_ready=lambda _: -1, iss_frame_free=lambda _: None)
    assert frame.ready
    assert frame.gpu_ready_observed_ns is None


def context_with(lib):
    context = native.NativeContext.__new__(native.NativeContext)
    context._handle = 123
    context._lib = lib
    return context


def test_eagain_drains_and_retries_identical_packet(monkeypatch):
    monkeypatch.setenv("ISS_NATIVE_SYNC_DECODE", "0")
    sent = []
    received = iter([11, None, 12, None])

    def send(handle, data, size, pts):
        sent.append((data, size, pts))
        return 1 if len(sent) == 1 else 0

    lib = S(
        iss_decoder_send=send,
        iss_decoder_receive=lambda _: next(received),
        iss_error=lambda: b"",
        iss_decoder_free=lambda _: None,
    )
    monkeypatch.setattr(native, "SurfaceFrame", lambda _lib, handle: handle)
    context = context_with(lib)
    packet = av.Packet(b"compressed-access-unit")
    packet.pts = 47
    assert context.decode(packet) == [11, 12]
    assert sent == [(b"compressed-access-unit", 22, 47)] * 2
    context.close()


def test_receive_error_does_not_look_like_empty_output():
    lib = S(
        iss_decoder_receive=lambda _: None,
        iss_error=lambda: b"GPU decode failed",
        iss_decoder_free=lambda _: None,
    )
    context = context_with(lib)
    with pytest.raises(RuntimeError, match="GPU decode failed"):
        context._receive()
    context.close()


def test_receive_waits_for_gpu_on_decoder_worker_before_publishing(monkeypatch):
    monkeypatch.setenv("ISS_NATIVE_SYNC_DECODE", "1")
    events = []
    received = iter([11, None])
    surface = S(gpu_ready_observed_ns=None)
    lib = S(
        iss_decoder_receive=lambda _: next(received),
        iss_frame_wait=lambda handle: events.append(("wait", handle)) or 0,
        iss_error=lambda: b"",
        iss_decoder_free=lambda _: None,
    )
    monkeypatch.setattr(native, "SurfaceFrame", lambda *_: surface)
    context = context_with(lib)
    assert context._receive() == [surface]
    assert events == [("wait", 11)]
    assert surface.gpu_ready_observed_ns is not None
    context.close()


def test_native_probe_rejects_no_output_and_closes_context(monkeypatch):
    closed = []
    monkeypatch.setenv("ISS_NATIVE_GPU", "1")
    monkeypatch.setattr(native.sys, "platform", "linux")
    monkeypatch.setattr(
        native,
        "NativeContext",
        lambda *_: S(decode=lambda _: [], close=lambda: closed.append(True)),
    )
    native.available.cache_clear()
    try:
        assert not native.available("hevc")
        assert closed == [True]
    finally:
        native.available.cache_clear()


def test_native_probe_requires_444_gpu_output(monkeypatch):
    monkeypatch.setenv("ISS_NATIVE_GPU", "1")
    monkeypatch.setattr(native.sys, "platform", "linux")
    monkeypatch.setattr(
        native,
        "NativeContext",
        lambda *_: S(
            decode=lambda _: [S(gpu_surface=True, is_yuv444=False)], close=lambda: None
        ),
    )
    native.available.cache_clear()
    try:
        assert not native.available("hevc")
        assert native.available("h264")
    finally:
        native.available.cache_clear()


def test_decoder_retains_completed_picture_when_newer_gpu_work_is_pending():
    decoder = native.make_decoder("hevc", 1)
    decoder._dpb_has_idr = True
    decoder._pts_to_tile = {1: 0, 2: 0}
    first = S(pts=1, generation=1, ready=True, decode_error_flags=0, flags=0)
    newer = S(pts=2, generation=1, ready=False, decode_error_flags=0, flags=0)
    try:
        decoder._publish_frame(first)
        decoder._publish_frame(newer)
        # The generic slot now points at newer; native must still return the
        # completed picture rather than overwrite it or wait on the GPU.
        assert decoder._tiles[0].raw_frame is newer
        assert decoder.get_frame(0) is first
        assert decoder.get_frame(0) is None
        newer.ready = True
        assert decoder.get_frame(0) is newer
    finally:
        decoder.close()
