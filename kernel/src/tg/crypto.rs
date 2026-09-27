//! The cryptography MTProto needs: SHA-1/256/512, AES-256 in IGE mode,
//! RSA with Telegram's public keys, Diffie-Hellman, factoring the small
//! number the server sends, and SRP for two-step verification passwords.
//! Big numbers come from num-bigint; everything else is here.

use alloc::vec::Vec;

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes256;
use num_bigint::BigUint;
use num_integer::Integer;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::sync::IrqMutex;

// ---- hashes -------------------------------------------------------------------

pub fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn hmac_sha512(key: &[u8], parts: &[&[u8]]) -> [u8; 64] {
    let mut k = [0u8; 128];
    if key.len() > 128 {
        k[..64].copy_from_slice(&Sha512::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha512::new();
    inner.update(k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let mut outer = Sha512::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// PBKDF2 with HMAC-SHA512, one 64-byte block.
pub fn pbkdf2_sha512(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 64] {
    let mut u = hmac_sha512(password, &[salt, &1u32.to_be_bytes()]);
    let mut out = u;
    for _ in 1..rounds {
        u = hmac_sha512(password, &[&u]);
        for (o, b) in out.iter_mut().zip(u.iter()) {
            *o ^= b;
        }
    }
    out
}

// ---- random numbers ---------------------------------------------------------------

/// Whether the CPU has the RDRAND instruction.
fn has_rdrand() -> bool {
    let ecx = core::arch::x86_64::__cpuid(1).ecx;
    ecx & (1 << 30) != 0
}

fn rdrand() -> Option<u64> {
    let mut v = 0u64;
    let ok = unsafe { core::arch::x86_64::_rdrand64_step(&mut v) };
    (ok == 1).then_some(v)
}

/// Random bytes: SHA-256 over a counter, the CPU's random number
/// generator when it has one, and the jitter of the time stamp counter.
pub fn random(buf: &mut [u8]) {
    use core::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static SEED: IrqMutex<[u8; 32]> = IrqMutex::new([0; 32]);
    let rdrand_ok = has_rdrand();
    if COUNTER.fetch_add(1, Ordering::Relaxed) == 0 {
        // gather the seed once, from timing jitter
        let mut h = Sha256::new();
        for _ in 0..256 {
            h.update(unsafe { core::arch::x86_64::_rdtsc() }.to_le_bytes());
            h.update(crate::interrupts::ticks().to_le_bytes());
            if rdrand_ok {
                h.update(rdrand().unwrap_or(0).to_le_bytes());
            }
            for _ in 0..50 {
                core::hint::spin_loop();
            }
        }
        *SEED.lock() = h.finalize().into();
    }
    let seed = *SEED.lock();
    for chunk in buf.chunks_mut(32) {
        let mut h = Sha256::new();
        h.update(seed);
        h.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        h.update(unsafe { core::arch::x86_64::_rdtsc() }.to_le_bytes());
        if rdrand_ok {
            h.update(rdrand().unwrap_or(0).to_le_bytes());
        }
        let out = h.finalize();
        chunk.copy_from_slice(&out[..chunk.len()]);
    }
}

pub fn random_array<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    random(&mut b);
    b
}

pub fn random_i64() -> i64 {
    i64::from_le_bytes(random_array())
}

// ---- AES-256 in IGE mode ------------------------------------------------------------

/// Encrypt whole 16-byte blocks. IGE chains each block with both the
/// previous plain and cipher text; the 32-byte iv holds both to start.
pub fn ige_encrypt(data: &[u8], key: &[u8; 32], iv: &[u8; 32]) -> Vec<u8> {
    assert!(data.len().is_multiple_of(16), "ige: not whole blocks");
    let aes = Aes256::new(key.into());
    let mut prev_c: [u8; 16] = iv[..16].try_into().unwrap();
    let mut prev_p: [u8; 16] = iv[16..].try_into().unwrap();
    let mut out = Vec::with_capacity(data.len());
    for block in data.chunks(16) {
        let mut b = [0u8; 16];
        for i in 0..16 {
            b[i] = block[i] ^ prev_c[i];
        }
        let mut ga = b.into();
        aes.encrypt_block(&mut ga);
        let mut c: [u8; 16] = ga.into();
        for i in 0..16 {
            c[i] ^= prev_p[i];
        }
        out.extend_from_slice(&c);
        prev_c = c;
        prev_p = block.try_into().unwrap();
    }
    out
}

pub fn ige_decrypt(data: &[u8], key: &[u8; 32], iv: &[u8; 32]) -> Vec<u8> {
    assert!(data.len().is_multiple_of(16), "ige: not whole blocks");
    let aes = Aes256::new(key.into());
    let mut prev_c: [u8; 16] = iv[..16].try_into().unwrap();
    let mut prev_p: [u8; 16] = iv[16..].try_into().unwrap();
    let mut out = Vec::with_capacity(data.len());
    for block in data.chunks(16) {
        let mut b = [0u8; 16];
        for i in 0..16 {
            b[i] = block[i] ^ prev_p[i];
        }
        let mut ga = b.into();
        aes.decrypt_block(&mut ga);
        let mut p: [u8; 16] = ga.into();
        for i in 0..16 {
            p[i] ^= prev_c[i];
        }
        out.extend_from_slice(&p);
        prev_p = p;
        prev_c = block.try_into().unwrap();
    }
    out
}

// ---- factoring pq -------------------------------------------------------------------------

fn mul_mod(a: u64, b: u64, m: u64) -> u64 {
    ((a as u128 * b as u128) % m as u128) as u64
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Split the product of two primes the server sends (Pollard's rho with
/// Brent's cycle finding). Returns the smaller factor first.
pub fn factor(pq: u64) -> Option<(u64, u64)> {
    if pq < 4 {
        return None;
    }
    if pq.is_multiple_of(2) {
        return Some((2, pq / 2));
    }
    for c in 1..64u64 {
        let f = |x: u64| (mul_mod(x, x, pq) + c) % pq;
        let (mut y, m) = (2u64, 128u64);
        let (mut g, mut r, mut q) = (1u64, 1u64, 1u64);
        let (mut x, mut ys) = (0u64, 0u64);
        while g == 1 {
            x = y;
            for _ in 0..r {
                y = f(y);
            }
            let mut k = 0;
            while k < r && g == 1 {
                ys = y;
                for _ in 0..m.min(r - k) {
                    y = f(y);
                    q = mul_mod(q, x.abs_diff(y), pq);
                }
                g = gcd(q, pq);
                k += m;
            }
            r *= 2;
        }
        if g == pq {
            loop {
                ys = f(ys);
                g = gcd(x.abs_diff(ys), pq);
                if g > 1 {
                    break;
                }
            }
        }
        if g != pq && g > 1 {
            let (p, q) = (g, pq / g);
            return Some((p.min(q), p.max(q)));
        }
    }
    None
}

// ---- RSA ----------------------------------------------------------------------------------

include!("keys.rs");

/// The modulus of the server key with this fingerprint.
pub fn server_key(fingerprint: u64, extra: &[(u64, Vec<u8>)]) -> Option<Vec<u8>> {
    if let Some((_, n)) = extra.iter().find(|(f, _)| *f == fingerprint) {
        return Some(n.clone());
    }
    SERVER_KEYS
        .iter()
        .find(|(f, _)| *f == fingerprint)
        .map(|(_, hex)| from_hex(hex))
}

pub fn from_hex(hex: &str) -> Vec<u8> {
    let digit = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
    hex.as_bytes()
        .chunks(2)
        .map(|p| digit(p[0]) << 4 | digit(*p.get(1).unwrap_or(&b'0')))
        .collect()
}

/// A key's fingerprint: the low 64 bits of SHA-1 over the modulus and
/// the exponent as TL bytes.
pub fn fingerprint(modulus: &[u8]) -> u64 {
    let mut w = super::tl::Writer::new();
    w.bytes(modulus);
    w.bytes(&[1, 0, 1]);
    let h = sha1(&[&w.buf]);
    u64::from_le_bytes(h[12..20].try_into().unwrap())
}

/// Big-endian bytes, left-padded to `len`.
pub fn to_bytes(n: &BigUint, len: usize) -> Vec<u8> {
    let b = n.to_bytes_be();
    let mut out = alloc::vec![0u8; len.saturating_sub(b.len())];
    out.extend_from_slice(&b);
    out
}

/// RSA_PAD from the MTProto 2.0 key exchange: `data` (at most 144 bytes)
/// gets random padding, a hash and a layer of AES under a random key,
/// then the whole 256 bytes go through RSA.
pub fn rsa_pad(data: &[u8], modulus: &[u8]) -> Vec<u8> {
    let n = BigUint::from_bytes_be(modulus);
    let e = BigUint::from(65537u32);
    let mut padded = [0u8; 192];
    padded[..data.len()].copy_from_slice(data);
    random(&mut padded[data.len()..]);
    let mut reversed = padded;
    reversed.reverse();
    loop {
        let temp_key: [u8; 32] = random_array();
        let mut with_hash = Vec::with_capacity(224);
        with_hash.extend_from_slice(&reversed);
        with_hash.extend_from_slice(&sha256(&[&temp_key, &padded]));
        let aes = ige_encrypt(&with_hash, &temp_key, &[0; 32]);
        let aes_hash = sha256(&[&aes]);
        let mut key_aes = Vec::with_capacity(256);
        for i in 0..32 {
            key_aes.push(temp_key[i] ^ aes_hash[i]);
        }
        key_aes.extend_from_slice(&aes);
        let m = BigUint::from_bytes_be(&key_aes);
        if m < n {
            return to_bytes(&m.modpow(&e, &n), 256);
        }
    }
}

// ---- Diffie-Hellman --------------------------------------------------------------------

/// The 2048-bit safe prime Telegram uses for Diffie-Hellman and SRP.
pub const DH_PRIME: &str = concat!(
    "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f",
    "48198a0aa7c14058229493d22530f4dbfa336f6e0ac925139543aed44cce7c37",
    "20fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18faf8c595f64",
    "2477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4",
    "a4a695811051907e162753b56b0f6b410dba74d8a84b2a14b3144e0ef1284754",
    "fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9ae4",
    "e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f",
    "0d8115f635b105ee2e4e15d04b2454bf6f4fadf034b10403119cd8e3b92fcc5b",
);

/// Whether the server's prime and generator are the ones we trust.
pub fn good_prime(prime: &[u8], g: i64) -> bool {
    prime == from_hex(DH_PRIME).as_slice() && (2..=7).contains(&g)
}

/// Whether a public value is safely inside (2^1984, p - 2^1984).
pub fn good_public(x: &BigUint, p: &BigUint) -> bool {
    let low = BigUint::from(1u32) << (2048 - 64);
    x > &low && x < p && (p - x) > low
}

pub fn modpow(base: &BigUint, exp: &BigUint, m: &BigUint) -> BigUint {
    base.modpow(exp, m)
}

// ---- SRP (two-step verification) -----------------------------------------------------

/// The proof of the password for `auth.checkPassword`: (A, M1), from the
/// algorithm's salts, g and p, the server's B and our random `a`.
#[allow(clippy::too_many_arguments)]
pub fn srp(
    password: &str,
    salt1: &[u8],
    salt2: &[u8],
    g: u32,
    p_bytes: &[u8],
    b_bytes: &[u8],
    a_random: &[u8; 256],
) -> Result<([u8; 256], [u8; 32]), &'static str> {
    if !good_prime(p_bytes, g as i64) {
        return Err("the server sent an unknown prime");
    }
    let hash1 = sha256(&[salt1, password.as_bytes(), salt1]);
    let hash2 = sha256(&[salt2, &hash1, salt2]);
    let hash3 = pbkdf2_sha512(&hash2, salt1, 100_000);
    let x_hash = sha256(&[salt2, &hash3, salt2]);

    let p = BigUint::from_bytes_be(p_bytes);
    let g_big = BigUint::from(g);
    let big_b = BigUint::from_bytes_be(b_bytes);
    if big_b == BigUint::from(0u32) || big_b >= p {
        return Err("the server sent a bad B");
    }
    let x = BigUint::from_bytes_be(&x_hash);
    let p_hash = to_bytes(&p, 256);
    let g_hash = to_bytes(&g_big, 256);
    let b_hash = to_bytes(&big_b, 256);
    let g_x = g_big.modpow(&x, &p);
    let k = BigUint::from_bytes_be(&sha256(&[&p_hash, &g_hash]));
    let kg_x = (k * g_x) % &p;

    let a = BigUint::from_bytes_be(a_random);
    let big_a = g_big.modpow(&a, &p);
    if !good_public(&big_a, &p) {
        return Err("bad random number, try again");
    }
    let a_hash = to_bytes(&big_a, 256);
    let u = BigUint::from_bytes_be(&sha256(&[&a_hash, &b_hash]));
    let g_b = (big_b + &p - kg_x).mod_floor(&p);
    if !good_public(&g_b, &p) {
        return Err("the server sent a bad B");
    }
    let s = g_b.modpow(&(a + u * x), &p);
    let key = sha256(&[&to_bytes(&s, 256)]);
    let hp = sha256(&[&p_hash]);
    let hg = sha256(&[&g_hash]);
    let mut xor = [0u8; 32];
    for i in 0..32 {
        xor[i] = hp[i] ^ hg[i];
    }
    let m1 = sha256(&[
        &xor,
        &sha256(&[salt1]),
        &sha256(&[salt2]),
        &a_hash,
        &b_hash,
        &key,
    ]);
    Ok((a_hash.try_into().unwrap(), m1))
}

pub const SERVER_KEY_COUNT: usize = SERVER_KEYS.len();

/// One of Telegram's keys: (fingerprint, modulus).
pub fn server_key_at(i: usize) -> (u64, Vec<u8>) {
    (SERVER_KEYS[i].0, from_hex(SERVER_KEYS[i].1))
}
