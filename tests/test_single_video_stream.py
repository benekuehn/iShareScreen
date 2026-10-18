import struct
import plistlib
import zlib
import pytest

from isharescreen.proxy.protocol.negotiation import build_0x1c, NegotiationKeys
from isharescreen.proxy.protocol.offers import create_offers, _read_varint


def _fields(data):
    fields = {}
    pos = 0
    while pos < len(data):
        tag, pos = _read_varint(data, pos)
        wire = tag & 7
        if wire == 0:
            value, pos = _read_varint(data, pos)
        elif wire == 2:
            length, pos = _read_varint(data, pos)
            value = data[pos : pos + length]
            pos += length
        else:
            raise AssertionError(f"unexpected offer wire type {wire}")
        fields[tag >> 3] = value
    return fields


@pytest.mark.parametrize(
    "codec,tiles,ltr,expected",
    [
        ("hevc", 1, "1", 0),
        ("avc", 1, "1", 0),
        ("hevc", 4, "1", 1),
        ("hevc", 4, "0", 0),
    ],
)
def test_rtcp_feedback_stays_enabled_independently_of_ltr(
    monkeypatch, codec, tiles, ltr, expected
):
    monkeypatch.setenv("ISS_VIDEO_CODEC", codec)
    monkeypatch.setenv("ISS_TILES_PER_FRAME", str(tiles))
    monkeypatch.setenv("ISS_LTRP", ltr)
    video, _ = create_offers()
    blob = zlib.decompress(plistlib.loads(video)["avcMediaStreamNegotiatorMediaBlob"])
    settings = _fields(_fields(blob)[5])
    assert settings[2] == 1  # allowRTCPFB
    assert settings[7] == expected  # ltrpEnabled


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
