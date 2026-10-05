//! MTProto 2.0: the TCP transport, creating an authorization key with
//! the server (Diffie-Hellman under RSA), and encrypting messages with it.
//! See https://core.telegram.org/mtproto.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use num_bigint::BigUint;
use smoltcp::wire::Ipv4Address;

use super::crypto::{self, sha1, sha256};
use super::tl::{Obj, Reader, Value, Writer};
use crate::interrupts;
use crate::net::TcpStream;

#[derive(Debug, Clone)]
pub enum Error {
    /// The connection failed or timed out.
    Net(String),
    /// The server answered a request with an error.
    Rpc {
        code: i32,
        message: String,
    },
    /// The server does not know our key any more (transport error -404).
    KeyUnknown,
    Other(String),
}

impl Error {
    pub fn text(&self) -> String {
        match self {
            Error::Net(e) => format!("Network error: {}", e),
            Error::Rpc { message, .. } => String::from(message.as_str()),
            Error::KeyUnknown => String::from("The server forgot this session"),
            Error::Other(e) => e.clone(),
        }
    }

    /// The number at the end of errors like PHONE_MIGRATE_4 or
    /// FLOOD_WAIT_30.
    pub fn number_after(&self, prefix: &str) -> Option<i64> {
        match self {
            Error::Rpc { message, .. } => message.strip_prefix(prefix)?.parse().ok(),
            _ => None,
        }
    }

    pub fn is(&self, name: &str) -> bool {
        matches!(self, Error::Rpc { message, .. } if message == name)
    }
}

impl From<&str> for Error {
    fn from(e: &str) -> Error {
        Error::Other(String::from(e))
    }
}

impl From<String> for Error {
    fn from(e: String) -> Error {
        Error::Other(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

// ---- time ------------------------------------------------------------------------

/// Milliseconds since the Unix epoch by the computer's clock. The clock
/// is read once; the timer counts from there.
pub fn now_ms() -> i64 {
    use core::sync::atomic::{AtomicI64, Ordering};
    static BASE: AtomicI64 = AtomicI64::new(i64::MIN);
    static BASE_TICKS: AtomicI64 = AtomicI64::new(0);
    let ticks = interrupts::ticks() as i64;
    if BASE.load(Ordering::Relaxed) == i64::MIN {
        BASE_TICKS.store(ticks, Ordering::Relaxed);
        BASE.store(crate::clock::utc() * 1000, Ordering::Relaxed);
    }
    let elapsed = ticks - BASE_TICKS.load(Ordering::Relaxed);
    BASE.load(Ordering::Relaxed) + elapsed * 1000 / interrupts::TIMER_HZ as i64
}

/// Seconds to add to [`now_ms`] for the real (UTC) time. A PC's clock
/// usually holds local time, hours off, and Telegram ignores messages
/// whose time is more than half a minute ahead or five minutes behind,
/// so before its first message we ask a web server what time it is.
pub fn clock_offset() -> i64 {
    use core::sync::atomic::{AtomicI64, Ordering};
    static OFFSET: AtomicI64 = AtomicI64::new(i64::MIN);
    let known = OFFSET.load(Ordering::Relaxed);
    if known != i64::MIN {
        return known;
    }
    super::client::log("checking the time");
    for url in [
        "http://www.google.com/generate_204",
        "https://api.github.com/zen",
    ] {
        let Some(u) = crate::web::url::Url::parse(url) else {
            continue;
        };
        if let Ok(Some(utc)) = crate::web::http::get(&u, None).map(|r| r.date) {
            let offset = utc - now_ms() / 1000;
            super::client::log(&alloc::format!("this PC's clock is {} s off", -offset));
            OFFSET.store(offset, Ordering::Relaxed);
            return offset;
        }
    }
    0
}

// ---- transport ---------------------------------------------------------------------

/// TCP with the "intermediate" framing (a 4-byte length before every
/// packet), obfuscated the way Telegram's own apps do it: the connection
/// starts with 64 random-looking bytes that carry the AES-CTR keys for
/// both directions, and everything after is encrypted. Without it some
/// networks recognise MTProto and drop the connection.
pub struct Transport {
    stream: TcpStream,
    buf: Vec<u8>,
    send_ctr: crypto::Ctr,
    recv_ctr: crypto::Ctr,
    /// The VPN's state when connecting: when it goes on or off, this
    /// connection leads the wrong way and is dropped.
    route: u64,
}

/// How long to wait for an answer.
pub const TIMEOUT_MS: i64 = 30_000;
const KEY_TIMEOUT_MS: i64 = 15_000;

/// The protocol tag of the intermediate framing.
const INTERMEDIATE: [u8; 4] = [0xee; 4];

impl Transport {
    /// Connect for data centre `dc` (as Telegram numbers it: 10000 and
    /// more for the test servers).
    pub fn connect(ip: Ipv4Address, port: u16, dc: i32) -> Result<Transport> {
        let mut stream = TcpStream::connect(ip, port).map_err(Error::Net)?;
        let init = obfuscation_header(dc);
        let rev: Vec<u8> = init[8..56].iter().rev().copied().collect();
        let mut send_ctr = crypto::Ctr::new(&init[8..40], &init[40..56]);
        let recv_ctr = crypto::Ctr::new(&rev[..32], &rev[32..48]);
        let mut encrypted = init;
        send_ctr.apply(&mut encrypted);
        let mut first = init;
        first[56..].copy_from_slice(&encrypted[56..]);
        stream.write_all(&first).map_err(Error::Net)?;
        Ok(Transport {
            stream,
            buf: Vec::new(),
            send_ctr,
            recv_ctr,
            route: crate::vpn::generation(),
        })
    }

    pub fn send(&mut self, packet: &[u8]) -> Result<()> {
        let mut data = Vec::with_capacity(packet.len() + 4);
        data.extend_from_slice(&(packet.len() as u32).to_le_bytes());
        data.extend_from_slice(packet);
        self.send_ctr.apply(&mut data);
        self.stream.write_all(&data).map_err(Error::Net)
    }

    /// A whole packet if one has arrived, without waiting.
    pub fn poll(&mut self) -> Result<Option<Vec<u8>>> {
        if self.route != crate::vpn::generation() {
            return Err(Error::Net(String::from("the VPN was switched; reconnecting")));
        }
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(packet) = self.take_packet()? {
                return Ok(Some(packet));
            }
            match self.stream.try_read(&mut chunk).map_err(Error::Net)? {
                None => return Ok(None),
                Some(0) => {
                    return Err(Error::Net(String::from("the server closed the connection")))
                }
                Some(n) => {
                    self.recv_ctr.apply(&mut chunk[..n]);
                    self.buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
    }

    fn take_packet(&mut self) -> Result<Option<Vec<u8>>> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(self.buf[..4].try_into().unwrap()) as usize;
        if len > 16 * 1024 * 1024 {
            return Err(Error::Net(String::from("bad packet length")));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let packet: Vec<u8> = self.buf[4..4 + len].to_vec();
        self.buf.drain(..4 + len);
        if packet.len() == 4 {
            // a transport error: a negative number such as -404
            let code = i32::from_le_bytes(packet[..4].try_into().unwrap());
            return Err(if code == -404 {
                Error::KeyUnknown
            } else {
                Error::Net(format!("server error {}", code))
            });
        }
        Ok(Some(packet))
    }

    /// Wait for a packet until `deadline` (in [`now_ms`] time).
    pub fn recv(&mut self, deadline: i64) -> Result<Vec<u8>> {
        loop {
            if let Some(p) = self.poll()? {
                return Ok(p);
            }
            if now_ms() > deadline {
                return Err(Error::Net(String::from("the server does not answer")));
            }
            if crate::fiber::cancelled() {
                return Err(Error::Net(String::from("cancelled")));
            }
            crate::fiber::pause();
        }
    }
}

// ---- unencrypted messages (only while creating the key) --------------------------------

fn plain_send(t: &mut Transport, body: &[u8], msg_id: i64) -> Result<()> {
    let mut w = Writer::new();
    w.i64(0);
    w.i64(msg_id);
    w.i32(body.len() as i32);
    w.raw(body);
    t.send(&w.buf)
}

/// Counts key exchanges a server couldn't read; odd means try the older
/// way of wrapping our half of the secret.
static RSA_STYLE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

fn legacy_rsa() -> bool {
    RSA_STYLE.load(core::sync::atomic::Ordering::Relaxed) % 2 == 1
}

fn plain_recv(t: &mut Transport) -> Result<Obj> {
    // a server that answers at all answers the key exchange at once
    let packet = match t.recv(now_ms() + KEY_TIMEOUT_MS) {
        // there is no key yet: -404 here means it couldn't read our message
        Err(Error::KeyUnknown) => {
            RSA_STYLE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            return Err(Error::Net(String::from(
                "the server could not read our key exchange (error -404)",
            )))
        }
        other => other?,
    };
    let mut r = Reader::new(&packet);
    if r.i64()? != 0 {
        return Err(Error::Other(String::from("expected an unencrypted answer")));
    }
    r.i64()?;
    let len = r.i32()? as usize;
    let body = r.take(len)?;
    Ok(Reader::new(body).obj()?)
}

/// The first 64 bytes: random, but not like the start of any other
/// protocol, with the framing tag and the data centre inside.
fn obfuscation_header(dc: i32) -> [u8; 64] {
    loop {
        let mut init: [u8; 64] = crypto::random_array();
        let start = &init[..4];
        if init[0] == 0xef
            || [b"HEAD", b"POST", b"GET ", b"OPTI", &[0xdd; 4], &[0xee; 4], &[0x16, 3, 1, 2]]
                .iter()
                .any(|p| start == &p[..])
            || init[4..8] == [0; 4]
        {
            continue;
        }
        init[56..60].copy_from_slice(&INTERMEDIATE);
        init[60..62].copy_from_slice(&(dc as i16).to_le_bytes());
        return init;
    }
}

fn bad(what: &str) -> Error {
    Error::Other(format!("key exchange: {}", what))
}

/// What creating a key gives: the key itself, the first salt, and how
/// far the server's clock is ahead of ours in seconds.
pub struct NewKey {
    pub key: [u8; 256],
    pub salt: i64,
    pub time_offset: i64,
}

/// Create an authorization key with the server at the other end of `t`.
/// `dc` is the data centre number (+10000 for test servers); `extra_keys`
/// are RSA keys to trust besides Telegram's own.
pub fn create_key(t: &mut Transport, dc: i32, extra_keys: &[(u64, Vec<u8>)]) -> Result<NewKey> {
    let mut msg_id = MsgIds::default();
    let offset = clock_offset();

    // 1. ask for pq
    let nonce: [u8; 16] = crypto::random_array();
    let req = Obj::new("req_pq_multi", &[("nonce", Value::Bytes(nonce.to_vec()))]);
    plain_send(t, &super::tl::encode(&req), msg_id.next(offset))?;
    let res_pq = plain_recv(t)?;
    if !res_pq.is("resPQ") || res_pq.bytes("nonce") != nonce {
        return Err(bad("bad resPQ"));
    }
    let server_nonce = res_pq.bytes("server_nonce").to_vec();
    let pq_bytes = res_pq.bytes("pq");
    if pq_bytes.len() > 8 {
        return Err(bad("pq too big"));
    }
    let pq = pq_bytes.iter().fold(0u64, |a, &b| a << 8 | b as u64);
    let (p, q) = crypto::factor(pq).ok_or_else(|| bad("can't factor pq"))?;
    // the server lists its keys: take a current one if we know one, an
    // old one (with the old encryption) only if not
    let offered: Vec<u64> = res_pq
        .vec("server_public_key_fingerprints")
        .iter()
        .map(|f| f.as_i64() as u64)
        .collect();
    let (fingerprint, modulus) = [false, true]
        .iter()
        .find_map(|&old| {
            offered.iter().find_map(|&f| {
                if crypto::is_old_key(f) != old {
                    return None;
                }
                crypto::server_key(f, extra_keys).map(|m| (f, m))
            })
        })
        .ok_or_else(|| bad("the server has none of the keys we know"))?;
    super::client::log(&alloc::format!(
        "the server offers keys {:x?}, using {:x}",
        offered, fingerprint
    ));

    // 2. send our half of the secret under the server's RSA key
    let be = |v: u64| {
        let b = v.to_be_bytes();
        let skip = b.iter().take_while(|&&x| x == 0).count();
        b[skip..].to_vec()
    };
    let new_nonce: [u8; 32] = crypto::random_array();
    // two ways to wrap it: RSA_PAD with the data centre (MTProto 2.0),
    // and the older SHA-1 and padding that Telethon still uses. After a
    // server rejects one (error -404) the next attempt takes the other.
    let legacy = legacy_rsa();
    super::client::log(if legacy {
        "sending our half the older way"
    } else {
        "sending our half with RSA_PAD"
    });
    let mut fields = alloc::vec![
        ("pq", Value::Bytes(pq_bytes.to_vec())),
        ("p", Value::Bytes(be(p))),
        ("q", Value::Bytes(be(q))),
        ("nonce", Value::Bytes(nonce.to_vec())),
        ("server_nonce", Value::Bytes(server_nonce.clone())),
        ("new_nonce", Value::Bytes(new_nonce.to_vec())),
    ];
    if !legacy {
        fields.push(("dc", Value::Int(dc)));
    }
    let inner = Obj::new(
        if legacy { "p_q_inner_data" } else { "p_q_inner_data_dc" },
        &fields,
    );
    let data = super::tl::encode(&inner);
    let encrypted = if legacy {
        crypto::rsa_old(&data, &modulus)
    } else {
        crypto::rsa_pad(&data, &modulus)
    };
    let req = Obj::new(
        "req_DH_params",
        &[
            ("nonce", Value::Bytes(nonce.to_vec())),
            ("server_nonce", Value::Bytes(server_nonce.clone())),
            ("p", Value::Bytes(be(p))),
            ("q", Value::Bytes(be(q))),
            ("public_key_fingerprint", Value::Long(fingerprint as i64)),
            ("encrypted_data", Value::Bytes(encrypted)),
        ],
    );
    plain_send(t, &super::tl::encode(&req), msg_id.next(offset))?;
    let answer = plain_recv(t)?;
    super::client::log(&format!("the server answered: {}", answer.name()));
    if !answer.is("server_DH_params_ok")
        || answer.bytes("nonce") != nonce
        || answer.bytes("server_nonce") != server_nonce.as_slice()
    {
        return Err(bad("the server refused our parameters"));
    }

    // 3. read the server's half
    let h1 = sha1(&[&new_nonce, &server_nonce]);
    let h2 = sha1(&[&server_nonce, &new_nonce]);
    let h3 = sha1(&[&new_nonce, &new_nonce]);
    let mut tmp_key = [0u8; 32];
    tmp_key[..20].copy_from_slice(&h1);
    tmp_key[20..].copy_from_slice(&h2[..12]);
    let mut tmp_iv = [0u8; 32];
    tmp_iv[..8].copy_from_slice(&h2[12..]);
    tmp_iv[8..28].copy_from_slice(&h3);
    tmp_iv[28..].copy_from_slice(&new_nonce[..4]);
    let enc = answer.bytes("encrypted_answer");
    if enc.len() % 16 != 0 || enc.len() < 32 {
        return Err(bad("bad encrypted answer"));
    }
    let plain = crypto::ige_decrypt(enc, &tmp_key, &tmp_iv);
    let mut r = Reader::new(&plain[20..]);
    let inner = r.obj()?;
    if sha1(&[&plain[20..20 + r.pos]]) != plain[..20] {
        return Err(bad("the answer's hash does not match"));
    }
    if !inner.is("server_DH_inner_data")
        || inner.bytes("nonce") != nonce
        || inner.bytes("server_nonce") != server_nonce.as_slice()
    {
        return Err(bad("bad server_DH_inner_data"));
    }
    let g = inner.int("g");
    if !crypto::good_prime(inner.bytes("dh_prime"), g) {
        return Err(bad("the server sent an unknown prime"));
    }
    let prime = BigUint::from_bytes_be(inner.bytes("dh_prime"));
    let g_a = BigUint::from_bytes_be(inner.bytes("g_a"));
    if !crypto::good_public(&g_a, &prime) {
        return Err(bad("bad g_a"));
    }
    let time_offset = inner.int("server_time") - now_ms() / 1000;

    // 4. send ours
    let (b, g_b) = loop {
        let b = BigUint::from_bytes_be(&crypto::random_array::<256>());
        let g_b = crypto::modpow(&BigUint::from(g as u32), &b, &prime);
        if crypto::good_public(&g_b, &prime) {
            break (b, g_b);
        }
    };
    let client_inner = Obj::new(
        "client_DH_inner_data",
        &[
            ("nonce", Value::Bytes(nonce.to_vec())),
            ("server_nonce", Value::Bytes(server_nonce.clone())),
            ("retry_id", Value::Long(0)),
            ("g_b", Value::Bytes(g_b.to_bytes_be())),
        ],
    );
    let data = super::tl::encode(&client_inner);
    let mut with_hash = sha1(&[&data]).to_vec();
    with_hash.extend_from_slice(&data);
    while !with_hash.len().is_multiple_of(16) {
        with_hash.push(crypto::random_array::<1>()[0]);
    }
    let req = Obj::new(
        "set_client_DH_params",
        &[
            ("nonce", Value::Bytes(nonce.to_vec())),
            ("server_nonce", Value::Bytes(server_nonce.clone())),
            (
                "encrypted_data",
                Value::Bytes(crypto::ige_encrypt(&with_hash, &tmp_key, &tmp_iv)),
            ),
        ],
    );
    plain_send(t, &super::tl::encode(&req), msg_id.next(time_offset))?;
    let done = plain_recv(t)?;
    super::client::log(&format!("the server answered: {}", done.name()));

    // 5. both sides now know g^ab
    let key_num = crypto::modpow(&g_a, &b, &prime);
    let key: [u8; 256] = crypto::to_bytes(&key_num, 256).try_into().unwrap();
    let aux = sha1(&[&key]);
    let expected = sha1(&[&new_nonce, &[1], &aux[..8]]);
    if !done.is("dh_gen_ok") || done.bytes("new_nonce_hash1") != &expected[4..20] {
        return Err(bad("the server did not accept the key"));
    }
    let mut salt = [0u8; 8];
    for i in 0..8 {
        salt[i] = new_nonce[i] ^ server_nonce[i];
    }
    Ok(NewKey {
        key,
        salt: i64::from_le_bytes(salt),
        time_offset,
    })
}

// ---- encrypted messages -------------------------------------------------------------------

/// Message ids: the server's time in the top 32 bits, always growing,
/// divisible by 4.
#[derive(Default)]
pub struct MsgIds {
    last: i64,
}

impl MsgIds {
    pub fn next(&mut self, time_offset: i64) -> i64 {
        let ms = now_ms() + time_offset * 1000;
        let secs = ms.div_euclid(1000);
        let frac = (ms.rem_euclid(1000) << 32) / 1000;
        let mut id = (secs << 32 | frac) & !3;
        if id <= self.last {
            id = self.last + 4;
        }
        self.last = id;
        id
    }
}

/// An encrypted session with a server.
pub struct Session {
    pub key: [u8; 256],
    key_id: [u8; 8],
    pub salt: i64,
    pub id: i64,
    seq: i32,
    ids: MsgIds,
    /// How far the server's clock is ahead of ours, in seconds.
    pub time_offset: i64,
}

impl Session {
    pub fn new(key: [u8; 256], salt: i64, time_offset: i64) -> Session {
        let h = sha1(&[&key]);
        Session {
            key,
            key_id: h[12..20].try_into().unwrap(),
            salt,
            id: crypto::random_i64(),
            seq: 0,
            ids: MsgIds::default(),
            time_offset,
        }
    }

    /// Start over with a new session id (after seqno trouble).
    pub fn reset(&mut self) {
        self.id = crypto::random_i64();
        self.seq = 0;
    }

    pub fn next_msg_id(&mut self) -> i64 {
        self.ids.next(self.time_offset)
    }

    fn next_seq(&mut self, content: bool) -> i32 {
        if content {
            self.seq += 1;
            self.seq * 2 - 1
        } else {
            self.seq * 2
        }
    }

    /// Encrypt a message. `content` is false for acks and containers.
    /// Returns its msg_id and the packet to send.
    pub fn encrypt(&mut self, body: &[u8], content: bool) -> (i64, Vec<u8>) {
        let msg_id = self.next_msg_id();
        let seq = self.next_seq(content);
        let mut w = Writer::new();
        w.i64(self.salt);
        w.i64(self.id);
        w.i64(msg_id);
        w.i32(seq);
        w.i32(body.len() as i32);
        w.raw(body);
        // 12 to 1024 bytes of padding, to a multiple of 16
        let mut pad = 12 + (16 - (w.buf.len() + 12) % 16) % 16;
        pad += (crypto::random_array::<1>()[0] as usize % 4) * 16;
        let mut padding = alloc::vec![0u8; pad];
        crypto::random(&mut padding);
        w.raw(&padding);
        let plain = w.buf;

        let msg_key_large = sha256(&[&self.key[88..120], &plain]);
        let msg_key: [u8; 16] = msg_key_large[8..24].try_into().unwrap();
        let (aes_key, aes_iv) = self.keys(&msg_key, 0);
        let mut packet = Vec::with_capacity(plain.len() + 24);
        packet.extend_from_slice(&self.key_id);
        packet.extend_from_slice(&msg_key);
        packet.extend_from_slice(&crypto::ige_encrypt(&plain, &aes_key, &aes_iv));
        (msg_id, packet)
    }

    /// Decrypt a packet from the server into (msg_id, seqno, body).
    pub fn decrypt(&self, packet: &[u8]) -> Result<(i64, i32, Vec<u8>)> {
        let broken = || Error::Other(String::from("broken encrypted message"));
        if packet.len() < 24 + 32
            || packet[..8] != self.key_id
            || !(packet.len() - 24).is_multiple_of(16)
        {
            return Err(broken());
        }
        let msg_key: [u8; 16] = packet[8..24].try_into().unwrap();
        let (aes_key, aes_iv) = self.keys(&msg_key, 8);
        let plain = crypto::ige_decrypt(&packet[24..], &aes_key, &aes_iv);
        let check = sha256(&[&self.key[96..128], &plain]);
        if check[8..24] != msg_key {
            return Err(broken());
        }
        let mut r = Reader::new(&plain);
        let _salt = r.i64()?;
        let session = r.i64()?;
        let msg_id = r.i64()?;
        let seq = r.i32()?;
        let len = r.i32()? as usize;
        if session != self.id || len > plain.len() - 32 || plain.len() - 32 - len < 12 {
            return Err(broken());
        }
        Ok((msg_id, seq, r.take(len)?.to_vec()))
    }

    /// The AES key and iv for a message, from its msg_key. `x` is 0 for
    /// messages to the server and 8 for messages from it.
    fn keys(&self, msg_key: &[u8; 16], x: usize) -> ([u8; 32], [u8; 32]) {
        let a = sha256(&[msg_key, &self.key[x..x + 36]]);
        let b = sha256(&[&self.key[40 + x..76 + x], msg_key]);
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&a[..8]);
        key[8..24].copy_from_slice(&b[8..24]);
        key[24..].copy_from_slice(&a[24..]);
        let mut iv = [0u8; 32];
        iv[..8].copy_from_slice(&b[..8]);
        iv[8..24].copy_from_slice(&a[8..24]);
        iv[24..].copy_from_slice(&b[24..]);
        (key, iv)
    }
}
