"""Optional native ABI tests, run with ISS_TEST_NATIVE=1 after building."""

import os
import socket
import time

import pytest

from isharescreen.proxy.media.native import NativeUdpReceiver

pytestmark = pytest.mark.skipif(
    os.environ.get("ISS_TEST_NATIVE") != "1",
    reason="requires built Linux native library",
)


def test_native_receiver_preserves_datagrams_and_socket_ownership():
    receiver_socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    receiver_socket.bind(("127.0.0.1", 0))
    sender = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    receiver = NativeUdpReceiver(receiver_socket.fileno(), capacity=1024)
    try:
        expected = [i.to_bytes(4, "big") + b"x" * 512 for i in range(500)]
        for packet in expected:
            sender.sendto(packet, receiver_socket.getsockname())
        deadline = time.monotonic() + 2
        actual = []
        while len(actual) < len(expected) and time.monotonic() < deadline:
            actual.extend(receiver.batch())
        assert actual == expected
        assert receiver.dropped == 0
    finally:
        receiver.close()
    # Freeing the native handle closes its duplicate, not the caller's fd.
    receiver_socket.settimeout(0.5)
    sender.sendto(b"still-owned", receiver_socket.getsockname())
    assert receiver_socket.recvfrom(1024)[0] == b"still-owned"
    sender.close()
    receiver_socket.close()


def test_native_overflow_is_bounded_and_counted():
    receiver_socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    receiver_socket.bind(("127.0.0.1", 0))
    sender = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    receiver = NativeUdpReceiver(receiver_socket.fileno(), capacity=1)
    try:
        for i in range(20):
            sender.sendto(bytes([i]), receiver_socket.getsockname())
        deadline = time.monotonic() + 2
        while receiver.dropped < 19 and time.monotonic() < deadline:
            time.sleep(0.005)
        assert receiver.dropped == 19
        assert receiver.batch() == [b"\x00"]
    finally:
        receiver.close()
        receiver_socket.close()
        sender.close()
