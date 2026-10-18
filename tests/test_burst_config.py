"""Configuration packets need no RTP picture-end marker."""

import struct
from types import SimpleNamespace as S
import pytest

from isharescreen.proxy.protocol.burst import gather_initial_burst


@pytest.mark.parametrize("has_donl", [True, False])
def test_markerless_hevc_config_seeds_separate_picture_timestamp(has_donl):
    vps, sps, pps = b"\x40\x01\x80", b"\x42\x01\x80", b"\x44\x01\x80"
    config = (
        b"\x60\x01"
        + (b"\x00\x00" if has_donl else b"")
        + b"".join(struct.pack(">H", len(n)) + n for n in (vps, sps, pps))
    )
    idr = b"\x28\x01\x80\x01"
    picture = idr[:2] + (b"\x00\x01" if has_donl else b"") + idr[2:]

    def rtp(seq, timestamp, marker):
        return struct.pack(
            ">BBHII", 0x80, 96 | (128 if marker else 0), seq, timestamp, 77
        )

    packets = [(rtp(1, 0, False), config), (rtp(2, 90, True), picture)]
    decryptor = S(
        decrypt=lambda index: packets[index], get_primary_ssrc_group=lambda **_: [77]
    )
    burst = gather_initial_burst(
        [0, 1], decryptor, settle_seconds=0, deadline_seconds=0, min_packets=0
    )
    assert (burst.vps, burst.sps, burst.all_pps) == (vps, sps, {0: pps})
    assert burst.last_idr == {0: idr}
    assert burst.tile_nalus == {0: [idr]}
    assert burst.has_donl is has_donl


def test_incomplete_picture_is_not_published_with_markerless_config():
    vps, sps, pps = b"\x40\x01\x80", b"\x42\x01\x80", b"\x44\x01\x80"
    config = b"\x60\x01\x00\x00" + b"".join(
        struct.pack(">H", len(n)) + n for n in (vps, sps, pps)
    )
    header = struct.pack(">BBHII", 0x80, 96, 1, 0, 77)
    decryptor = S(
        decrypt=lambda _: (header, config), get_primary_ssrc_group=lambda **_: [77]
    )
    burst = gather_initial_burst(
        [0], decryptor, settle_seconds=0, deadline_seconds=0, min_packets=0
    )
    assert not burst.tile_nalus
    assert (77, 0) in burst.burst_pending
