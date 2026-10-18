"""Check the active decoder, rather than the requested HWAccel settings.

PyAV wheels can lack a requested backend and silently construct a software
context even with allow_software_fallback=False. Conversely, PyAV normally
downloads genuine hardware frames, so a CPU output format does not prove a
software decoder. PyAV 19's is_hwaccel property resolves both cases.
"""

from __future__ import annotations

from .decode_common import _HW_FRAME_FORMATS


def hardware_active(context, frame=None) -> bool:
    active = getattr(context, "is_hwaccel", None)
    if active is not None:
        return bool(active)
    # Older PyAV does not expose live context state. Only claim hardware
    # when a hardware-owned surface proves it; settings objects aren't proof.
    return frame is not None and frame.format.name in _HW_FRAME_FORMATS


def hardware_configured(context) -> bool:
    """Reject known software-only contexts; allow older PyAV to decode first."""
    return getattr(context, "is_hwaccel", None) is not False
