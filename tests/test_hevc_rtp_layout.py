"""Synthetic RTP fixtures for tiled Apple DONL and single-picture HEVC."""

import struct

import pytest

from isharescreen.proxy.media.nalu import (
    aggregation_has_donl,
    reassemble_group,
    first_donl,
)


@pytest.mark.parametrize("has_donl", [True, False])
def test_ap_layout_and_parameter_sets(has_donl):
    nalus = [b"\x40\x01\x80", b"\x42\x01\x80", b"\x44\x01\x80"]
    ap = b"\x60\x01" + (b"\x00\x47" if has_donl else b"")
    ap += b"".join(struct.pack(">H", len(n)) + n for n in nalus)
    assert aggregation_has_donl(ap) is has_donl
    assert reassemble_group([ap], has_donl=has_donl) == nalus
    assert first_donl([ap], has_donl=has_donl) == (71 if has_donl else None)


@pytest.mark.parametrize("has_donl", [True, False])
def test_fragment_and_single_picture_bytes_survive_unchanged(has_donl):
    donl = b"\x00\x47" if has_donl else b""
    start = b"\x62\x01\x94" + donl + b"\x80first"
    end = b"\x62\x01\x54" + donl + b"last"
    assert reassemble_group([start, end], has_donl=has_donl) == [
        b"\x28\x01\x80firstlast"
    ]
    single = b"\x02\x01" + donl + b"\x80picture"
    assert reassemble_group([single], has_donl=has_donl) == [b"\x02\x01\x80picture"]


def test_malformed_ap_is_not_mistaken_for_a_layout():
    assert aggregation_has_donl(b"\x60\x01\x00\xff\x40\x01") is None
    assert aggregation_has_donl(b"\x60\x01\x00\x03\x40\x00\x80") is None
