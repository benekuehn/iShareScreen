"""Temporary reference-client adapter for the portable Rust protocol core.

The future standalone client calls its safe Rust API directly. CDLL calls
release the GIL; native handles own cipher state and serialize concurrent use.
"""
import ctypes as C
import functools
import logging
import os
from pathlib import Path
import struct
import sys
import threading

log = logging.getLogger(__name__)


class SourceReport(C.Structure):
    _fields_ = [(name, C.c_uint32) for name in ("ssrc", "roc", "max_seq", "reserved")] + [("packets", C.c_uint64)]


@functools.lru_cache(maxsize=1)
def library():
    suffix = "dylib" if sys.platform == "darwin" else "so"
    path = os.environ.get("ISS_PROTOCOL_LIBRARY") or str(
        Path(__file__).resolve().parents[4] / "protocol" / "target" / "release" / f"libisharescreen_protocol.{suffix}"
    )
    try:
        lib = C.CDLL(path)
    except OSError as exc:
        raise RuntimeError("Rust protocol core is not built; run scripts/build-on-mini.sh or cargo build --release in protocol/") from exc
    lib.iss_protocol_abi_version.restype = C.c_uint32
    if lib.iss_protocol_abi_version() != 1:
        raise RuntimeError("Rust protocol ABI mismatch; rebuild protocol/")
    signatures = {
        "iss_crypto_new": ([C.c_int, C.c_void_p, C.c_size_t, C.c_void_p, C.c_size_t, C.c_uint32], C.c_void_p),
        "iss_crypto_transform": ([C.c_void_p, C.c_int, C.c_void_p, C.c_size_t, C.c_void_p, C.c_size_t, C.POINTER(C.c_uint32)], C.c_ssize_t),
        "iss_crypto_sources": ([C.c_void_p, C.POINTER(SourceReport), C.c_size_t], C.c_size_t),
        "iss_crypto_keep_sources": ([C.c_void_p, C.POINTER(C.c_uint32), C.c_size_t], None),
        "iss_crypto_control_key": ([C.c_void_p, C.c_void_p], C.c_int),
        "iss_crypto_free": ([C.c_void_p], None),
        "iss_hevc_reassemble": ([C.c_void_p, C.c_size_t, C.c_int, C.c_void_p, C.c_size_t], C.c_ssize_t),
        "iss_auth_connect": ([C.c_void_p, C.c_size_t, C.c_uint16, C.c_int, C.c_void_p, C.c_size_t, C.c_void_p, C.c_size_t, C.c_void_p], C.c_int),
        "iss_protocol_error": ([], C.c_char_p),
    }
    for name, (args, result) in signatures.items():
        fn = getattr(lib, name)
        fn.argtypes, fn.restype = args, result
    log.info("Rust protocol core active (ABI 1)")
    return lib


def connect_authenticated(host, port, user, password, mode):
    import socket
    if mode not in ("srp", "nonsrp"):
        raise ValueError("Native auth mode must be srp or nonsrp")
    lib = library()
    host, user, password = host.encode(), user.encode(), password.encode()
    key = C.create_string_buffer(16)
    fd = lib.iss_auth_connect(host, len(host), port, 0 if mode == "srp" else 1,
                             user, len(user), password, len(password), key)
    if fd < 0:
        message = lib.iss_protocol_error()
        raise ConnectionError("Native authentication failed: " + (message.decode(errors="replace") if message else "invalid configuration"))
    try:
        sock = socket.socket(fileno=fd)
    except BaseException:
        os.close(fd)
        raise
    sock.settimeout(15)
    log.info("Native %s authentication succeeded%s", mode.upper(),
             " (server proof verified)" if mode == "srp" else "")
    return sock, key.raw


class Crypto:
    def __init__(self, kind, blob, *, ssrc=0, wrapping_key=b""):
        self.lib = library()
        self.handle = self.lib.iss_crypto_new(kind, blob, len(blob), wrapping_key, len(wrapping_key), ssrc)
        if not self.handle:
            raise ValueError("Invalid native cipher configuration")

    def transform(self, data, operation, metadata=0):
        data = bytes(data)
        out = C.create_string_buffer(len(data) + 64)
        meta = C.c_uint32(metadata)
        result = self.lib.iss_crypto_transform(self.handle, operation, data, len(data), out, len(out), C.byref(meta))
        return (out.raw[:result], meta.value) if result >= 0 else (None, 0)

    def __del__(self):
        if getattr(self, "handle", None):
            self.lib.iss_crypto_free(self.handle)
            self.handle = None


class Receiver(Crypto):
    def __init__(self, blob):
        super().__init__(0, blob)
        self._reference = None
        self._verify_packets = self._verify_accepted = 0
        if os.environ.get("ISS_PROTOCOL_VERIFY") == "1":
            from .srtp import SRTPDecryptor
            self._reference = SRTPDecryptor(blob[:32], blob[32:])

    def decrypt(self, packet):
        data, header_len = self.transform(packet, 0)
        result = None if data is None else (data[:header_len], data[header_len:])
        if self._reference is not None:
            expected = self._reference.decrypt(packet)
            self._verify_packets += 1
            self._verify_accepted += expected is not None
            if result != expected:
                raise RuntimeError(
                    f"Rust/reference SRTP mismatch (length={len(packet)}, "
                    f"flags={packet[0]:#x}, native_accepted={result is not None}, "
                    f"reference_accepted={expected is not None}); payload and keys omitted"
                )
            if self._verify_packets % 1024 == 0:
                log.info("SRTP comparison: packets=%d accepted=%d (no payload/key logging)",
                         self._verify_packets, self._verify_accepted)
        return result

    def _sources(self):
        records = (SourceReport * 64)()
        count = self.lib.iss_crypto_sources(self.handle, records, len(records))
        return list(records[:count])

    @property
    def ssrc_counts(self):
        return {record.ssrc: record.packets for record in self._sources()}

    def state_snapshot(self):
        return {record.ssrc: {"roc": record.roc, "max_seq": record.max_seq} for record in self._sources()}

    def forget_ssrcs_except(self, keep):
        if self._reference is not None:
            self._reference.forget_ssrcs_except(keep)
        values = (C.c_uint32 * len(keep))(*keep)
        self.lib.iss_crypto_keep_sources(self.handle, values, len(values))

    def get_primary_ssrc_group(self, tier=0):
        counts = self.ssrc_counts
        if not counts:
            return set()
        try:
            size = max(1, int(os.environ.get("ISS_TILES_PER_FRAME", "4")))
        except ValueError:
            size = 4
        groups = []
        for source in sorted(counts):
            if groups and source - groups[-1][-1] <= 1 and len(groups[-1]) < size:
                groups[-1].append(source)
            else:
                groups.append([source])
        groups.sort(key=lambda group: -sum(counts[source] for source in group))
        return set(groups[min(tier, len(groups) - 1)])


class Sender(Crypto):
    def __init__(self, blob, ssrc):
        super().__init__(1, blob, ssrc=ssrc)

    def encrypt(self, payload, *, pt=101, marker=False):
        data, _ = self.transform(payload, 1, (pt & 127) | (128 if marker else 0))
        if data is None:
            raise RuntimeError("Native SRTP encryption rejected packet or exhausted key")
        return data


class Control(Crypto):
    def __init__(self, blob):
        super().__init__(2, blob)

    def unprotect(self, packet):
        return self.transform(packet, 0)[0]

    def protect(self, packet):
        data, _ = self.transform(packet, 1)
        if data is None:
            raise RuntimeError("Native SRTCP encryption rejected packet or exhausted key")
        return data


class StreamCipher(Crypto):
    def __init__(self, blob, *, ecb_key):
        super().__init__(3, blob, wrapping_key=ecb_key)
        self._send_lock = threading.Lock()
        key = C.create_string_buffer(16)
        if self.lib.iss_crypto_control_key(self.handle, key) != 0:
            raise RuntimeError("Native control key unavailable")
        self.cbc_key = key.raw

    def encrypt_message(self, plaintext):
        with self._send_lock:
            return self._encrypt(plaintext)

    def _encrypt(self, plaintext):
        data, _ = self.transform(plaintext, 1)
        if data is None:
            raise RuntimeError("Native control encryption rejected record")
        return data

    def encrypt_and_send(self, sock, plaintext):
        # Hold wire ordering through sendall, not just counter assignment.
        with self._send_lock:
            sock.sendall(self._encrypt(plaintext))

    def decrypt_message(self, ciphertext):
        return self.transform(ciphertext, 0)[0]

    def decrypt_stream(self, data):
        messages, pos = [], 0
        while pos + 2 <= len(data):
            size = struct.unpack_from(">H", data, pos)[0]
            if size == 0 or size % 16 or pos + 2 + size > len(data):
                break
            message = self.decrypt_message(data[pos + 2:pos + 2 + size])
            pos += 2 + size
            if message is not None:
                messages.append(message)
        return messages, pos


def reassemble(payloads, *, has_donl=True, partial_params=False):
    payloads = list(payloads)
    packed = b"".join(struct.pack(">H", len(payload)) + payload for payload in payloads)
    if len(packed) > 16 * 1024 * 1024:
        return []
    out = C.create_string_buffer(len(packed) * 2 + 16)
    lib = library()
    flags = int(has_donl) | (2 if partial_params else 0)
    length = lib.iss_hevc_reassemble(packed, len(packed), flags, out, len(out))
    if length < 0:
        log.warning("Rust HEVC reassembly rejected a malformed access unit")
        return []
    data = out.raw[:length]
    nalus, pos = [], 0
    while pos < length:
        size = struct.unpack_from(">I", data, pos)[0]
        pos += 4
        nalus.append(data[pos:pos + size])
        pos += size
    return nalus
