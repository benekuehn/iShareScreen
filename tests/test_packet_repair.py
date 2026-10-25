"""Recovery timing under lost NACKs/retransmits and bounded live state."""

import importlib
from types import SimpleNamespace as S
import pytest

from isharescreen.proxy.protocol.rtp_repair import PacketRepair
from isharescreen.proxy.session import Session


def test_retries_lost_request_and_stops_after_retransmission():
    repair = PacketRepair()
    repair.missing(7, [65535, 0], 0)
    assert repair.due(0) == [(7, [65535, 0])]
    assert repair.due(0.019) == []
    # The first request was lost. Retry both missing packets after 20 ms.
    assert repair.due(0.020) == [(7, [65535, 0])]
    repair.received(7, 65535, 0.025)
    assert repair.due(0.040) == [(7, [0])]
    repair.received(7, 0, 0.045)
    assert repair.due(0.060) == []
    assert repair.snapshot()["returned_after_nack"] == 2
    assert repair.snapshot()["pending"] == 0


def test_retry_limit_and_deadline_do_not_extend_frame_repair_window():
    repair = PacketRepair()
    repair.missing(7, [10], 0)
    assert repair.due(0)
    assert repair.due(0.020)
    assert repair.due(0.040)
    assert repair.due(0.060) == []  # three attempts, not a NACK storm
    assert abs(repair.next_delay(0.060, 0.5) - 0.015) < 1e-9
    assert repair.due(0.076) == []
    assert repair.snapshot()["expired"] == 1
    assert repair.next_delay(0.076, 0.5) == pytest.approx(0.5)


def test_reordered_packet_cancels_unsent_nack_and_sources_are_independent():
    repair = PacketRepair()
    repair.missing(7, [10], 0)
    repair.missing(8, [10], 0)
    repair.received(7, 10, 0.001)
    assert repair.due(0.002) == [(8, [10])]
    assert repair.snapshot()["returned"] == 1
    assert repair.snapshot()["returned_after_nack"] == 0


def test_pending_packet_and_ssrc_generations_are_bounded():
    repair = PacketRepair(packet_cap=4, source_cap=2)
    repair.missing(7, range(10), 0)
    assert repair.due(0) == [(7, [6, 7, 8, 9])]
    repair.missing(8, [10], 0)
    repair.missing(9, [11], 0)
    assert repair.snapshot()["pending"] == 2
    assert repair.snapshot()["evicted"] == 10
    assert repair.due(0) == [(8, [10]), (9, [11])]


def test_retry_wakes_do_not_accelerate_heartbeat_rr_or_cursor_poll(monkeypatch):
    module = importlib.import_module("isharescreen.proxy.session")
    clock = [0.0]
    stopped = [False]
    periodic = []
    requests = []
    session = Session.__new__(Session)
    session._rtp_repair = PacketRepair()
    session._rtp_repair.missing(7, [10], 0)
    session._stop_evt = S(is_set=lambda: stopped[0])

    def wait(timeout):
        assert timeout > 0  # no busy spin between retries
        clock[0] += timeout
        stopped[0] = clock[0] >= 0.6

    session._tx_wakeup = S(clear=lambda: None, wait=wait)
    session._tx_tick = 0
    session._needs_post_layout_fir = False
    for name in ("_send_heartbeat", "_send_rr_and_maybe_sr", "_maybe_poll_cursor"):
        setattr(session, name, lambda name=name: periodic.append((name, clock[0])))
    for name in (
        "_drain_pending_fir",
        "_maybe_reanchor_d3d11va_avc",
        "_check_stall",
        "_log_profile_snapshot",
    ):
        setattr(session, name, lambda: None)
    session._drain_pending_nack = lambda: requests.extend(
        (clock[0], source, sequences)
        for source, sequences in session._rtp_repair.due(clock[0])
    )
    monkeypatch.setattr(module, "time", S(monotonic=lambda: clock[0]))
    session._tx_loop()
    assert [timestamp for timestamp, _, _ in requests] == [0, 0.020, 0.040]
    for name in ("_send_heartbeat", "_send_rr_and_maybe_sr", "_maybe_poll_cursor"):
        assert [timestamp for method, timestamp in periodic if method == name] == [
            0,
            0.5,
        ]
