import struct

import pytest

from isharescreen.proxy.protocol.rtcp import build_rr
from isharescreen.proxy.protocol.rtp_stats import ReceptionStats


def test_loss_is_reported_per_interval_and_late_repair_reduces_total():
    stats = ReceptionStats()
    for seq in (100, 101, 103):
        stats.observe(seq, seq * 1500, seq / 60)
    report = stats.report()
    assert report["lost"] == 1
    assert report["fraction_lost"] == 64  # 1 of 4 expected
    stats.observe(102, 102 * 1500, 104 / 60)
    repaired = stats.report()
    assert repaired["lost"] == repaired["fraction_lost"] == 0
    assert repaired["extended_seq"] == 103
    stats.observe(104, 104 * 1500, 105 / 60)
    assert stats.report()["fraction_lost"] == 0


def test_sequence_wrap_and_late_previous_cycle_do_not_inflate_loss():
    stats = ReceptionStats()
    for seq in (65534, 0, 65535, 1):
        stats.observe(seq, 0, 1)
    report = stats.report()
    assert report["extended_seq"] == 65537
    assert report["lost"] == report["fraction_lost"] == 0


def test_duplicates_can_make_cumulative_loss_negative_but_not_fraction():
    stats = ReceptionStats()
    for _ in range(2):
        stats.observe(10, 0, 0)
    report = stats.report()
    assert report["lost"] == -1
    assert report["fraction_lost"] == 0
    packet = build_rr(1, [2], {2: report})
    assert struct.unpack_from(">I", packet, 12)[0] == 0x00FFFFFF


def test_jitter_uses_rtp_clock_units_and_handles_timestamp_wrap():
    stats = ReceptionStats()
    stats.observe(1, 0xFFFFFFF0, 0)
    stats.observe(2, (0xFFFFFFF0 + 1500) & 0xFFFFFFFF, 1 / 60)
    assert stats.report()["jitter"] == 0
    stats.observe(3, (0xFFFFFFF0 + 3000) & 0xFFFFFFFF, 1 / 30 + 0.016)
    # 16 ms arrival variation = 1440 ticks, filtered by 1/16.
    assert stats.report()["jitter"] == pytest.approx(90, abs=1)


def test_receiver_report_encodes_measured_loss_sequence_and_jitter():
    stats = ReceptionStats()
    stats.observe(100, 0, 0)
    stats.observe(103, 3000, 0.05)
    report = stats.report()
    packet = build_rr(1, [2], {2: report})
    source, loss, highest, jitter = struct.unpack_from(">IIII", packet, 8)
    assert source == 2
    assert loss == (128 << 24) | 2
    assert highest == 103
    assert jitter == report["jitter"] > 0


def test_empty_stats_and_idle_report_do_not_invent_loss():
    stats = ReceptionStats()
    assert stats.report() == {}
    stats.observe(10, 0, 0)
    stats.report()
    assert stats.report()["fraction_lost"] == 0


def test_large_discontinuity_requires_confirmation_and_restarts_accounting():
    stats = ReceptionStats()
    stats.observe(100, 0, 0)
    stats.report()
    stats.observe(20000, 0, 0.01)
    assert stats.report()["extended_seq"] == 100
    stats.observe(20001, 0, 0.02)
    report = stats.report()
    assert report["extended_seq"] == 20001
    assert report["lost"] == report["fraction_lost"] == 0


def test_isolated_old_packet_does_not_advance_an_extra_sequence_cycle():
    stats = ReceptionStats()
    stats.observe(65000, 0, 0)
    stats.observe(1000, 0, 0.01)
    expected = stats.report()["extended_seq"]
    stats.observe(30000, 0, 0.02)
    assert stats.report()["extended_seq"] == expected


def test_a_large_4k_nack_repair_is_late_reception_not_a_source_restart():
    stats = ReceptionStats()
    stats.observe(1000, 0, 0)
    stats.observe(2001, 1500, 0.02)
    assert stats.report()["lost"] == 1000
    for seq in range(1001, 2001):
        stats.observe(seq, 0, 0.03)
    report = stats.report()
    assert report["extended_seq"] == 2001
    assert report["lost"] == report["fraction_lost"] == 0
