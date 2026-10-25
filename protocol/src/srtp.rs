//! Apple's AES-256-CTR / HMAC-SHA1-80 SRTP and SRTCP profile.
//! Authentication precedes decryption and state mutation. Retransmitted RTP
//! packets remain admissible; access-unit assembly deduplicates their sequence.
use crate::Error;
use aes::Aes256;
use aes::cipher::{BlockEncrypt, KeyInit, KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::collections::{HashMap, HashSet};
use zeroize::{Zeroize, ZeroizeOnDrop};

const TAG: usize = 10;
const SOURCE_CAP: usize = 64;
type Auth = Hmac<Sha1>;

#[derive(Zeroize, ZeroizeOnDrop)]
struct Keys {
    cipher: [u8; 32],
    auth: [u8; 20],
    salt: [u8; 14],
}

fn derive(master: &[u8], salt: &[u8], label: u8, output: &mut [u8]) {
    let aes = Aes256::new_from_slice(master).expect("validated master key");
    let mut iv = [0u8; 16];
    iv[..14].copy_from_slice(salt);
    iv[7] ^= label;
    let base = u128::from_be_bytes(iv);
    for (i, chunk) in output.chunks_mut(16).enumerate() {
        let mut block = (base + i as u128).to_be_bytes().into();
        aes.encrypt_block(&mut block);
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
}

impl Keys {
    fn new(blob: &[u8], rtcp: bool) -> Result<Self, Error> {
        if blob.len() != 46 {
            return Err(Error::InvalidKey);
        }
        let mut keys = Self {
            cipher: [0; 32],
            auth: [0; 20],
            salt: [0; 14],
        };
        let first = if rtcp { 3 } else { 0 };
        derive(&blob[..32], &blob[32..], first, &mut keys.cipher);
        derive(&blob[..32], &blob[32..], first + 1, &mut keys.auth);
        derive(&blob[..32], &blob[32..], first + 2, &mut keys.salt);
        Ok(keys)
    }
    fn iv(&self, ssrc: u32, index: u64) -> [u8; 16] {
        let mut padded = [0u8; 16];
        padded[..14].copy_from_slice(&self.salt);
        (u128::from_be_bytes(padded) ^ ((ssrc as u128) << 64) ^ ((index as u128) << 16))
            .to_be_bytes()
    }
    fn crypt(&self, ssrc: u32, index: u64, data: &mut [u8]) {
        let mut cipher = Ctr128BE::<Aes256>::new(&self.cipher.into(), &self.iv(ssrc, index).into());
        cipher.apply_keystream(data);
    }
    fn mac(&self, data: &[u8], suffix: &[u8]) -> Auth {
        let mut mac = <Auth as Mac>::new_from_slice(&self.auth).expect("fixed HMAC key");
        mac.update(data);
        mac.update(suffix);
        mac
    }
}

#[derive(Default, Clone, Copy)]
struct Source {
    roc: u32,
    seq: u16,
    packets: u64,
}

pub struct Packet {
    pub header: Vec<u8>,
    pub payload: Vec<u8>,
    pub ssrc: u32,
    pub sequence: u16,
    pub timestamp: u32,
    pub marker: bool,
}

pub struct Receiver {
    keys: Keys,
    sources: HashMap<u32, Source>,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SourceReport {
    pub ssrc: u32,
    pub roc: u32,
    pub max_seq: u32,
    pub reserved: u32,
    pub packets: u64,
}
impl Receiver {
    pub fn new(blob: &[u8]) -> Result<Self, Error> {
        Ok(Self {
            keys: Keys::new(blob, false)?,
            sources: HashMap::new(),
        })
    }
    pub fn counts(&self) -> HashMap<u32, u64> {
        self.sources
            .iter()
            .map(|(ssrc, state)| (*ssrc, state.packets))
            .collect()
    }
    pub fn states(&self) -> Vec<SourceReport> {
        self.sources
            .iter()
            .map(|(ssrc, state)| SourceReport {
                ssrc: *ssrc,
                roc: state.roc,
                max_seq: state.seq as u32,
                reserved: 0,
                packets: state.packets,
            })
            .collect()
    }
    pub fn forget_except(&mut self, keep: &HashSet<u32>) {
        self.sources.retain(|ssrc, _| keep.contains(ssrc));
    }
    pub fn unprotect(&mut self, packet: &[u8]) -> Result<Packet, Error> {
        if packet.len() < 12 + TAG || packet.len() > 65535 || packet[0] >> 6 != 2 {
            return Err(Error::InvalidPacket);
        }
        let body = &packet[..packet.len() - TAG];
        let seq = u16::from_be_bytes([body[2], body[3]]);
        let ssrc = u32::from_be_bytes(body[8..12].try_into().unwrap());
        let state = self.sources.get(&ssrc).copied();
        let current = state.map_or(0, |state| state.roc);
        let guess = state.map_or(0, |state| {
            let diff = seq as i32 - state.seq as i32;
            if diff > 0x7fff {
                current.saturating_sub(1)
            } else if diff < -0x7fff {
                current.saturating_add(1)
            } else {
                current
            }
        });
        let candidates = [
            guess,
            current,
            guess.saturating_add(1),
            guess.saturating_sub(1),
        ];
        let mut authenticated = None;
        for (i, roc) in candidates.iter().copied().enumerate() {
            if candidates[..i].contains(&roc) {
                continue;
            }
            if self
                .keys
                .mac(body, &roc.to_be_bytes())
                .verify_truncated_left(&packet[body.len()..])
                .is_ok()
            {
                authenticated = Some(roc);
                break;
            }
        }
        let roc = authenticated.ok_or(Error::Authentication)?;
        let mut header_len = 12 + (body[0] as usize & 15) * 4;
        if header_len > body.len() {
            return Err(Error::InvalidPacket);
        }
        if body[0] & 0x10 != 0 {
            if header_len + 4 > body.len() {
                return Err(Error::InvalidPacket);
            }
            let words = u16::from_be_bytes([body[header_len + 2], body[header_len + 3]]) as usize;
            header_len += 4 + words * 4;
            if header_len > body.len() {
                return Err(Error::InvalidPacket);
            }
        }
        if state.is_none() && self.sources.len() >= SOURCE_CAP {
            return Err(Error::SourceLimit);
        }
        let mut payload = body[header_len..].to_vec();
        self.keys
            .crypt(ssrc, ((roc as u64) << 16) | seq as u64, &mut payload);
        if body[0] & 0x20 != 0 {
            let padding = *payload.last().ok_or(Error::InvalidPacket)? as usize;
            if padding == 0 || padding > payload.len() {
                return Err(Error::InvalidPacket);
            }
            payload.truncate(payload.len() - padding);
        }
        let entry = self.sources.entry(ssrc).or_insert(Source {
            roc,
            seq,
            packets: 0,
        });
        if ((roc as u64) << 16 | seq as u64) > ((entry.roc as u64) << 16 | entry.seq as u64) {
            entry.roc = roc;
            entry.seq = seq;
        }
        entry.packets += 1;
        Ok(Packet {
            header: body[..header_len].to_vec(),
            payload,
            ssrc,
            sequence: seq,
            timestamp: u32::from_be_bytes(body[4..8].try_into().unwrap()),
            marker: body[1] & 128 != 0,
        })
    }
}

pub struct Sender {
    keys: Keys,
    ssrc: u32,
    index: u64,
    timestamp: u32,
}
impl Sender {
    pub fn new(blob: &[u8], ssrc: u32) -> Result<Self, Error> {
        Ok(Self {
            keys: Keys::new(blob, false)?,
            ssrc,
            index: 0,
            timestamp: 0,
        })
    }
    pub fn protect(&mut self, payload: &[u8], pt: u8, marker: bool) -> Result<Vec<u8>, Error> {
        if self.index >= (1u64 << 48) {
            return Err(Error::CounterExhausted);
        }
        if payload.len() > 65535 - 12 - TAG {
            return Err(Error::InvalidPacket);
        }
        let roc = (self.index >> 16) as u32;
        let mut out = vec![0x80, (pt & 127) | if marker { 128 } else { 0 }];
        out.extend_from_slice(&(self.index as u16).to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.ssrc.to_be_bytes());
        out.extend_from_slice(payload);
        self.keys.crypt(self.ssrc, self.index, &mut out[12..]);
        let tag = self
            .keys
            .mac(&out, &roc.to_be_bytes())
            .finalize()
            .into_bytes();
        out.extend_from_slice(&tag[..TAG]);
        self.index += 1;
        self.timestamp = self.timestamp.wrapping_add(480);
        Ok(out)
    }
}

pub struct Control {
    keys: Keys,
    index: u32,
}
impl Control {
    pub fn new(blob: &[u8]) -> Result<Self, Error> {
        Ok(Self {
            keys: Keys::new(blob, true)?,
            index: 0,
        })
    }
    pub fn protect(&mut self, plain: &[u8]) -> Result<Vec<u8>, Error> {
        if plain.len() < 8 || plain.len() > 65535 - 14 {
            return Err(Error::InvalidPacket);
        }
        if self.index >= 0x80000000 {
            return Err(Error::CounterExhausted);
        }
        let ssrc = u32::from_be_bytes(plain[4..8].try_into().unwrap());
        let mut out = plain.to_vec();
        self.keys.crypt(ssrc, self.index as u64, &mut out[8..]);
        out.extend_from_slice(&(self.index | 0x80000000).to_be_bytes());
        let tag = self.keys.mac(&out, &[]).finalize().into_bytes();
        out.extend_from_slice(&tag[..TAG]);
        self.index += 1;
        Ok(out)
    }
    pub fn unprotect(&self, packet: &[u8]) -> Result<Vec<u8>, Error> {
        if packet.len() < 22 || packet.len() > 65535 {
            return Err(Error::InvalidPacket);
        }
        let body = &packet[..packet.len() - TAG];
        self.keys
            .mac(body, &[])
            .verify_truncated_left(&packet[body.len()..])
            .map_err(|_| Error::Authentication)?;
        let end = body.len() - 4;
        let index = u32::from_be_bytes(body[end..].try_into().unwrap());
        let ssrc = u32::from_be_bytes(body[4..8].try_into().unwrap());
        let mut out = body[..end].to_vec();
        if index & 0x80000000 != 0 {
            self.keys
                .crypt(ssrc, (index & 0x7fffffff) as u64, &mut out[8..]);
        }
        Ok(out)
    }
}

/// Derived key fixture access stays test-only; production never serializes keys.
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_tag_never_changes_source_state() {
        let key = [42; 46];
        let mut sender = Sender::new(&key, 7).unwrap();
        let mut receiver = Receiver::new(&key).unwrap();
        let mut packet = sender.protect(b"payload", 96, true).unwrap();
        packet[12] ^= 1;
        assert!(matches!(
            receiver.unprotect(&packet),
            Err(Error::Authentication)
        ));
        assert!(receiver.counts().is_empty());
        packet[12] ^= 1;
        assert_eq!(receiver.unprotect(&packet).unwrap().payload, b"payload");
    }
    #[test]
    fn rollover_and_previous_roc_retransmission() {
        let key = [3; 46];
        let mut sender = Sender::new(&key, 7).unwrap();
        sender.index = 65535;
        let last = sender.protect(b"old", 96, false).unwrap();
        let first = sender.protect(b"new", 96, true).unwrap();
        let mut receiver = Receiver::new(&key).unwrap();
        assert_eq!(receiver.unprotect(&last).unwrap().sequence, 65535);
        assert_eq!(receiver.unprotect(&first).unwrap().sequence, 0);
        assert_eq!(receiver.unprotect(&last).unwrap().payload, b"old");
        assert_eq!(receiver.sources[&7].roc, 1);
    }
    #[test]
    fn malformed_inputs_are_rejected_without_panicking() {
        let mut receiver = Receiver::new(&[0; 46]).unwrap();
        for len in 0..128 {
            assert!(receiver.unprotect(&vec![0; len]).is_err());
        }
    }
    #[test]
    fn counter_exhaustion_does_not_repeat_keystream() {
        let mut sender = Sender::new(&[0; 46], 7).unwrap();
        sender.index = 1 << 48;
        assert_eq!(
            sender.protect(b"x", 96, false),
            Err(Error::CounterExhausted)
        );
        let mut control = Control::new(&[0; 46]).unwrap();
        control.index = 0x80000000;
        assert_eq!(control.protect(&[0; 8]), Err(Error::CounterExhausted));
    }
}
