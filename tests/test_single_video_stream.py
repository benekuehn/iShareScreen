import struct

from isharescreen.proxy.protocol.negotiation import build_0x1c, NegotiationKeys


def test_single_stream_keeps_60fps_and_cursor_flags(monkeypatch):
    monkeypatch.setenv("ISS_SINGLE_VIDEO_STREAM", "1")
    monkeypatch.delenv("ISS_LEGACY_CURSOR", raising=False)
    keys = NegotiationKeys(*(b"\0" * 46 for _ in range(4)))
    message = build_0x1c(b"", b"", keys)
    assert struct.unpack_from(">I", message, 6)[0] == 5


def test_existing_two_tier_offer_is_default_outside_native(monkeypatch):
    monkeypatch.delenv("ISS_SINGLE_VIDEO_STREAM", raising=False)
    monkeypatch.delenv("ISS_LEGACY_CURSOR", raising=False)
    keys = NegotiationKeys(*(b"\0" * 46 for _ in range(4)))
    assert struct.unpack_from(">I", build_0x1c(b"", b"", keys), 6)[0] == 7
