//! Images for web pages: PNG and JPEG, decoded with zune-png and
//! zune-jpeg into 32-bit ARGB pixels.

use alloc::vec::Vec;

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;

/// Bigger images are skipped: they would not fit in memory.
const MAX_PIXELS: usize = 12 * 1024 * 1024;

pub struct Image {
    pub width: usize,
    pub height: usize,
    /// 0xAARRGGBB, row by row.
    pub pixels: Vec<u32>,
}

impl Image {
    pub fn empty() -> Image {
        Image {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        }
    }

    /// The colour at a point of the image, averaging a `step` x `step`
    /// block when the image is drawn smaller than it is.
    pub fn sample(&self, x: usize, y: usize, step: usize) -> u32 {
        if step <= 1 {
            return self.pixels[y.min(self.height - 1) * self.width + x.min(self.width - 1)];
        }
        let (mut a, mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
        let step = step.min(4);
        for dy in 0..step {
            for dx in 0..step {
                let px = (x + dx).min(self.width - 1);
                let py = (y + dy).min(self.height - 1);
                let p = self.pixels[py * self.width + px];
                a += p >> 24;
                r += (p >> 16) & 255;
                g += (p >> 8) & 255;
                b += p & 255;
                n += 1;
            }
        }
        (a / n) << 24 | (r / n) << 16 | (g / n) << 8 | (b / n)
    }
}

/// Decode PNG or JPEG data.
pub fn decode(data: &[u8]) -> Option<Image> {
    if data.starts_with(b"\x89PNG") {
        decode_png(data)
    } else if data.starts_with(&[0xff, 0xd8]) {
        decode_jpeg(data)
    } else {
        None
    }
}

fn options() -> DecoderOptions {
    DecoderOptions::default()
        .set_max_width(8192)
        .set_max_height(8192)
}

fn decode_png(data: &[u8]) -> Option<Image> {
    let opts = options()
        .png_set_strip_to_8bit(true)
        .png_set_add_alpha_channel(true);
    let mut d = zune_png::PngDecoder::new_with_options(ZCursor::new(data), opts);
    d.decode_headers().ok()?;
    let (w, h) = d.dimensions()?;
    if w * h > MAX_PIXELS || w == 0 || h == 0 {
        return None;
    }
    let raw = d.decode().ok()?.u8()?;
    let channels = raw.len() / (w * h);
    let mut pixels = Vec::with_capacity(w * h);
    for p in raw.chunks_exact(channels.max(1)).take(w * h) {
        let (r, g, b, a) = match channels {
            1 => (p[0], p[0], p[0], 255),
            2 => (p[0], p[0], p[0], p[1]),
            3 => (p[0], p[1], p[2], 255),
            _ => (p[0], p[1], p[2], p[3]),
        };
        pixels.push((a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32);
    }
    Some(Image {
        width: w,
        height: h,
        pixels,
    })
}

fn decode_jpeg(data: &[u8]) -> Option<Image> {
    let opts = options().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(data), opts);
    d.decode_headers().ok()?;
    let (w, h) = d.dimensions()?;
    if w * h > MAX_PIXELS || w == 0 || h == 0 {
        return None;
    }
    let raw = d.decode().ok()?;
    let mut pixels = Vec::with_capacity(w * h);
    for &[r, g, b] in raw.as_chunks::<3>().0.iter().take(w * h) {
        pixels.push(0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32);
    }
    if pixels.len() < w * h {
        pixels.resize(w * h, 0xffff_ffff);
    }
    Some(Image {
        width: w,
        height: h,
        pixels,
    })
}

/// Decode a `data:` address (after the `data:` part).
pub fn decode_data_url(rest: &str) -> Option<Image> {
    let (meta, payload) = rest.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    decode(&base64(payload))
}

fn base64(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut bits = 0u32;
    let mut n = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        };
        bits = bits << 6 | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    out
}
