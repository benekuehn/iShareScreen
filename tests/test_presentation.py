import time
from types import SimpleNamespace as S

from isharescreen.frontend.native.presentation import Presentation
from isharescreen.frontend.native import presentation


def event(frame_id, presented_ms, **kwargs):
    data = dict(
        frame_id=frame_id,
        presented_ns=int(presented_ms * 1e6),
        submit_ns=0,
        decode_submit_ns=0,
        gpu_ready_ns=0,
        refresh_ns=16666667,
        sequence=frame_id,
        flags=7,
        discarded=0,
    )
    data.update(kwargs)
    return data


def test_counts_actual_presentations_and_discards_and_measures_clock_matched_latency(
    monkeypatch,
):
    observer = Presentation(
        S(
            iss_presentation_clock=lambda _: time.CLOCK_MONOTONIC,
            iss_presentation_dropped=lambda _: 0,
        ),
        1,
    )
    events = [
        event(
            1,
            1010,
            submit_ns=1000_000_000,
            decode_submit_ns=990_000_000,
            gpu_ready_ns=995_000_000,
        ),
        event(2, 1026.666667),
        event(3, 0, discarded=1),
    ]
    monkeypatch.setattr(observer, "read", lambda: events)
    stats = observer.profile(1)
    assert stats["displayed_fps"] == 2
    assert stats["discarded"] == 1
    assert stats["gap_p95_ms"] == 16.667
    assert stats["submit_to_display_p95_ms"] == 10
    assert stats["decode_to_display_p95_ms"] == 20
    assert stats["ready_to_display_p95_ms"] == 15
    assert stats["flags"] == 7


def test_never_subtracts_timestamps_from_different_clock_domains(monkeypatch):
    observer = Presentation(
        S(iss_presentation_clock=lambda _: 12345, iss_presentation_dropped=lambda _: 3),
        1,
    )
    monkeypatch.setattr(observer, "read", lambda: [event(1, 1000, submit_ns=1)])
    stats = observer.profile(1)
    assert stats["displayed_fps"] == 1
    assert stats["submit_to_display_p95_ms"] is None
    assert stats["feedback_dropped"] == 3


def test_gaps_cross_profile_boundaries_and_late_feedback_is_explicit(monkeypatch):
    observer = Presentation(
        S(
            iss_presentation_clock=lambda _: time.CLOCK_MONOTONIC,
            iss_presentation_dropped=lambda _: 0,
        ),
        1,
    )
    batches = iter([[event(1, 1000)], [event(2, 1050)], [event(3, 1020)]])
    monkeypatch.setattr(observer, "read", lambda: next(batches))
    assert observer.profile(1)["gap_p95_ms"] is None
    assert observer.profile(1)["gap_max_ms"] == 50
    stats = observer.profile(1)
    assert stats["late_feedback"] == 1
    assert stats["gap_max_ms"] is None


def test_pacing_waits_for_feedback_without_blocking_and_bounds_stalled_feedback(
    monkeypatch,
):
    monkeypatch.setenv("ISS_NATIVE_PRESENT_PACING", "1")
    now, pending = [10.0], [0]
    monkeypatch.setattr(presentation.time, "monotonic", lambda: now[0])
    observer = Presentation(
        S(
            iss_presentation_clock=lambda _: time.CLOCK_MONOTONIC,
            iss_presentation_frame_pending=lambda _: pending[0],
            iss_presentation_request=lambda *_: 1,
        ),
        1,
    )
    assert observer.can_submit()
    observer.request(1, S())
    pending[0] = 1
    assert not observer.can_submit()
    now[0] += 0.3
    assert observer.can_submit()  # bounded retry even if no feedback arrives
    observer.request(2, S())
    assert not observer.can_submit()
    pending[0] = 0
    assert observer.can_submit()
