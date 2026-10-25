"""Real C ABI interoperability; enable with ISS_TEST_RUST_PROTOCOL=1."""

import ctypes as C
import json
import os
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor

import pytest

from isharescreen.proxy.protocol import rust_core
from isharescreen.proxy.protocol.enc1103 import StreamCipher as ReferenceCipher
from isharescreen.proxy.protocol.srtp import SRTPEncryptor

pytestmark = pytest.mark.skipif(
    os.environ.get("ISS_TEST_RUST_PROTOCOL") != "1",
    reason="requires a built Rust protocol core",
)


@pytest.fixture
def vectors():
    data = json.loads((Path(__file__).parents[1] / "protocol/tests/vectors.json").read_text())
    assert data["synthetic_only"] is True
    return data


def test_real_abi_decrypts_reference_rtp_and_keeps_per_source_state(vectors):
    receiver = rust_core.Receiver(bytes.fromhex(vectors["blob"]))
    for vector in vectors["rtp"]:
        assert receiver.decrypt(bytes.fromhex(vector["packet"])) == (
            bytes.fromhex(vector["header"]), bytes.fromhex(vector["payload"]),
        )
    assert receiver.state_snapshot()[0x12345678]["roc"] == 1
    assert receiver.ssrc_counts[0x12345678] == 5
    receiver.forget_ssrcs_except({0x12345678})
    assert set(receiver.ssrc_counts) == {0x12345678}
    packet = bytearray.fromhex(vectors["rtp"][0]["packet"])
    packet[-1] ^= 1
    assert receiver.decrypt(packet) is None
    assert receiver.ssrc_counts[0x12345678] == 5


def test_sender_is_byte_identical_to_reference_and_insufficient_output_does_not_consume_counter(vectors):
    blob = bytes.fromhex(vectors["blob"])
    sender = rust_core.Sender(blob, 42)
    out = C.create_string_buffer(1)
    meta = C.c_uint32(101)
    assert sender.lib.iss_crypto_transform(sender.handle, 1, b"payload", 7, out, 1, C.byref(meta)) == -1
    reference = SRTPEncryptor(blob[:32], blob[32:], 42)
    for marker in (False, True, False):
        assert sender.encrypt(b"synthetic payload", marker=marker) == reference.encrypt(b"synthetic payload", marker=marker)


def test_srtcp_ffi_matches_reference_including_unencrypted_and_last_index(vectors):
    control = rust_core.Control(bytes.fromhex(vectors["blob"]))
    for i, vector in enumerate(vectors["rtcp"]):
        packet, plain = bytes.fromhex(vector["packet"]), bytes.fromhex(vector["plain"])
        assert control.unprotect(packet) == plain
        if i < 2:
            assert control.protect(plain) == packet


def test_control_ffi_handles_empty_records_counter_gaps_and_bad_mac(vectors):
    key, blob = bytes.fromhex(vectors["auth_key"]), bytes.fromhex(vectors["control_blob"])
    receiver = rust_core.StreamCipher(blob, ecb_key=key)
    reference = ReferenceCipher(blob, ecb_key=key)
    assert receiver.cbc_key == reference.cbc_key
    for vector in vectors["records"]:
        expected = bytes.fromhex(vector["plain"]) if vector["plain"] is not None else None
        assert receiver.decrypt_message(bytes.fromhex(vector["packet"])[2:]) == expected


def test_control_send_order_stays_atomic_across_threads(vectors):
    key, blob = bytes.fromhex(vectors["auth_key"]), bytes.fromhex(vectors["control_blob"])
    sender = rust_core.StreamCipher(blob, ecb_key=key)
    reference = ReferenceCipher(blob, ecb_key=key)
    records = []
    class Socket:
        def sendall(self, data):
            records.append(data)
    messages = [f"synthetic {i}".encode() for i in range(64)]
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(lambda message: sender.encrypt_and_send(Socket(), message), messages))
    decoded = [reference.decrypt_message(packet[2:]) for packet in records]
    assert set(decoded) == set(messages)
    assert len(decoded) == 64


@pytest.mark.parametrize("donl", [False, True])
def test_hevc_ffi_removes_donl_and_rejects_partial_fragment(donl):
    prefix = b"\0\x09" if donl else b""
    first = b"\x62\x01\x81" + prefix + b"\x80synthetic "
    last = b"\x62\x01\x41" + prefix + b"payload"
    assert rust_core.reassemble([first, last], has_donl=donl) == [b"\x02\x01\x80synthetic payload"]
    assert rust_core.reassemble([first], has_donl=donl) == []
    assert rust_core.reassemble([last], has_donl=donl) == []


def test_startup_can_harvest_parameters_from_a_prefix_without_publishing_a_partial_picture():
    parameters = b"\x60\x01\x00\x03\x40\x01\x80"
    partial = b"\x62\x01\x81\x80picture prefix"
    assert rust_core.reassemble([parameters, partial], has_donl=False) == []
    assert rust_core.reassemble([parameters, partial], has_donl=False, partial_params=True) == [b"\x40\x01\x80"]


def test_factory_and_malformed_input_lifecycle(vectors, monkeypatch):
    from isharescreen.proxy.protocol.srtp import SRTPDecryptor, SRTCPDecryptor
    monkeypatch.setenv("ISS_NATIVE_PROTOCOL", "1")
    blob = bytes.fromhex(vectors["blob"])
    receiver = SRTPDecryptor.from_blob(blob)
    assert isinstance(receiver, rust_core.Receiver)
    assert isinstance(SRTCPDecryptor.from_blob(blob), rust_core.Control)
    for length in range(128):
        assert receiver.decrypt(bytes(length)) is None
    for _ in range(50):
        other = rust_core.Receiver(blob)
        assert other.decrypt(bytes.fromhex(vectors["rtp"][0]["packet"])) is not None
        del other
