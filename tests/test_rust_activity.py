"""Shared native power policy through the real ABI; no UI automation."""
import os
import pytest

from isharescreen.frontend.native.activity import ActivityBudget
from isharescreen.frontend.native import activity

pytestmark = pytest.mark.skipif(os.environ.get("ISS_TEST_RUST_PROTOCOL") != "1", reason="requires the native Rust core")


def test_five_fps_background_budget_and_immediate_foreground_restore(monkeypatch):
    now = [0]
    monkeypatch.setattr(activity.time, "monotonic_ns", lambda: now[0])
    budget = ActivityBudget(5)
    budget.window(False, True)
    outputs = presents = 0
    for frame in range(600):
        now[0] = frame * 1_000_000_000 // 60
        outputs += budget.take_output()
        if budget.render_delay() == 0:
            presents += 1
            budget.submitted()
    assert outputs == 50
    assert presents == 50
    stats = budget.snapshot()
    assert stats["mode"] == "background"
    assert stats["outputs_suppressed"] == 550
    assert stats["host_fps_controlled"] is False
    # Focus restores immediately, even within the last background interval.
    budget.window(True, True)
    assert budget.render_delay() == 0
    assert budget.take_output()
    assert budget.event_wait() < 0.017


def test_idle_and_minimized_event_waits_are_interruptible_by_activity(monkeypatch):
    now = [2_000_000_000]
    monkeypatch.setattr(activity.time, "monotonic_ns", lambda: now[0])
    budget = ActivityBudget()
    assert budget.event_wait() == 0.2
    budget.input()
    assert budget.event_wait() < 0.017
    budget.window(False, False)
    assert budget.event_wait() == 0.5
    assert budget.render_delay() == 0.5
    budget.window(True, True)
    assert budget.render_delay() == 0
    assert budget.take_output()
