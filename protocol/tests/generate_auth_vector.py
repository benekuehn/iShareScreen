"""Synthetic SRP interoperability vector. No real server/session/password data."""
import ctypes as C
import hashlib
import json
from pathlib import Path
from unittest.mock import patch

from isharescreen.proxy.protocol.auth import _SrpChallenge, _derive_x, _solve_srp

crypto = C.CDLL('libcrypto.so.3')
crypto.BN_get_rfc3526_prime_4096.argtypes = [C.c_void_p]
crypto.BN_get_rfc3526_prime_4096.restype = C.c_void_p
crypto.BN_bn2bin.argtypes = [C.c_void_p, C.c_void_p]
crypto.BN_bn2bin.restype = C.c_int
crypto.BN_free.argtypes = [C.c_void_p]
prime = crypto.BN_get_rfc3526_prime_4096(None)
assert prime
try:
    encoded = C.create_string_buffer(512)
    assert crypto.BN_bn2bin(prime, encoded) == 512
    modulus = encoded.raw
finally:
    crypto.BN_free(prime)

password = 'synthetic password, never a credential'
salt = bytes(range(32))
iterations = 10
n = int.from_bytes(modulus, 'big')
g = 5
k = int.from_bytes(hashlib.sha512(modulus + g.to_bytes(512, 'big')).digest(), 'big')
x = _derive_x(salt, iterations, password)
b = int.from_bytes(bytes(range(64, 128)), 'big')
public = (k * pow(g, x, n) + pow(g, b, n)) % n
public_bytes = public.to_bytes(512, 'big')
cap = b'mda=SHA-512,kdf=SALTED-SHA512-PBKDF2'
challenge = _SrpChallenge(N=n, Nb=modulus, g=g, salt=salt, B=public, Bb=public_bytes, iterations=iterations, cap=cap)
entropy = bytes(range(64))
with patch('isharescreen.proxy.protocol.auth.os.urandom', return_value=entropy):
    proof = _solve_srp(challenge, password)
encoded_challenge = b'\0'*13 + modulus + b'\0\1\5' + bytes([len(salt)]) + salt
encoded_challenge += len(public_bytes).to_bytes(2, 'big') + public_bytes + iterations.to_bytes(8, 'big')
encoded_challenge += len(cap).to_bytes(2, 'big') + cap
Path(__file__).with_name('auth-vector.json').write_text(json.dumps({
    'synthetic_only': True, 'password': password, 'challenge': encoded_challenge.hex(),
    'entropy': entropy.hex(), 'public': proof.Ab.hex(), 'm1': proof.M1.hex(), 'key': proof.K.hex(),
    'm2': hashlib.sha512(proof.Ab + proof.M1 + proof.K).hexdigest(),
}, indent=2) + '\n')
print('Generated synthetic SRP fixture; no real credential/session data used')
