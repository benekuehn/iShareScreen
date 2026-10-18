"""A requested accelerator is not proof; downloaded output is not disproof."""

from types import SimpleNamespace as S

from isharescreen.proxy.media.hardware import hardware_active, hardware_configured
from isharescreen.proxy.media import hwcaps


def test_software_fallback_with_output_is_not_hardware():
    assert not hardware_active(S(is_hwaccel=False), S(format=S(name="yuv444p")))
    assert not hardware_configured(S(is_hwaccel=False))


def test_downloaded_hardware_frame_keeps_hardware_label():
    assert hardware_active(S(is_hwaccel=True), S(format=S(name="yuv444p")))


def test_old_pyav_requires_a_hardware_surface():
    assert not hardware_active(
        S(hwaccel=S(device_type="vaapi")), S(format=S(name="nv12"))
    )
    assert hardware_active(S(), S(format=S(name="vaapi")))


def test_probe_rejects_silent_cpu_fallback(monkeypatch):
    import av

    fake = S(is_hwaccel=False, decode=lambda _packet: [S(format=S(name="yuv444p"))])
    monkeypatch.setattr(av, "CodecContext", S(create=lambda *_a, **_k: fake))
    assert not hwcaps._probe_one("vaapi")


def test_probe_accepts_downloaded_hardware_frame(monkeypatch):
    import av

    fake = S(is_hwaccel=True, decode=lambda _packet: [S(format=S(name="yuv444p"))])
    monkeypatch.setattr(av, "CodecContext", S(create=lambda *_a, **_k: fake))
    assert hwcaps._probe_one("vaapi")
