//! RyzikOS's own TLS client, for the browser (https) and the VPN.
//!
//! TLS 1.3, and TLS 1.2 with ECDHE for the many sites that have not moved
//! on, with X25519 or P-256 keys and AES-GCM or ChaCha20. Handshake
//! messages may span records, so sites with long certificate chains work.
//!
//! For the VPN it does REALITY: a key hidden in the ClientHello's session
//! id, and the server proven by an HMAC in its certificate. XTLS Vision
//! may also drop the TLS layer halfway through a connection.
//!
//! It does not check certificates (RyzikOS has no list of authorities;
//! the VPN protocols carry their own secrets), and verifies the server's
//! Finished message.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use chacha20poly1305::ChaCha20Poly1305;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha384, Sha512};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::net::{Socket, TcpStream};
use crate::tg::crypto::random;

/// What TLS runs over: a plain socket (the VPN's own connection to its
/// server) or a stream that may itself go through the VPN (the browser).
pub trait Transport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    /// None if nothing has arrived yet.
    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, String>;
    fn write_all(&mut self, data: &[u8]) -> Result<(), String>;
}

impl Transport for Socket {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        Socket::read(self, buf)
    }
    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, String> {
        Socket::try_read(self, buf)
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        Socket::write_all(self, data)
    }
}

impl Transport for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        TcpStream::read(self, buf)
    }
    fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, String> {
        TcpStream::try_read(self, buf)
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        TcpStream::write_all(self, data)
    }
}

/// The server closed the connection (a clean end of a reply).
const CLOSED: &str = "the server closed the connection";

const HANDSHAKE: u8 = 22;
const APP_DATA: u8 = 23;
const ALERT: u8 = 21;
const CHANGE_CIPHER: u8 = 20;
/// The biggest record payload, plus room for the tag, padding and (TLS
/// 1.2) the explicit nonce.
const MAX_RECORD: usize = 16384 + 2048;

/// REALITY's settings from the link: the server's X25519 public key
/// (pbk) and the short id (sid).
pub struct Reality {
    pub public_key: [u8; 32],
    pub short_id: [u8; 8],
}

pub struct Options<'a> {
    pub sni: &'a str,
    pub alpn: &'a [String],
    pub reality: Option<&'a Reality>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Suite {
    Aes128,
    Aes256,
    Chacha,
}

impl Suite {
    fn from_id(id: u16) -> Option<Suite> {
        match id {
            0x1301 => Some(Suite::Aes128),
            0x1302 => Some(Suite::Aes256),
            0x1303 => Some(Suite::Chacha),
            _ => None,
        }
    }

    /// TLS 1.2 suites with ECDHE (RSA or ECDSA certificates): the record
    /// protection is the same, and the hash goes with it as in 1.3.
    fn from_id12(id: u16) -> Option<Suite> {
        match id {
            0xc02b | 0xc02f => Some(Suite::Aes128),
            0xc02c | 0xc030 => Some(Suite::Aes256),
            0xcca9 | 0xcca8 => Some(Suite::Chacha),
            _ => None,
        }
    }

    /// TLS 1.2's PRF (P_hash with this suite's hash).
    fn prf(self, secret: &[u8], label: &str, seed: &[&[u8]], len: usize) -> Vec<u8> {
        let mut ls = Vec::from(label.as_bytes());
        for s in seed {
            ls.extend_from_slice(s);
        }
        let mut a = self.hmac(secret, &[&ls]);
        let mut out = Vec::with_capacity(len + 48);
        while out.len() < len {
            out.extend_from_slice(&self.hmac(secret, &[&a, &ls]));
            a = self.hmac(secret, &[&a]);
        }
        out.truncate(len);
        out
    }

    fn key_len(self) -> usize {
        if self == Suite::Aes128 {
            16
        } else {
            32
        }
    }

    fn hash_len(self) -> usize {
        if self == Suite::Aes256 {
            48
        } else {
            32
        }
    }

    fn hash(self, data: &[u8]) -> Vec<u8> {
        if self == Suite::Aes256 {
            Sha384::digest(data).to_vec()
        } else {
            Sha256::digest(data).to_vec()
        }
    }

    fn hmac(self, key: &[u8], data: &[&[u8]]) -> Vec<u8> {
        if self == Suite::Aes256 {
            let mut m = <Hmac<Sha384> as Mac>::new_from_slice(key).unwrap();
            for d in data {
                m.update(d);
            }
            m.finalize().into_bytes().to_vec()
        } else {
            let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
            for d in data {
                m.update(d);
            }
            m.finalize().into_bytes().to_vec()
        }
    }

    fn extract(self, salt: &[u8], ikm: &[u8]) -> Vec<u8> {
        self.hmac(salt, &[ikm])
    }

    fn expand_label(self, secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
        let mut info = Vec::new();
        info.extend_from_slice(&(len as u16).to_be_bytes());
        info.push((6 + label.len()) as u8);
        info.extend_from_slice(b"tls13 ");
        info.extend_from_slice(label.as_bytes());
        info.push(context.len() as u8);
        info.extend_from_slice(context);
        let mut out = Vec::new();
        let mut t: Vec<u8> = Vec::new();
        let mut i = 1u8;
        while out.len() < len {
            t = self.hmac(secret, &[&t, &info, &[i]]);
            out.extend_from_slice(&t);
            i += 1;
        }
        out.truncate(len);
        out
    }
}

enum Cipher {
    Aes128(Aes128Gcm),
    Aes256(Aes256Gcm),
    Chacha(ChaCha20Poly1305),
}

/// One direction's record protection.
struct Keys {
    cipher: Cipher,
    iv: [u8; 12],
    seq: u64,
    /// TLS 1.2 records: the real type in the header, the sequence number
    /// in the additional data and, for AES-GCM, an explicit nonce.
    v12: bool,
}

impl Keys {
    fn new(suite: Suite, secret: &[u8]) -> Keys {
        let key = suite.expand_label(secret, "key", &[], suite.key_len());
        let iv_v = suite.expand_label(secret, "iv", &[], 12);
        let mut iv = [0u8; 12];
        iv.copy_from_slice(&iv_v);
        let cipher = match suite {
            Suite::Aes128 => Cipher::Aes128(Aes128Gcm::new_from_slice(&key).unwrap()),
            Suite::Aes256 => Cipher::Aes256(Aes256Gcm::new_from_slice(&key).unwrap()),
            Suite::Chacha => Cipher::Chacha(ChaCha20Poly1305::new_from_slice(&key).unwrap()),
        };
        Keys {
            cipher,
            iv,
            seq: 0,
            v12: false,
        }
    }

    /// TLS 1.2 keys from the key block: AES-GCM takes a 4-byte salt (the
    /// rest of its nonce travels in each record), ChaCha20 a 12-byte iv.
    fn new12(suite: Suite, key: &[u8], iv: &[u8]) -> Keys {
        let mut n = [0u8; 12];
        n[..iv.len()].copy_from_slice(iv);
        let cipher = match suite {
            Suite::Aes128 => Cipher::Aes128(Aes128Gcm::new_from_slice(key).unwrap()),
            Suite::Aes256 => Cipher::Aes256(Aes256Gcm::new_from_slice(key).unwrap()),
            Suite::Chacha => Cipher::Chacha(ChaCha20Poly1305::new_from_slice(key).unwrap()),
        };
        Keys {
            cipher,
            iv: n,
            seq: 0,
            v12: true,
        }
    }

    fn explicit_nonce(&self) -> bool {
        self.v12 && !matches!(self.cipher, Cipher::Chacha(_))
    }

    fn seal12(&mut self, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut aad = self.seq.to_be_bytes().to_vec();
        aad.extend_from_slice(&[kind, 3, 3, (data.len() >> 8) as u8, data.len() as u8]);
        let explicit = self.seq.to_be_bytes();
        // AES-GCM: salt and sequence number; ChaCha20: iv xor sequence
        let nonce = self.nonce();
        let mut body = data.to_vec();
        let tag = match &self.cipher {
            Cipher::Aes128(c) => c.encrypt_in_place_detached((&nonce).into(), &aad, &mut body),
            Cipher::Aes256(c) => c.encrypt_in_place_detached((&nonce).into(), &aad, &mut body),
            Cipher::Chacha(c) => c.encrypt_in_place_detached((&nonce).into(), &aad, &mut body),
        }
        .map(|t| t.to_vec())
        .unwrap_or_default();
        let extra = if self.explicit_nonce() { 8 } else { 0 };
        let len = extra + body.len() + 16;
        let mut out = Vec::with_capacity(5 + len);
        out.extend_from_slice(&[kind, 3, 3, (len >> 8) as u8, len as u8]);
        if extra > 0 {
            out.extend_from_slice(&explicit);
        }
        out.extend_from_slice(&body);
        out.extend_from_slice(&tag);
        out
    }

    fn open12(&mut self, head: &[u8], body: &[u8]) -> Result<(u8, Vec<u8>), String> {
        let extra = if self.explicit_nonce() { 8 } else { 0 };
        if body.len() < extra + 16 {
            return Err(String::from("TLS: short record"));
        }
        let mut nonce = self.nonce();
        if extra > 0 {
            nonce[4..].copy_from_slice(&body[..8]);
        }
        let (ct, tag) = body[extra..].split_at(body.len() - extra - 16);
        let mut aad = (self.seq - 1).to_be_bytes().to_vec();
        aad.extend_from_slice(&[head[0], 3, 3, (ct.len() >> 8) as u8, ct.len() as u8]);
        let mut data = ct.to_vec();
        let ok = match &self.cipher {
            Cipher::Aes128(c) => c.decrypt_in_place_detached((&nonce).into(), &aad, &mut data, tag.into()),
            Cipher::Aes256(c) => c.decrypt_in_place_detached((&nonce).into(), &aad, &mut data, tag.into()),
            Cipher::Chacha(c) => c.decrypt_in_place_detached((&nonce).into(), &aad, &mut data, tag.into()),
        };
        ok.map_err(|_| String::from("TLS: a record did not decrypt"))?;
        Ok((head[0], data))
    }

    fn nonce(&mut self) -> [u8; 12] {
        let mut n = self.iv;
        for (i, b) in self.seq.to_be_bytes().iter().enumerate() {
            n[4 + i] ^= b;
        }
        self.seq += 1;
        n
    }

    /// A whole record carrying `data` as `kind`.
    fn seal(&mut self, kind: u8, data: &[u8]) -> Vec<u8> {
        if self.v12 {
            return self.seal12(kind, data);
        }
        let mut body = Vec::with_capacity(data.len() + 32);
        body.extend_from_slice(data);
        body.push(kind);
        let len = body.len() + 16;
        let head = [APP_DATA, 3, 3, (len >> 8) as u8, len as u8];
        let nonce = self.nonce();
        let tag = match &self.cipher {
            Cipher::Aes128(c) => c.encrypt_in_place_detached((&nonce).into(), &head, &mut body),
            Cipher::Aes256(c) => c.encrypt_in_place_detached((&nonce).into(), &head, &mut body),
            Cipher::Chacha(c) => c.encrypt_in_place_detached((&nonce).into(), &head, &mut body),
        }
        .map(|t| t.to_vec())
        .unwrap_or_default();
        let mut out = Vec::with_capacity(5 + len);
        out.extend_from_slice(&head);
        out.extend_from_slice(&body);
        out.extend_from_slice(&tag);
        out
    }

    /// The content type and data of an encrypted record.
    fn open(&mut self, head: &[u8], body: &[u8]) -> Result<(u8, Vec<u8>), String> {
        if self.v12 {
            return self.open12(head, body);
        }
        if body.len() < 17 {
            return Err(String::from("TLS: short record"));
        }
        let (ct, tag) = body.split_at(body.len() - 16);
        let mut data = ct.to_vec();
        let nonce = self.nonce();
        let ok = match &self.cipher {
            Cipher::Aes128(c) => c.decrypt_in_place_detached((&nonce).into(), head, &mut data, tag.into()),
            Cipher::Aes256(c) => c.decrypt_in_place_detached((&nonce).into(), head, &mut data, tag.into()),
            Cipher::Chacha(c) => c.decrypt_in_place_detached((&nonce).into(), head, &mut data, tag.into()),
        };
        ok.map_err(|_| String::from("TLS: a record did not decrypt"))?;
        while data.last() == Some(&0) {
            data.pop();
        }
        let kind = data.pop().ok_or("TLS: empty record")?;
        Ok((kind, data))
    }
}

pub struct Tls<T: Transport = Socket> {
    sock: T,
    /// Bytes from the socket not yet made into records.
    rx: Vec<u8>,
    /// Decrypted application data not yet read.
    plain: Vec<u8>,
    plain_pos: usize,
    read_keys: Keys,
    write_keys: Keys,
    suite: Suite,
    server_secret: Vec<u8>,
    client_secret: Vec<u8>,
    /// XTLS Vision took the TLS layer off what the server sends.
    raw_read: bool,
    closed: bool,
    /// Talking TLS 1.2.
    v12: bool,
}

/// Our key shares: X25519, and P-256 for servers without X25519.
struct Shares {
    x25519: StaticSecret,
    p256: Option<p256::SecretKey>,
}

impl Shares {
    fn new(p256: bool) -> Shares {
        let mut seed = [0u8; 32];
        random(&mut seed);
        let p256 = p256.then(|| loop {
            let mut k = [0u8; 32];
            random(&mut k);
            if let Ok(key) = p256::SecretKey::from_slice(&k) {
                break key;
            }
        });
        Shares {
            x25519: StaticSecret::from(seed),
            p256,
        }
    }

    fn p256_public(&self) -> Option<Vec<u8>> {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        self.p256
            .as_ref()
            .map(|k| k.public_key().to_encoded_point(false).as_bytes().to_vec())
    }

    /// The shared secret with the server's share for `group`.
    fn agree(&self, group: u16, theirs: &[u8]) -> Result<Vec<u8>, String> {
        match group {
            0x001d => {
                let p: [u8; 32] = theirs.try_into().map_err(|_| "TLS: bad key share")?;
                Ok(self.x25519.diffie_hellman(&PublicKey::from(p)).as_bytes().to_vec())
            }
            0x0017 => {
                let ours = self.p256.as_ref().ok_or("TLS: unexpected key type")?;
                let pk = p256::PublicKey::from_sec1_bytes(theirs).map_err(|_| "TLS: bad key share")?;
                let shared = p256::ecdh::diffie_hellman(ours.to_nonzero_scalar(), pk.as_affine());
                Ok(shared.raw_secret_bytes().to_vec())
            }
            _ => Err(String::from("TLS: the server wants a key type RyzikOS doesn't offer")),
        }
    }
}

fn put_u16(v: &mut Vec<u8>, x: usize) {
    v.extend_from_slice(&(x as u16).to_be_bytes());
}

fn put_ext(v: &mut Vec<u8>, kind: u16, data: &[u8]) {
    v.extend_from_slice(&kind.to_be_bytes());
    put_u16(v, data.len());
    v.extend_from_slice(data);
}

/// A GREASE value as Chrome picks them: 0x?a?a.
fn grease() -> u16 {
    let mut b = [0u8; 1];
    random(&mut b);
    let n = (b[0] & 0x0f) as u16;
    (n << 12) | 0x0a00 | (n << 4) | 0x0a
}

fn client_hello(random32: &[u8; 32], share: &[u8; 32], p256: Option<&[u8]>, opts: &Options) -> Vec<u8> {
    let g = grease();
    let mut body = Vec::new();
    body.extend_from_slice(&[3, 3]);
    body.extend_from_slice(random32);
    // the session id: random, or REALITY's sealed note filled in later
    body.push(32);
    let mut sid = [0u8; 32];
    if opts.reality.is_none() {
        random(&mut sid);
    }
    body.extend_from_slice(&sid);
    // REALITY looks like Chrome; elsewhere, only what we can speak
    let chrome = [
        g, 0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8, 0xc013,
        0xc014, 0x009c, 0x009d, 0x002f, 0x0035,
    ];
    let suites: &[u16] = if opts.reality.is_some() {
        &chrome
    } else {
        &chrome[..10]
    };
    put_u16(&mut body, suites.len() * 2);
    for s in suites {
        body.extend_from_slice(&s.to_be_bytes());
    }
    body.extend_from_slice(&[1, 0]);

    let mut ext = Vec::new();
    put_ext(&mut ext, g ^ 0x1010, &[]);
    if !opts.sni.is_empty() && crate::net::parse_ipv4(opts.sni).is_none() {
        let name = opts.sni.as_bytes();
        let mut sni = Vec::new();
        put_u16(&mut sni, name.len() + 3);
        sni.push(0);
        put_u16(&mut sni, name.len());
        sni.extend_from_slice(name);
        put_ext(&mut ext, 0, &sni);
    }
    put_ext(&mut ext, 23, &[]); // extended master secret
    put_ext(&mut ext, 0xff01, &[0]); // renegotiation info
    let gg = grease();
    // P-384 has no key share of ours, but TLS 1.2 servers with P-384
    // certificates want it listed; servers go by our order and pick
    // X25519 or P-256 for the key exchange
    let groups = [gg, 0x001d, 0x0017, 0x0018];
    let mut sg = Vec::new();
    put_u16(&mut sg, groups.len() * 2);
    for x in groups {
        sg.extend_from_slice(&x.to_be_bytes());
    }
    put_ext(&mut ext, 10, &sg);
    put_ext(&mut ext, 11, &[1, 0]); // point formats
    put_ext(&mut ext, 35, &[]); // session tickets
    if !opts.alpn.is_empty() {
        let mut list = Vec::new();
        for a in opts.alpn {
            list.push(a.len() as u8);
            list.extend_from_slice(a.as_bytes());
        }
        let mut alpn = Vec::new();
        put_u16(&mut alpn, list.len());
        alpn.extend_from_slice(&list);
        put_ext(&mut ext, 16, &alpn);
    }
    put_ext(&mut ext, 5, &[1, 0, 0, 0, 0]); // OCSP stapling
    let sigs: [u16; 8] = [0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601];
    let mut sa = Vec::new();
    put_u16(&mut sa, sigs.len() * 2);
    for x in sigs {
        sa.extend_from_slice(&x.to_be_bytes());
    }
    put_ext(&mut ext, 13, &sa);
    put_ext(&mut ext, 18, &[]); // certificate transparency
    let mut ks = Vec::new();
    put_u16(&mut ks, 5 + 36 + p256.map_or(0, |p| 4 + p.len()));
    ks.extend_from_slice(&gg.to_be_bytes());
    ks.extend_from_slice(&[0, 1, 0]);
    ks.extend_from_slice(&[0x00, 0x1d, 0, 32]);
    ks.extend_from_slice(share);
    if let Some(p) = p256 {
        ks.extend_from_slice(&[0x00, 0x17, 0, p.len() as u8]);
        ks.extend_from_slice(p);
    }
    put_ext(&mut ext, 51, &ks);
    put_ext(&mut ext, 45, &[1, 1]); // PSK with (EC)DHE
    let gv = grease();
    let mut sv = vec![6];
    sv.extend_from_slice(&gv.to_be_bytes());
    sv.extend_from_slice(&[3, 4, 3, 3]);
    put_ext(&mut ext, 43, &sv);
    let g2 = grease();
    put_ext(&mut ext, if g2 == g ^ 0x1010 { g2 ^ 0x2020 } else { g2 }, &[0]);
    // pad to 512 bytes like Chrome, which some servers expect
    let unpadded = 4 + body.len() + 2 + ext.len();
    if unpadded < 512 {
        let pad = (512 - unpadded).saturating_sub(4).max(1);
        put_ext(&mut ext, 21, &vec![0u8; pad]);
    }
    put_u16(&mut body, ext.len());
    body.extend_from_slice(&ext);

    let mut msg = vec![1u8];
    msg.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    msg.extend_from_slice(&body);
    msg
}

/// HKDF-SHA256 with one block of output (REALITY's key).
fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf::Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, &mut out)
        .unwrap();
    out
}

/// REALITY: seal the version, the time and the short id into the
/// session id, under a key only the server can also compute.
fn seal_reality(msg: &mut [u8], secret: &StaticSecret, r: &Reality) -> [u8; 32] {
    let shared = secret.diffie_hellman(&PublicKey::from(r.public_key));
    let random32: [u8; 32] = msg[6..38].try_into().unwrap();
    let key = hkdf_sha256(shared.as_bytes(), &random32[..20], b"REALITY");
    let mut note = [0u8; 16];
    // the client's version (Xray 26.3.27), then reserved, time, short id
    note[..3].copy_from_slice(&[26, 3, 27]);
    let now = (crate::tg::mtproto::now_ms() / 1000) as u32;
    note[4..8].copy_from_slice(&now.to_be_bytes());
    note[8..].copy_from_slice(&r.short_id);
    let aead = Aes256Gcm::new_from_slice(&key).unwrap();
    let nonce: [u8; 12] = random32[20..].try_into().unwrap();
    let mut data = note.to_vec();
    let tag = aead
        .encrypt_in_place_detached((&nonce).into(), msg, &mut data)
        .unwrap();
    data.extend_from_slice(&tag);
    msg[39..71].copy_from_slice(&data);
    key
}

/// REALITY's server signs its throwaway certificate with an HMAC of its
/// ed25519 key; any other certificate means the server did not accept us
/// and is showing the real site instead.
fn reality_cert_ok(cert: &[u8], key: &[u8; 32]) -> bool {
    let oid = [0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
    let Some(at) = cert.windows(oid.len()).position(|w| w == oid) else {
        return false;
    };
    let Some(public) = cert.get(at + oid.len()..at + oid.len() + 32) else {
        return false;
    };
    if cert.len() < 64 {
        return false;
    }
    let signature = &cert[cert.len() - 64..];
    let mut m = <Hmac<Sha512> as Mac>::new_from_slice(key).unwrap();
    m.update(public);
    m.finalize().into_bytes().as_slice() == signature
}

fn alert_text(code: u8) -> &'static str {
    match code {
        0 => "the server closed the connection",
        40 => "handshake failure",
        42 | 43 | 44 | 45 | 46 => "the server did not like a certificate",
        47 => "illegal parameter",
        50 => "decode error",
        70 => "protocol version",
        80 => "internal error",
        112 => "unknown server name",
        120 => "no application protocol",
        _ => "alert",
    }
}

impl<T: Transport> Tls<T> {
    /// Connect over `sock` and do the handshake.
    pub fn connect(sock: T, opts: &Options) -> Result<Tls<T>, String> {
        let shares = Shares::new(opts.reality.is_none());
        let share = PublicKey::from(&shares.x25519);
        let p256 = shares.p256_public();
        let mut random32 = [0u8; 32];
        random(&mut random32);
        let mut hello = client_hello(&random32, share.as_bytes(), p256.as_deref(), opts);
        let auth = opts.reality.map(|r| seal_reality(&mut hello, &shares.x25519, r));

        let mut t = Tls {
            sock,
            rx: Vec::new(),
            plain: Vec::new(),
            plain_pos: 0,
            // placeholders until the handshake keys are known
            read_keys: Keys::new(Suite::Aes128, &[0; 32]),
            write_keys: Keys::new(Suite::Aes128, &[0; 32]),
            suite: Suite::Aes128,
            server_secret: Vec::new(),
            client_secret: Vec::new(),
            raw_read: false,
            closed: false,
            v12: false,
        };
        let mut record = vec![HANDSHAKE, 3, 1];
        put_u16(&mut record, hello.len());
        record.extend_from_slice(&hello);
        t.sock.write_all(&record)?;

        // ServerHello, in the clear; in TLS 1.2 the messages after it may
        // share its record
        let mut hs_buf: Vec<u8> = Vec::new();
        loop {
            if hs_buf.len() >= 4 {
                let len = u32::from_be_bytes([0, hs_buf[1], hs_buf[2], hs_buf[3]]) as usize;
                if hs_buf.len() >= 4 + len {
                    break;
                }
            }
            let (kind, _, body) = t.next_record(true)?.ok_or("TLS: no answer")?;
            match kind {
                CHANGE_CIPHER => continue,
                ALERT => {
                    let code = body.get(1).copied().unwrap_or(0);
                    return Err(format!("TLS: {} ({})", alert_text(code), code));
                }
                HANDSHAKE => hs_buf.extend_from_slice(&body),
                _ => return Err(String::from("TLS: not a TLS server")),
            }
        }
        if hs_buf[0] != 2 {
            return Err(String::from("TLS: not a TLS server"));
        }
        let sh_len = u32::from_be_bytes([0, hs_buf[1], hs_buf[2], hs_buf[3]]) as usize;
        let sh: Vec<u8> = hs_buf.drain(..4 + sh_len).collect();
        let server = parse_server_hello(&sh[4..])?;
        let mut transcript = hello.clone();
        transcript.extend_from_slice(&sh);
        if server.version != 0x0304 {
            if auth.is_some() {
                return Err(String::from(
                    "TLS: the server only speaks TLS 1.2; RyzikOS's VPN needs TLS 1.3",
                ));
            }
            return t.handshake12(transcript, hs_buf, &random32, &server, &shares);
        }
        let suite = Suite::from_id(server.suite).ok_or("TLS: unknown cipher")?;
        let shared = shares.agree(server.group, &server.share)?;

        let hl = suite.hash_len();
        let zeros = vec![0u8; hl];
        let early = suite.extract(&[], &zeros);
        let derived = suite.expand_label(&early, "derived", &suite.hash(&[]), hl);
        let hs = suite.extract(&derived, &shared);
        let th = suite.hash(&transcript);
        let c_hs = suite.expand_label(&hs, "c hs traffic", &th, hl);
        let s_hs = suite.expand_label(&hs, "s hs traffic", &th, hl);
        t.suite = suite;
        t.read_keys = Keys::new(suite, &s_hs);
        t.write_keys = Keys::new(suite, &c_hs);

        // the encrypted part of the server's handshake
        hs_buf.clear();
        let mut cert_request: Option<Vec<u8>> = None;
        let mut reality_ok = false;
        loop {
            while hs_buf.len() >= 4 {
                let len = u32::from_be_bytes([0, hs_buf[1], hs_buf[2], hs_buf[3]]) as usize;
                if hs_buf.len() < 4 + len {
                    break;
                }
                let msg: Vec<u8> = hs_buf.drain(..4 + len).collect();
                match msg[0] {
                    11 => {
                        if let Some(key) = &auth {
                            reality_ok = first_cert(&msg[4..]).is_some_and(|c| reality_cert_ok(c, key));
                            if !reality_ok {
                                return Err(String::from(
                                    "REALITY: the server did not accept the key (check pbk, sid and sni)",
                                ));
                            }
                        }
                    }
                    13 => {
                        let ctx_len = *msg.get(4).unwrap_or(&0) as usize;
                        cert_request = Some(msg.get(5..5 + ctx_len).unwrap_or(&[]).to_vec());
                    }
                    20 => {
                        let finished_key = suite.expand_label(&s_hs, "finished", &[], hl);
                        let expected = suite.hmac(&finished_key, &[&suite.hash(&transcript)]);
                        if msg[4..] != expected[..] {
                            return Err(String::from("TLS: the server's Finished is wrong"));
                        }
                        transcript.extend_from_slice(&msg);
                        if auth.is_some() && !reality_ok {
                            return Err(String::from("REALITY: no certificate from the server"));
                        }
                        return t.finish(transcript, &hs, &c_hs, cert_request);
                    }
                    _ => {}
                }
                transcript.extend_from_slice(&msg);
            }
            let (kind, head, body) = t.next_record(true)?.ok_or("TLS: the server hung up")?;
            match kind {
                CHANGE_CIPHER => {}
                ALERT => {
                    let code = body.get(1).copied().unwrap_or(0);
                    return Err(format!("TLS: {} ({})", alert_text(code), code));
                }
                APP_DATA => {
                    let (inner, data) = t.read_keys.open(&head, &body)?;
                    match inner {
                        HANDSHAKE => hs_buf.extend_from_slice(&data),
                        ALERT => {
                            let code = data.get(1).copied().unwrap_or(0);
                            return Err(format!("TLS: {} ({})", alert_text(code), code));
                        }
                        _ => return Err(String::from("TLS: unexpected record")),
                    }
                }
                _ => return Err(String::from("TLS: unexpected record")),
            }
        }
    }

    /// The rest of a TLS 1.2 handshake with ECDHE, after the ServerHello.
    fn handshake12(
        mut self,
        mut transcript: Vec<u8>,
        mut hs_buf: Vec<u8>,
        client_random: &[u8; 32],
        server: &ServerHello,
        shares: &Shares,
    ) -> Result<Self, String> {
        let suite = Suite::from_id12(server.suite).ok_or("TLS: unknown cipher")?;
        // the certificate, the server's key share, maybe a certificate
        // request, then ServerHelloDone
        let mut key: Option<(u16, Vec<u8>)> = None;
        let mut cert_request = false;
        'messages: loop {
            while hs_buf.len() >= 4 {
                let len = u32::from_be_bytes([0, hs_buf[1], hs_buf[2], hs_buf[3]]) as usize;
                if hs_buf.len() < 4 + len {
                    break;
                }
                let msg: Vec<u8> = hs_buf.drain(..4 + len).collect();
                transcript.extend_from_slice(&msg);
                match msg[0] {
                    12 => {
                        // named curve, then the point; its signature is not checked
                        let b = &msg[4..];
                        if b.len() < 4 || b[0] != 3 {
                            return Err(String::from("TLS: the server's key exchange is not supported"));
                        }
                        let group = u16::from_be_bytes([b[1], b[2]]);
                        let n = b[3] as usize;
                        let point = b.get(4..4 + n).ok_or("TLS: short key exchange")?;
                        key = Some((group, point.to_vec()));
                    }
                    13 => cert_request = true,
                    14 => break 'messages,
                    _ => {}
                }
            }
            let (kind, _, body) = self.next_record(true)?.ok_or("TLS: the server hung up")?;
            match kind {
                HANDSHAKE => hs_buf.extend_from_slice(&body),
                ALERT => {
                    let code = body.get(1).copied().unwrap_or(0);
                    return Err(format!("TLS: {} ({})", alert_text(code), code));
                }
                _ => return Err(String::from("TLS: unexpected record")),
            }
        }
        let (group, point) = key.ok_or("TLS: the server sent no key")?;
        let premaster = shares.agree(group, &point)?;
        let ours = if group == 0x001d {
            PublicKey::from(&shares.x25519).as_bytes().to_vec()
        } else {
            shares.p256_public().unwrap_or_default()
        };

        let mut msgs = Vec::new();
        if cert_request {
            // no client certificate
            msgs.extend_from_slice(&[11, 0, 0, 3, 0, 0, 0]);
        }
        msgs.extend_from_slice(&[16, 0, 0, ours.len() as u8 + 1, ours.len() as u8]);
        msgs.extend_from_slice(&ours);
        transcript.extend_from_slice(&msgs);

        let master = if server.extended_master {
            suite.prf(&premaster, "extended master secret", &[&suite.hash(&transcript)], 48)
        } else {
            suite.prf(&premaster, "master secret", &[client_random, &server.random], 48)
        };
        let kl = suite.key_len();
        let il = if suite == Suite::Chacha { 12 } else { 4 };
        let block = suite.prf(&master, "key expansion", &[&server.random, client_random], 2 * (kl + il));
        let (ck, rest) = block.split_at(kl);
        let (sk, rest) = rest.split_at(kl);
        let (civ, siv) = rest.split_at(il);
        self.write_keys = Keys::new12(suite, ck, civ);
        self.read_keys = Keys::new12(suite, sk, &siv[..il]);

        let verify = suite.prf(&master, "client finished", &[&suite.hash(&transcript)], 12);
        let mut finished = vec![20u8, 0, 0, 12];
        finished.extend_from_slice(&verify);
        let mut out = vec![HANDSHAKE, 3, 3];
        put_u16(&mut out, msgs.len());
        out.extend_from_slice(&msgs);
        out.extend_from_slice(&[CHANGE_CIPHER, 3, 3, 0, 1, 1]);
        out.extend_from_slice(&self.write_keys.seal(HANDSHAKE, &finished));
        transcript.extend_from_slice(&finished);
        self.sock.write_all(&out)?;

        // maybe a session ticket, then the server's Finished, encrypted
        let mut encrypted = false;
        hs_buf.clear();
        loop {
            while hs_buf.len() >= 4 {
                let len = u32::from_be_bytes([0, hs_buf[1], hs_buf[2], hs_buf[3]]) as usize;
                if hs_buf.len() < 4 + len {
                    break;
                }
                let msg: Vec<u8> = hs_buf.drain(..4 + len).collect();
                if msg[0] == 20 {
                    let expected = suite.prf(&master, "server finished", &[&suite.hash(&transcript)], 12);
                    if msg[4..] != expected[..] {
                        return Err(String::from("TLS: the server's Finished is wrong"));
                    }
                    self.suite = suite;
                    self.v12 = true;
                    return Ok(self);
                }
                transcript.extend_from_slice(&msg);
            }
            let (kind, head, body) = self.next_record(true)?.ok_or("TLS: the server hung up")?;
            let body = if encrypted && kind != CHANGE_CIPHER {
                self.read_keys.open(&head, &body)?.1
            } else {
                body
            };
            match kind {
                CHANGE_CIPHER => encrypted = true,
                HANDSHAKE => hs_buf.extend_from_slice(&body),
                ALERT => {
                    let code = body.get(1).copied().unwrap_or(0);
                    return Err(format!("TLS: {} ({})", alert_text(code), code));
                }
                _ => return Err(String::from("TLS: unexpected record")),
            }
        }
    }

    fn finish(
        mut self,
        mut transcript: Vec<u8>,
        hs: &[u8],
        c_hs: &[u8],
        cert_request: Option<Vec<u8>>,
    ) -> Result<Tls<T>, String> {
        let suite = self.suite;
        let hl = suite.hash_len();
        let th = suite.hash(&transcript);
        let derived = suite.expand_label(hs, "derived", &suite.hash(&[]), hl);
        let master = suite.extract(&derived, &vec![0u8; hl]);
        let c_ap = suite.expand_label(&master, "c ap traffic", &th, hl);
        let s_ap = suite.expand_label(&master, "s ap traffic", &th, hl);

        let mut out = vec![CHANGE_CIPHER, 3, 3, 0, 1, 1];
        if let Some(ctx) = cert_request {
            // no client certificate: an empty list
            let mut msg = vec![11u8, 0, 0, 0];
            msg.push(ctx.len() as u8);
            msg.extend_from_slice(&ctx);
            msg.extend_from_slice(&[0, 0, 0]);
            let n = msg.len() - 4;
            msg[1..4].copy_from_slice(&(n as u32).to_be_bytes()[1..]);
            out.extend_from_slice(&self.write_keys.seal(HANDSHAKE, &msg));
            transcript.extend_from_slice(&msg);
        }
        let finished_key = suite.expand_label(c_hs, "finished", &[], hl);
        let verify = suite.hmac(&finished_key, &[&suite.hash(&transcript)]);
        let mut msg = vec![20u8, 0, 0, verify.len() as u8];
        msg.extend_from_slice(&verify);
        out.extend_from_slice(&self.write_keys.seal(HANDSHAKE, &msg));
        self.sock.write_all(&out)?;
        self.read_keys = Keys::new(suite, &s_ap);
        self.write_keys = Keys::new(suite, &c_ap);
        self.server_secret = s_ap;
        self.client_secret = c_ap;
        Ok(self)
    }

    /// The next whole record from the socket: its type, header and body.
    /// Without `block`, None if a whole one has not arrived yet.
    fn next_record(&mut self, block: bool) -> Result<Option<(u8, [u8; 5], Vec<u8>)>, String> {
        loop {
            if self.rx.len() >= 5 {
                let len = u16::from_be_bytes([self.rx[3], self.rx[4]]) as usize;
                if len > MAX_RECORD {
                    return Err(String::from("TLS: record too long"));
                }
                if self.rx.len() >= 5 + len {
                    let head: [u8; 5] = self.rx[..5].try_into().unwrap();
                    let body = self.rx[5..5 + len].to_vec();
                    self.rx.drain(..5 + len);
                    return Ok(Some((head[0], head, body)));
                }
            }
            if !self.fill(block)? {
                return Ok(None);
            }
        }
    }

    /// Read more from the socket. False if nothing came (without block).
    fn fill(&mut self, block: bool) -> Result<bool, String> {
        let mut buf = [0u8; 8192];
        let n = if block {
            self.sock.read(&mut buf)?
        } else {
            match self.sock.try_read(&mut buf)? {
                Some(n) => n,
                None => return Ok(false),
            }
        };
        if n == 0 {
            return Err(String::from(CLOSED));
        }
        self.rx.extend_from_slice(&buf[..n]);
        Ok(true)
    }

    pub fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        let mut out = Vec::with_capacity(data.len() + 64);
        for chunk in data.chunks(16384) {
            out.extend_from_slice(&self.write_keys.seal(APP_DATA, chunk));
        }
        self.sock.write_all(&out)
    }

    /// Read application data. Without `block`, None if nothing is there
    /// yet; Some(0) when the server has closed the connection.
    pub fn read(&mut self, buf: &mut [u8], block: bool) -> Result<Option<usize>, String> {
        loop {
            if self.plain_pos < self.plain.len() {
                let n = buf.len().min(self.plain.len() - self.plain_pos);
                buf[..n].copy_from_slice(&self.plain[self.plain_pos..self.plain_pos + n]);
                self.plain_pos += n;
                return Ok(Some(n));
            }
            if self.closed {
                return Ok(Some(0));
            }
            if self.raw_read {
                if !self.rx.is_empty() {
                    let n = buf.len().min(self.rx.len());
                    buf[..n].copy_from_slice(&self.rx[..n]);
                    self.rx.drain(..n);
                    return Ok(Some(n));
                }
                return if block {
                    self.sock.read(buf).map(Some)
                } else {
                    self.sock.try_read(buf)
                };
            }
            let record = match self.next_record(block) {
                Ok(Some(r)) => r,
                Ok(None) => return Ok(None),
                Err(e) if e == CLOSED => return Ok(Some(0)),
                Err(e) => return Err(e),
            };
            let (kind, head, body) = record;
            match kind {
                APP_DATA => {}
                CHANGE_CIPHER => continue,
                // TLS 1.2 encrypts alerts and handshakes under their own type
                ALERT | HANDSHAKE if self.v12 => {}
                ALERT => {
                    self.closed = true;
                    continue;
                }
                _ => return Err(String::from("TLS: unexpected record")),
            }
            let (inner, data) = self.read_keys.open(&head, &body)?;
            match inner {
                APP_DATA => {
                    self.plain = data;
                    self.plain_pos = 0;
                }
                HANDSHAKE => self.post_handshake(&data)?,
                ALERT => {
                    let code = data.get(1).copied().unwrap_or(0);
                    if code == 0 {
                        self.closed = true;
                    } else {
                        return Err(format!("TLS: {} ({})", alert_text(code), code));
                    }
                }
                _ => {}
            }
        }
    }

    /// Session tickets are ignored; a key update is followed.
    fn post_handshake(&mut self, data: &[u8]) -> Result<(), String> {
        let mut rest = data;
        while rest.len() >= 4 {
            let len = u32::from_be_bytes([0, rest[1], rest[2], rest[3]]) as usize;
            let body = rest.get(4..4 + len).unwrap_or(&[]);
            if rest[0] == 24 {
                let hl = self.suite.hash_len();
                self.server_secret = self.suite.expand_label(&self.server_secret, "traffic upd", &[], hl);
                self.read_keys = Keys::new(self.suite, &self.server_secret);
                if body.first() == Some(&1) {
                    let msg = [24u8, 0, 0, 1, 0];
                    let rec = self.write_keys.seal(HANDSHAKE, &msg);
                    self.sock.write_all(&rec)?;
                    self.client_secret = self.suite.expand_label(&self.client_secret, "traffic upd", &[], hl);
                    self.write_keys = Keys::new(self.suite, &self.client_secret);
                }
            }
            rest = rest.get(4 + len..).unwrap_or(&[]);
        }
        Ok(())
    }

    /// XTLS Vision: from here on the server sends plain bytes, not records.
    pub fn switch_to_raw_read(&mut self) {
        self.raw_read = true;
    }

    /// Whether decrypted data is waiting (Vision must not switch then).
    pub fn has_buffered(&self) -> bool {
        self.plain_pos < self.plain.len()
    }
}

/// What the server chose in its ServerHello.
struct ServerHello {
    random: [u8; 32],
    version: u16,
    suite: u16,
    /// TLS 1.3: the key share's group and bytes.
    group: u16,
    share: Vec<u8>,
    /// TLS 1.2: the extended master secret (RFC 7627) is on.
    extended_master: bool,
}

fn parse_server_hello(b: &[u8]) -> Result<ServerHello, String> {
    const RETRY: [u8; 8] = [0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11];
    if b.len() < 38 {
        return Err(String::from("TLS: short ServerHello"));
    }
    if b[2..10] == RETRY {
        return Err(String::from("TLS: the server wants a key type RyzikOS doesn't offer"));
    }
    let random: [u8; 32] = b[2..34].try_into().unwrap();
    let sid_len = b[34] as usize;
    let mut p = 35 + sid_len;
    let suite = u16::from_be_bytes([*b.get(p).ok_or("TLS: short")?, *b.get(p + 1).ok_or("TLS: short")?]);
    p += 3;
    let mut hello = ServerHello {
        random,
        version: 0x0303,
        suite,
        group: 0,
        share: Vec::new(),
        extended_master: false,
    };
    // a TLS 1.2 server may send no extensions at all
    let Some(len) = b.get(p..p + 2) else {
        return Ok(hello);
    };
    let ext_len = u16::from_be_bytes([len[0], len[1]]) as usize;
    p += 2;
    let exts = b.get(p..p + ext_len).ok_or("TLS: short ServerHello")?;
    let mut q = 0;
    while q + 4 <= exts.len() {
        let kind = u16::from_be_bytes([exts[q], exts[q + 1]]);
        let len = u16::from_be_bytes([exts[q + 2], exts[q + 3]]) as usize;
        let data = exts.get(q + 4..q + 4 + len).ok_or("TLS: bad extension")?;
        match kind {
            43 if len == 2 => hello.version = u16::from_be_bytes([data[0], data[1]]),
            51 if len >= 4 => {
                hello.group = u16::from_be_bytes([data[0], data[1]]);
                let n = u16::from_be_bytes([data[2], data[3]]) as usize;
                hello.share = data.get(4..4 + n).ok_or("TLS: bad key share")?.to_vec();
            }
            23 => hello.extended_master = true,
            _ => {}
        }
        q += 4 + len;
    }
    Ok(hello)
}

/// The first certificate in a Certificate message.
fn first_cert(b: &[u8]) -> Option<&[u8]> {
    let ctx = *b.first()? as usize;
    let p = 1 + ctx + 3;
    let len = u32::from_be_bytes([0, *b.get(p)?, *b.get(p + 1)?, *b.get(p + 2)?]) as usize;
    b.get(p + 3..p + 3 + len)
}
