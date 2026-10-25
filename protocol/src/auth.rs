//! Native RSA1 and Apple SRP authentication. No password or key logging.
use openssl::bn::{BigNum, BigNumContext, BigNumRef};
use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Public};
use openssl::rsa::{Padding, Rsa};
use openssl::sha::{sha256, sha512};
use openssl::symm::{Cipher, Crypter, Mode};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::net::ToSocketAddrs;
use std::ops::{Deref, DerefMut};
use std::time::Duration;
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const WIDTH: usize = 512;

pub fn open(host: &str, port: u16) -> Result<TcpStream> {
    let address = (host, port)
        .to_socket_addrs()?
        .find(|addr| addr.is_ipv4())
        .ok_or("HP transport requires IPv4")?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    stream.set_nodelay(true)?;
    let mut banner = [0u8; 12];
    stream.read_exact(&mut banner)?;
    if &banner[..4] != b"RFB " {
        return Err("server did not send an RFB banner".into());
    }
    stream.write_all(b"RFB 003.889\n")?;
    let mut count = [0u8; 1];
    stream.read_exact(&mut count)?;
    let mut types = vec![0; count[0] as usize];
    stream.read_exact(&mut types)?;
    if !types.contains(&0x21) {
        return Err("server does not offer RSA1/SRP authentication".into());
    }
    Ok(stream)
}

pub fn connect(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    use_srp: bool,
) -> Result<(TcpStream, Zeroizing<[u8; 16]>)> {
    let mut stream = open(host, port)?;
    let key = if use_srp {
        srp(&mut stream, user, password)?
    } else {
        nonsrp(&mut stream, user, password)?
    };
    Ok((stream, key))
}

struct Secret(BigNum);
impl Secret {
    fn new() -> Result<Self> {
        let mut value = BigNum::new_secure()?;
        value.set_const_time();
        Ok(Self(value))
    }
    fn from_slice(bytes: &[u8]) -> Result<Self> {
        let mut value = Self::new()?;
        value.0.copy_from_slice(bytes)?;
        Ok(value)
    }
}
impl Deref for Secret {
    type Target = BigNumRef;
    fn deref(&self) -> &BigNumRef {
        &self.0
    }
}
impl DerefMut for Secret {
    fn deref_mut(&mut self) -> &mut BigNumRef {
        &mut self.0
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.clear();
    }
}

fn read_len(stream: &mut TcpStream, maximum: usize) -> Result<Vec<u8>> {
    let mut size = [0u8; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > maximum {
        return Err("authentication record exceeds size limit".into());
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn send_len(stream: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}
fn rsa_init(stream: &mut TcpStream) -> Result<Rsa<Public>> {
    stream.write_all(b"\x21\x00\x00\x00\x0a\x01\x00RSA1\x00\x00\x00\x00")?;
    let packet = read_len(stream, 8192)?;
    if packet.len() < 6 {
        return Err("truncated RSA1 response".into());
    }
    let size = u32::from_be_bytes(packet[2..6].try_into()?) as usize;
    if size == 0 || size + 6 > packet.len() {
        return Err("invalid RSA1 key length".into());
    }
    let public = PKey::public_key_from_der(&packet[6..6 + size])?.rsa()?;
    if public.size() != 256 {
        return Err("expected a 2048-bit RSA1 server key".into());
    }
    Ok(public)
}
fn rsa_encrypt(public: &Rsa<Public>, plain: &[u8]) -> Result<Vec<u8>> {
    let mut encrypted = vec![0; public.size() as usize];
    let size = public.public_encrypt(plain, &mut encrypted, Padding::PKCS1)?;
    encrypted.truncate(size);
    Ok(encrypted)
}

pub fn nonsrp(stream: &mut TcpStream, user: &str, password: &str) -> Result<Zeroizing<[u8; 16]>> {
    if user.len() > 63 || password.len() > 63 {
        return Err("legacy RSA1 credentials exceed 63 bytes".into());
    }
    let public = rsa_init(stream)?;
    let mut key = Zeroizing::new([0u8; 16]);
    openssl::rand::rand_bytes(&mut *key)?;
    let mut slots = Zeroizing::new([0u8; 128]);
    openssl::rand::rand_bytes(&mut *slots)?;
    for (offset, value) in [(0, user.as_bytes()), (64, password.as_bytes())] {
        slots[offset..offset + value.len()].copy_from_slice(value);
        slots[offset + value.len()] = 0;
    }
    let mut aes = Crypter::new(Cipher::aes_128_ecb(), Mode::Encrypt, &*key, None)?;
    aes.pad(false);
    let mut encrypted = vec![0; 144];
    let mut used = aes.update(&*slots, &mut encrypted)?;
    used += aes.finalize(&mut encrypted[used..])?;
    encrypted.truncate(used);
    let mut message = b"\x01\x00RSA1\x00\x01".to_vec();
    message.extend(encrypted);
    message.extend_from_slice(b"\x00\x01");
    message.extend(rsa_encrypt(&public, &*key)?);
    send_len(stream, &message)?;
    let mut result = [0u8; 8];
    stream.read_exact(&mut result)?;
    if result[4..] != [0, 0, 0, 0] {
        return Err("RSA1 authentication rejected".into());
    }
    Ok(key)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or("authentication length overflow")?;
        let value = self
            .bytes
            .get(self.pos..end)
            .ok_or("truncated SRP challenge")?;
        self.pos = end;
        Ok(value)
    }
    fn u16(&mut self) -> Result<usize> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?) as usize)
    }
}
struct Challenge {
    modulus: Vec<u8>,
    generator: Vec<u8>,
    salt: Vec<u8>,
    public: Vec<u8>,
    iterations: usize,
    cap: Vec<u8>,
}
impl Challenge {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let mut reader = Reader { bytes, pos: 12 };
        if reader.take(1)? != [0] {
            return Err("invalid SRP modulus marker".into());
        }
        let modulus = reader.take(WIDTH)?.to_vec();
        let size = reader.u16()?;
        if size > WIDTH {
            return Err("oversized SRP generator".into());
        }
        let generator = reader.take(size)?.to_vec();
        let size = reader.take(1)?[0] as usize;
        let salt = reader.take(size)?.to_vec();
        let size = reader.u16()?;
        if size > WIDTH + 1 {
            return Err("oversized SRP public value".into());
        }
        let public = reader.take(size)?.to_vec();
        let iterations = u64::from_be_bytes(reader.take(8)?.try_into()?);
        if iterations == 0 || iterations > 1_000_000 {
            return Err("SRP iteration count outside 1..1000000".into());
        }
        let size = reader.u16()?;
        if size > 1024 {
            return Err("oversized SRP capability".into());
        }
        let cap = reader.take(size)?.to_vec();
        if salt.is_empty() || public.is_empty() {
            return Err("empty SRP challenge field".into());
        }
        if modulus != BigNum::get_rfc3526_prime_4096()?.to_vec() || generator != [5] {
            return Err("server requested an unsupported SRP group".into());
        }
        Ok(Self {
            modulus,
            generator,
            salt,
            public,
            iterations: iterations as usize,
            cap,
        })
    }
}
struct Proof {
    public: Vec<u8>,
    m1: Zeroizing<[u8; 64]>,
    key: Zeroizing<[u8; 64]>,
    server_proof: Zeroizing<[u8; 64]>,
}
fn digest(parts: &[&[u8]]) -> [u8; 64] {
    let mut input = Zeroizing::new(Vec::new());
    for part in parts {
        input.extend_from_slice(part);
    }
    sha512(&input)
}
fn solve(challenge: &Challenge, password: &str, entropy: &[u8; 64]) -> Result<Proof> {
    let mut context = BigNumContext::new_secure()?;
    let n = BigNum::from_slice(&challenge.modulus)?;
    let g = BigNum::from_slice(&challenge.generator)?;
    let b = BigNum::from_slice(&challenge.public)?;
    let mut check = BigNum::new()?;
    check.nnmod(&b, &n, &mut context)?;
    if check.num_bits() == 0 {
        return Err("SRP public value is zero modulo N".into());
    }
    let padded_g = g.to_vec_padded(WIDTH as i32)?;
    let k = BigNum::from_slice(&digest(&[&challenge.modulus, &padded_g]))?;
    // 512-bit entropy is always smaller than this pinned 4096-bit modulus.
    let mut a = Secret::from_slice(entropy)?;
    a.add_word(1)?;
    let mut public = BigNum::new()?;
    public.mod_exp(&g, &a, &n, &mut context)?;
    let public = public.to_vec_padded(WIDTH as i32)?;
    let u = BigNum::from_slice(&digest(&[&public, &challenge.public]))?;
    if u.num_bits() == 0 {
        return Err("invalid zero SRP scrambling parameter".into());
    }
    let mut stretched = Zeroizing::new([0u8; 128]);
    openssl::pkcs5::pbkdf2_hmac(
        password.as_bytes(),
        &challenge.salt,
        challenge.iterations,
        MessageDigest::sha512(),
        &mut *stretched,
    )?;
    let inner = Zeroizing::new(digest(&[b":", &*stretched]));
    let x_hash = Zeroizing::new(digest(&[&challenge.salt, &*inner]));
    let x_raw = Secret::from_slice(&*x_hash)?;
    let mut x = Secret::new()?;
    x.nnmod(&x_raw, &n, &mut context)?;
    let mut gx = Secret::new()?;
    gx.mod_exp(&g, &x, &n, &mut context)?;
    let mut product = Secret::new()?;
    product.mod_mul(&k, &gx, &n, &mut context)?;
    let mut base = Secret::new()?;
    base.mod_sub(&b, &product, &n, &mut context)?;
    if base.num_bits() == 0 {
        return Err("invalid zero SRP premaster base".into());
    }
    let mut ux = Secret::new()?;
    ux.checked_mul(&u, &x, &mut context)?;
    let mut exponent = Secret::new()?;
    exponent.checked_add(&a, &ux)?;
    let mut secret = Secret::new()?;
    secret.mod_exp(&base, &exponent, &n, &mut context)?;
    let padded_secret = Zeroizing::new(secret.to_vec_padded(WIDTH as i32)?);
    let key = Zeroizing::new(sha512(&padded_secret));
    let mut xor = sha512(&challenge.modulus);
    for (value, other) in xor.iter_mut().zip(sha512(&padded_g)) {
        *value ^= other;
    }
    let m1 = Zeroizing::new(digest(&[
        &xor,
        &sha512(b""),
        &challenge.salt,
        &public,
        &challenge.public,
        &*key,
    ]));
    let server_proof = Zeroizing::new(digest(&[&public, &*m1, &*key]));
    Ok(Proof {
        public,
        m1,
        key,
        server_proof,
    })
}

pub fn srp(stream: &mut TcpStream, user: &str, password: &str) -> Result<Zeroizing<[u8; 16]>> {
    let public_key = rsa_init(stream)?;
    if user.len() > 200 {
        return Err("SRP username is too long".into());
    }
    let mut identity = (user.len() as u32).to_be_bytes().to_vec();
    identity.extend_from_slice(user.as_bytes());
    identity.extend_from_slice(&[0; 3]);
    let mut identity_slot = (identity.len() as u32).to_be_bytes().to_vec();
    identity_slot.extend(identity);
    let encrypted = rsa_encrypt(&public_key, &identity_slot)?;
    let mut request = b"\x01\x00RSA1\x00\x02".to_vec();
    request.extend_from_slice(&256u16.to_be_bytes());
    request.extend(encrypted);
    request.resize(650, 0);
    send_len(stream, &request)?;
    let bytes = read_len(stream, 8192)?;
    let challenge = Challenge::parse(&bytes)?;
    let mut entropy = Zeroizing::new([0u8; 64]);
    openssl::rand::rand_bytes(&mut *entropy)?;
    let proof = solve(&challenge, password, &entropy)?;
    let mut data = (WIDTH as u16).to_be_bytes().to_vec();
    data.extend_from_slice(&proof.public);
    data.push(64);
    data.extend_from_slice(&*proof.m1);
    data.extend_from_slice(&(challenge.cap.len() as u16).to_be_bytes());
    data.extend_from_slice(&challenge.cap);
    data.push(16);
    let mut iv = [0u8; 16];
    openssl::rand::rand_bytes(&mut iv)?;
    data.extend(iv);
    let mut request = b"\x01\x00RSA1\x00\x02".to_vec();
    request.extend_from_slice(&((data.len() + 4) as u16).to_be_bytes());
    request.extend_from_slice(&(data.len() as u32).to_be_bytes());
    request.extend(data);
    if request.len() > 1076 {
        return Err("SRP proof exceeds envelope limit".into());
    }
    request.resize(1076, 0);
    send_len(stream, &request)?;
    let server_proof = Zeroizing::new(read_len(stream, 8192)?);
    let mut result = [0u8; 4];
    stream.read_exact(&mut result)?;
    if result != [0; 4] {
        return Err("SRP authentication rejected".into());
    }
    // Apple's SASL proof may have an envelope. Accept only a matching M2
    // digest, regardless of its position; no envelope data is trusted/used.
    if !server_proof
        .windows(64)
        .any(|part| openssl::memcmp::eq(part, &*proof.server_proof))
    {
        return Err(format!(
            "SRP server proof did not verify (record length={})",
            server_proof.len()
        )
        .into());
    }
    let hash = Zeroizing::new(sha256(&*proof.key));
    let mut key = Zeroizing::new([0u8; 16]);
    key.copy_from_slice(&hash[..16]);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unhex(value: &serde_json::Value) -> Vec<u8> {
        let value = value.as_str().unwrap();
        (0..value.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
            .collect()
    }
    #[test]
    fn srp_proof_matches_independent_python_reference() {
        let data: serde_json::Value =
            serde_json::from_str(include_str!("../tests/auth-vector.json")).unwrap();
        assert_eq!(data["synthetic_only"], true);
        let challenge = Challenge::parse(&unhex(&data["challenge"])).unwrap();
        let entropy: [u8; 64] = unhex(&data["entropy"]).try_into().unwrap();
        let proof = solve(&challenge, data["password"].as_str().unwrap(), &entropy).unwrap();
        assert_eq!(proof.public, unhex(&data["public"]));
        assert_eq!(&*proof.m1, unhex(&data["m1"]).as_slice());
        assert_eq!(&*proof.key, unhex(&data["key"]).as_slice());
        assert_eq!(&*proof.server_proof, unhex(&data["m2"]).as_slice());
    }
    #[test]
    fn zero_modulus_public_value_is_rejected_before_password_stretching() {
        let data: serde_json::Value =
            serde_json::from_str(include_str!("../tests/auth-vector.json")).unwrap();
        let mut challenge = Challenge::parse(&unhex(&data["challenge"])).unwrap();
        challenge.public = challenge.modulus.clone();
        assert!(solve(&challenge, "synthetic", &[0; 64]).is_err());
    }
    #[test]
    fn malformed_challenges_never_panic_or_request_unbounded_work() {
        for len in 0..1200 {
            assert!(Challenge::parse(&vec![0; len]).is_err());
        }
    }
}
