from collections import defaultdict
from types import SimpleNamespace as S

import pytest

from isharescreen.proxy.session import Session
from isharescreen.proxy.protocol.rtp_stats import ReceptionStats
import struct


@pytest.mark.parametrize("packet_type", [192, 200, 201, 202, 204, 205, 206, 207])
def test_rtcp_types_are_not_misclassified_as_audio(packet_type):
    assert Session._is_rtcp_packet(bytes([0x80, packet_type, 0, 1]) + b"\0" * 4)


@pytest.mark.parametrize("marker", [0, 128])
def test_audio_payload_101_is_not_rtcp(marker):
    assert not Session._is_rtcp_packet(bytes([0x80, 101 | marker, 0, 1]) + b"\0" * 8)


def test_video_feedback_uses_video_port_for_nack_rr_and_fir(monkeypatch):
    monkeypatch.setenv("ISS_VIDEO_RTCP_MUX", "1")
    session = Session.__new__(Session)
    audio, video = [], []
    session._sock_ctrl = S(sendto=lambda *args: audio.append(args))
    session._sock_video = S(sendto=lambda *args: video.append(args))
    session._config = S(port=5900, udp_ctrl_port=None, udp_video_port=None)
    session._dest_host = "127.0.0.1"
    session._our_video_ssrc = 3
    session._srtcp_enc = S(protect=lambda data: data)
    session._ssrc_to_tile = {7: 0}
    session._nack_pending = defaultdict(set, {7: {10, 11}})
    session._max_seq = {7: 12}
    session._roc = {7: 0}
    session._server_sr = {}
    session._tx_tick = 1
    session._last_fir_per_tile = {}
    received = ReceptionStats()
    received.observe(10, 0, 0)
    received.observe(12, 3000, 0.05)
    session._video_receiver_stats = {7: received}
    session._drain_pending_nack()
    session._send_rr_and_maybe_sr()
    assert session._send_fir_for_tile(0, log_per_tile=False, record_grayout=False)
    assert not audio
    assert len(video) == 3
    assert all(address == ("127.0.0.1", 5901) for _, address in video)
    # The regular RR must carry measured loss, rather than placeholder zeroes.
    rr = video[1][0]
    loss_word = struct.unpack_from(">I", rr, 12)[0]
    assert loss_word & 0xFFFFFF == 1
    assert loss_word >> 24 == 85  # 1 of 3 expected packets
    assert session._last_receiver_reports[7]["jitter"] > 0
