//! Apple's enc1103 record framing and continuously chained AES-128-CBC.
use crate::Error;
use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use sha1::{Digest, Sha1};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub struct Cipher {
    aes: Aes128,
    key: Zeroizing<[u8; 16]>,
    send_iv: Zeroizing<[u8; 16]>,
    receive_iv: Zeroizing<[u8; 16]>,
    send_counter: u64,
    receive_counter: u64,
}
impl Cipher {
    pub fn new(blob: &[u8], auth_key: &[u8]) -> Result<Self, Error> {
        if blob.len() != 36 || auth_key.len() != 16 {
            return Err(Error::InvalidKey);
        }
        let wrapping = Aes128::new_from_slice(auth_key).map_err(|_| Error::InvalidKey)?;
        let mut key = Zeroizing::new([0u8; 16]);
        key.copy_from_slice(&blob[4..20]);
        wrapping.decrypt_block((&mut *key).into());
        let mut iv = Zeroizing::new([0u8; 16]);
        iv.copy_from_slice(&blob[20..36]);
        wrapping.decrypt_block((&mut *iv).into());
        Ok(Self {
            aes: Aes128::new((&*key).into()),
            key,
            send_iv: iv.clone(),
            receive_iv: iv,
            send_counter: 0,
            receive_counter: 0,
        })
    }
    pub fn cbc_key(&self) -> &[u8; 16] {
        &self.key
    }
    pub fn encrypt(&mut self, message: &[u8]) -> Result<Vec<u8>, Error> {
        let size = (2 + message.len() + 20).div_ceil(16) * 16;
        if size > u16::MAX as usize {
            return Err(Error::InvalidPacket);
        }
        if self.send_counter > u32::MAX as u64 {
            return Err(Error::CounterExhausted);
        }
        let mut block = Zeroizing::new(vec![0u8; size]);
        block[..2].copy_from_slice(&(message.len() as u16).to_be_bytes());
        block[2..2 + message.len()].copy_from_slice(message);
        let mut mac = Sha1::new();
        mac.update((self.send_counter as u32).to_be_bytes());
        mac.update(&block[..size - 20]);
        block[size - 20..].copy_from_slice(&mac.finalize());
        for chunk in block.chunks_exact_mut(16) {
            for (byte, iv) in chunk.iter_mut().zip(self.send_iv.iter()) {
                *byte ^= iv;
            }
            self.aes.encrypt_block(chunk.into());
            self.send_iv.copy_from_slice(chunk);
        }
        self.send_counter += 1;
        let mut out = Vec::with_capacity(size + 2);
        out.extend_from_slice(&(size as u16).to_be_bytes());
        out.extend_from_slice(&block);
        Ok(out)
    }
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
        if ciphertext.len() < 32 || ciphertext.len() > 65535 || ciphertext.len() % 16 != 0 {
            return Err(Error::InvalidPacket);
        }
        if self.receive_counter > u32::MAX as u64 {
            return Err(Error::CounterExhausted);
        }
        let mut plain = Zeroizing::new(ciphertext.to_vec());
        for chunk in plain.chunks_exact_mut(16) {
            let next: [u8; 16] = chunk.try_into().unwrap();
            self.aes.decrypt_block(chunk.into());
            for (byte, iv) in chunk.iter_mut().zip(self.receive_iv.iter()) {
                *byte ^= iv;
            }
            self.receive_iv.copy_from_slice(&next);
        }
        let start = self.receive_counter;
        self.receive_counter += 1;
        let end = plain.len() - 20;
        for counter in start.saturating_sub(1)..(start + 6).min(u32::MAX as u64 + 1) {
            let mut mac = Sha1::new();
            mac.update((counter as u32).to_be_bytes());
            mac.update(&plain[..end]);
            if bool::from(mac.finalize().as_slice().ct_eq(&plain[end..])) {
                self.receive_counter = counter + 1;
                let size = u16::from_be_bytes([plain[0], plain[1]]) as usize;
                if size > end - 2 {
                    return Err(Error::InvalidPacket);
                }
                return Ok(plain[2..2 + size].to_vec());
            }
        }
        Err(Error::Authentication)
    }
}
