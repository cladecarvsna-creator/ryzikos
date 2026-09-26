//! Picture files: reading PNG, JPEG and BMP (the first two with the
//! browser's decoders in web/image.rs), and writing PNG and BMP for
//! Paint. PNGs are compressed with a small deflate: LZ77 matches coded
//! with the fixed Huffman tables, which shrinks drawings a lot.

use alloc::vec;
use alloc::vec::Vec;

pub use crate::web::image::Image;

/// Read a picture file from its bytes. Pixels are 0xAARRGGBB.
pub fn decode(data: &[u8]) -> Option<Image> {
    if data.starts_with(b"BM") {
        decode_bmp(data)
    } else {
        crate::web::image::decode(data)
    }
}

/// Whether a file name looks like a picture EverOS can show.
pub fn is_picture(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".bmp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// An uncompressed 24 or 32 bit BMP, rows bottom up or top down.
fn decode_bmp(data: &[u8]) -> Option<Image> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(data.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?));
    let offset = u32_at(10)? as usize;
    let w = u32_at(18)? as i32;
    let h_raw = u32_at(22)? as i32;
    let bpp = u16_at(28)? as usize;
    let compression = u32_at(30)?;
    if w <= 0 || h_raw == 0 || !(bpp == 24 || bpp == 32) || !(compression == 0 || compression == 3)
    {
        return None;
    }
    let (w, h) = (w as usize, h_raw.unsigned_abs() as usize);
    if w * h > 12 * 1024 * 1024 {
        return None;
    }
    let stride = (w * bpp / 8 + 3) & !3;
    let mut pixels = vec![0u32; w * h];
    for y in 0..h {
        let row = if h_raw > 0 { h - 1 - y } else { y };
        let line = data.get(offset + row * stride..offset + row * stride + w * bpp / 8)?;
        for x in 0..w {
            let p = &line[x * bpp / 8..];
            pixels[y * w + x] =
                0xff00_0000 | (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32;
        }
    }
    Some(Image {
        width: w,
        height: h,
        pixels,
    })
}

/// A 24-bit BMP of 0xRRGGBB pixels.
pub fn encode_bmp(pixels: &[u32], w: usize, h: usize) -> Vec<u8> {
    let stride = (w * 3 + 3) & !3;
    let size = 54 + stride * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&((stride * h) as u32).to_le_bytes());
    // 2835 pixels per metre is 72 dpi
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    for y in (0..h).rev() {
        for &p in &pixels[y * w..y * w + w] {
            out.extend_from_slice(&[p as u8, (p >> 8) as u8, (p >> 16) as u8]);
        }
        out.resize(out.len() + stride - w * 3, 0);
    }
    out
}

/// An RGB PNG of 0xRRGGBB pixels.
pub fn encode_png(pixels: &[u32], w: usize, h: usize) -> Vec<u8> {
    // every row starts with its filter: 1 ("Sub") stores each byte as the
    // difference from the pixel to its left, so flat colour becomes zeros
    let mut raw = Vec::with_capacity(h * (w * 3 + 1));
    for y in 0..h {
        raw.push(1);
        let mut left = [0u8; 3];
        for &p in &pixels[y * w..y * w + w] {
            let px = [(p >> 16) as u8, (p >> 8) as u8, p as u8];
            for k in 0..3 {
                raw.push(px[k].wrapping_sub(left[k]));
            }
            left = px;
        }
    }
    let mut zlib = vec![0x78, 0x01];
    zlib.extend_from_slice(&deflate(&raw));
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::with_capacity(zlib.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    // 8 bits per channel, RGB, deflate, no interlace
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

/// Writes bits the way deflate wants them: least significant first.
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, count: u32) {
        self.acc |= (value as u64) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code, which deflate stores most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// A literal byte or the end of the block (256) in the fixed code.
fn literal(bits: &mut Bits, v: u32) {
    match v {
        0..=143 => bits.code(0x30 + v, 8),
        144..=255 => bits.code(0x190 + v - 144, 9),
        256..=279 => bits.code(v - 256, 7),
        _ => bits.code(0xc0 + v - 280, 8),
    }
}

fn copy(bits: &mut Bits, len: usize, dist: usize) {
    let i = LEN_BASE
        .iter()
        .rposition(|&b| b as usize <= len)
        .unwrap_or(0);
    literal(bits, 257 + i as u32);
    bits.put((len - LEN_BASE[i] as usize) as u32, LEN_EXTRA[i] as u32);
    let d = DIST_BASE
        .iter()
        .rposition(|&b| b as usize <= dist)
        .unwrap_or(0);
    bits.code(d as u32, 5);
    bits.put((dist - DIST_BASE[d] as usize) as u32, DIST_EXTRA[d] as u32);
}

/// Compress into one deflate block with the fixed Huffman code.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    const WINDOW: usize = 32 * 1024;
    const HASH_BITS: u32 = 15;
    const TRIES: usize = 24;
    let mut bits = Bits {
        out: Vec::with_capacity(data.len() / 4 + 16),
        acc: 0,
        n: 0,
    };
    // the last block, fixed Huffman codes
    bits.put(1, 1);
    bits.put(1, 2);
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; WINDOW];
    let hash = |i: usize| {
        let v = (data[i] as u32) << 16 | (data[i + 1] as u32) << 8 | data[i + 2] as u32;
        (v.wrapping_mul(0x9e37_79b1) >> (32 - HASH_BITS)) as usize
    };
    let insert = |i: usize, head: &mut Vec<usize>, prev: &mut Vec<usize>| {
        if i + 2 < data.len() {
            let h = hash(i);
            prev[i % WINDOW] = head[h];
            head[h] = i;
        }
    };
    let mut i = 0;
    while i < data.len() {
        let mut best = (0usize, 0usize);
        if i + 2 < data.len() {
            let mut cand = head[hash(i)];
            let limit = (data.len() - i).min(258);
            for _ in 0..TRIES {
                if cand == usize::MAX || i - cand > WINDOW - 1 || cand >= i {
                    break;
                }
                let mut n = 0;
                while n < limit && data[cand + n] == data[i + n] {
                    n += 1;
                }
                if n > best.0 {
                    best = (n, i - cand);
                    if n == limit {
                        break;
                    }
                }
                cand = prev[cand % WINDOW];
            }
        }
        if best.0 >= 3 {
            copy(&mut bits, best.0, best.1);
            for k in i..i + best.0 {
                insert(k, &mut head, &mut prev);
            }
            i += best.0;
        } else {
            literal(&mut bits, data[i] as u32);
            insert(i, &mut head, &mut prev);
            i += 1;
        }
    }
    literal(&mut bits, 256);
    bits.finish()
}
