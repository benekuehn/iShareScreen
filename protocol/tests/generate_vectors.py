"""Regenerate public, synthetic interoperability fixtures using the Python reference.

No recording, desktop content, real credentials or negotiated keys are used.
Run from the checkout with its development venv; never needed at runtime.
"""
import hashlib
import hmac
import json
import struct
from pathlib import Path

from Crypto.Cipher import AES
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from isharescreen.proxy.protocol.srtp import SRTPEncryptor, SRTPDecryptor, SRTCPEncryptor
from isharescreen.proxy.protocol.enc1103 import StreamCipher

blob = bytes(range(46))
sender = SRTPEncryptor.from_blob(blob, 0x12345678)
sender._seq = 65534
rtp = []
for payload, marker in ((b'first synthetic payload', False), (bytes(range(256)), True), (b'', False), (b'after wrap', True)):
    packet = sender.encrypt(payload, pt=96, marker=marker)
    rtp.append({'packet': packet.hex(), 'header': packet[:12].hex(), 'payload': payload.hex()})
# Late retransmit across ROC wrap and an independent SSRC.
rtp.insert(3, dict(rtp[1]))
other = SRTPEncryptor.from_blob(blob, 0x76543210)
packet = other.encrypt(b'other source', marker=True)
rtp.append({'packet': packet.hex(), 'header': packet[:12].hex(), 'payload': b'other source'.hex()})
# Authenticated RTP CSRC list and extension, using the same reference keys.
header = struct.pack('>BBHII', 0x92, 0xe0, 1, 90000, 42)
header += struct.pack('>IIII', 17, 19, 0xbede0002, 0x01020304) + bytes.fromhex('05060708')
payload = b'extension packet'
index = 1
iv = int.from_bytes(other._salt + b'\0\0', 'big') ^ (42 << 64) ^ (index << 16)
encrypted = Cipher(algorithms.AES(other._cipher_key), modes.CTR(iv.to_bytes(16, 'big'))).encryptor().update(payload)
body = header + encrypted
packet = body + hmac.new(other._auth_key, body + b'\0'*4, 'sha1').digest()[:10]
assert SRTPDecryptor.from_blob(blob).decrypt(packet) == (header, payload)
rtp.append({'packet': packet.hex(), 'header': header.hex(), 'payload': payload.hex()})

control = SRTCPEncryptor.from_blob(blob)
rtcp = []
for index in (0, 1, 0x7fffffff):
    plain = struct.pack('>BBHII', 0x81, 206, 2, 0x12345678, 42)
    control._index = index
    packet = control.protect(plain)
    rtcp.append({'packet': packet.hex(), 'plain': plain.hex()})
plain = bytes.fromhex('80c9000112345678')
body = plain + struct.pack('>I', 7)  # authenticated but unencrypted SRTCP
rtcp.append({'packet': (body + hmac.new(control._auth_key, body, 'sha1').digest()[:10]).hex(), 'plain': plain.hex()})

auth_key = bytes(range(16))
key_blob = bytes(range(36))
cipher = StreamCipher(key_blob, ecb_key=auth_key)
records = []
for i, message in enumerate((b'', b'synthetic control message', bytes(range(256))*4, b'after counter gap')):
    if i == 3:
        cipher._enc_ctr += 2
    records.append({'packet': cipher.encrypt_message(message).hex(), 'plain': message.hex()})
# Correct CBC chaining with a deliberately invalid digest, then a valid record.
message = b'bad digest test'
pad = (-(2 + len(message) + 20)) % 16
framed = struct.pack('>H', len(message)) + message + b'\0' * pad
ciphertext = cipher._enc.encrypt(framed + b'\0' * 20)
cipher._enc_ctr += 1
records.append({'packet': (struct.pack('>H', len(ciphertext)) + ciphertext).hex(), 'plain': None})
records.append({'packet': cipher.encrypt_message(b'valid after rejected digest').hex(), 'plain': b'valid after rejected digest'.hex()})
check = StreamCipher(key_blob, ecb_key=auth_key)
for record in records:
    result = check.decrypt_message(bytes.fromhex(record['packet'])[2:])
    assert (result.hex() if result is not None else None) == record['plain']

Path(__file__).with_name('vectors.json').write_text(json.dumps({
    'synthetic_only': True, 'blob': blob.hex(), 'rtp': rtp, 'rtcp': rtcp,
    'auth_key': auth_key.hex(), 'control_blob': key_blob.hex(), 'records': records,
}, indent=2) + '\n')
print('Generated synthetic fixtures; no session data used')
