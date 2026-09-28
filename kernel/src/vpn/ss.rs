//! Shadowsocks with AEAD ciphers (the kind Outline and most servers use):
//! a random salt, then length-prefixed chunks, each sealed with a key
//! made from the password and the salt.

use alloc::string::String;
use alloc::vec::Vec;

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use chacha20poly1305::ChaCha20Poly1305;
use md5::{Digest, Md5};

use crate::tg::crypto::random;

const MAX_CHUNK: usize = 0x3fff;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Aes128Gcm,
    Aes256Gcm,
    Chacha20,
}

impl Method {
    pub fn parse(name: &str) -> Option<Method> {
        match name.to_ascii_lowercase().as_str() {
            "aes-128-gcm" => Some(Method::Aes128Gcm),
            "aes-256-gcm" => Some(Method::Aes256Gcm),
            "chacha20-ietf-poly1305" | "chacha20-poly1305" => Some(Method::Chacha20),
            _ => None,
        }
    }

    fn key_len(self) -> usize {
        if self == Method::Aes128Gcm {
            16
        } else {
            32
        }
    }
}

enum Aead {
    A128(Aes128Gcm),
    A256(Aes256Gcm),
    Chacha(ChaCha20Poly1305),
}

/// One direction: its cipher and the nonce counter.
struct Half {
    aead: Aead,
    nonce: [u8; 12],
}

impl Half {
    fn new(method: Method, key: &[u8], salt: &[u8]) -> Half {
        let mut sub = [0u8; 32];
        let sub = &mut sub[..method.key_len()];
        hkdf::Hkdf::<sha1::Sha1>::new(Some(salt), key)
            .expand(b"ss-subkey", sub)
            .unwrap();
        let aead = match method {
            Method::Aes128Gcm => Aead::A128(Aes128Gcm::new_from_slice(sub).unwrap()),
            Method::Aes256Gcm => Aead::A256(Aes256Gcm::new_from_slice(sub).unwrap()),
            Method::Chacha20 => Aead::Chacha(ChaCha20Poly1305::new_from_slice(sub).unwrap()),
        };
        Half { aead, nonce: [0; 12] }
    }

    fn step(&mut self) -> [u8; 12] {
        let n = self.nonce;
        for b in self.nonce.iter_mut() {
            *b = b.wrapping_add(1);
            if *b != 0 {
                break;
            }
        }
        n
    }

    fn seal(&mut self, data: &[u8], out: &mut Vec<u8>) {
        let nonce = self.step();
        let mut buf = data.to_vec();
        let tag = match &self.aead {
            Aead::A128(c) => c.encrypt_in_place_detached((&nonce).into(), &[], &mut buf),
            Aead::A256(c) => c.encrypt_in_place_detached((&nonce).into(), &[], &mut buf),
            Aead::Chacha(c) => c.encrypt_in_place_detached((&nonce).into(), &[], &mut buf),
        }
        .unwrap();
        out.extend_from_slice(&buf);
        out.extend_from_slice(&tag);
    }

    fn open(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let (ct, tag) = data.split_at(data.len() - 16);
        let nonce = self.step();
        let mut buf = ct.to_vec();
        match &self.aead {
            Aead::A128(c) => c.decrypt_in_place_detached((&nonce).into(), &[], &mut buf, tag.into()),
            Aead::A256(c) => c.decrypt_in_place_detached((&nonce).into(), &[], &mut buf, tag.into()),
            Aead::Chacha(c) => c.decrypt_in_place_detached((&nonce).into(), &[], &mut buf, tag.into()),
        }
        .map_err(|_| String::from("Shadowsocks: wrong password or cipher"))?;
        Ok(buf)
    }
}

/// OpenSSL's EVP_BytesToKey with MD5: how Shadowsocks turns a password
/// into a key.
fn password_key(password: &str, len: usize) -> Vec<u8> {
    let mut key = Vec::new();
    let mut last: Vec<u8> = Vec::new();
    while key.len() < len {
        let mut h = Md5::new();
        h.update(&last);
        h.update(password.as_bytes());
        last = h.finalize().to_vec();
        key.extend_from_slice(&last);
    }
    key.truncate(len);
    key
}

pub struct Session {
    method: Method,
    key: Vec<u8>,
    send: Option<Half>,
    recv: Option<Half>,
    /// Received bytes not yet opened.
    rx: Vec<u8>,
    /// The next chunk's payload length, once its header is open.
    want: Option<usize>,
}

impl Session {
    pub fn new(method: Method, password: &str) -> Session {
        Session {
            method,
            key: password_key(password, method.key_len()),
            send: None,
            recv: None,
            rx: Vec::new(),
            want: None,
        }
    }

    /// Encrypt `data` for the server; the first call adds the salt.
    pub fn seal(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 64);
        if self.send.is_none() {
            let mut salt = [0u8; 32];
            let salt = &mut salt[..self.method.key_len()];
            random(salt);
            out.extend_from_slice(salt);
            self.send = Some(Half::new(self.method, &self.key, salt));
        }
        let half = self.send.as_mut().unwrap();
        for chunk in data.chunks(MAX_CHUNK) {
            half.seal(&(chunk.len() as u16).to_be_bytes(), &mut out);
            half.seal(chunk, &mut out);
        }
        out
    }

    /// Take bytes from the server and return what could be opened.
    pub fn open(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        self.rx.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            if self.recv.is_none() {
                let n = self.method.key_len();
                if self.rx.len() < n {
                    return Ok(out);
                }
                let salt: Vec<u8> = self.rx.drain(..n).collect();
                self.recv = Some(Half::new(self.method, &self.key, &salt));
            }
            let half = self.recv.as_mut().unwrap();
            match self.want {
                None => {
                    if self.rx.len() < 18 {
                        return Ok(out);
                    }
                    let head: Vec<u8> = self.rx.drain(..18).collect();
                    let len = half.open(&head)?;
                    self.want = Some((u16::from_be_bytes([len[0], len[1]]) as usize) & MAX_CHUNK);
                }
                Some(n) => {
                    if self.rx.len() < n + 16 {
                        return Ok(out);
                    }
                    let body: Vec<u8> = self.rx.drain(..n + 16).collect();
                    out.extend_from_slice(&half.open(&body)?);
                    self.want = None;
                }
            }
        }
    }
}
