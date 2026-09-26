//! Desktop backgrounds: the pictures that come with EverOS, pictures
//! from the disk and plain colours, fitted to any screen size the way
//! Windows does it (fill, fit, stretch, center or tile).
//!
//! More built-in pictures: put the file in `kernel/assets/wallpapers/`
//! and add a line to `BUILTIN`.

use alloc::vec;
use alloc::vec::Vec;

use super::canvas::{Canvas, Color, Rect};
use super::personalize::{Background, Fit, Prefs};
use super::picture::{self, Image};
use crate::fs;

/// A picture that comes with EverOS.
pub enum Builtin {
    /// A PNG or JPEG built into the kernel.
    File(&'static [u8]),
    /// Drawn in code for the exact screen size.
    Drawn(fn(&mut Canvas)),
}

pub const BUILTIN: [(&str, Builtin); 2] = [
    (
        "Dotted wave",
        Builtin::File(include_bytes!("../../assets/wallpapers/wave.png")),
    ),
    ("Bloom", Builtin::Drawn(super::draw_wallpaper)),
];

/// Draw the background `prefs` asks for into `out` (`w` x `h`). A
/// picture that can't be read falls back to the first built-in one.
/// Returns false in that case.
pub fn render(prefs: &Prefs, out: &mut [u32], w: i32, h: i32) -> bool {
    let (ok, source) = match &prefs.background {
        Background::Solid => {
            out[..(w * h) as usize].fill(prefs.color);
            return true;
        }
        Background::Builtin(i) => (true, builtin(*i)),
        Background::Picture(path) => match load(path) {
            Some(img) => (true, Source::Image(img)),
            None => (false, builtin(0)),
        },
    };
    match source {
        Source::Image(img) => fit(&img, prefs.fit, prefs.color, out, w, h),
        Source::Drawn(draw) => draw(&mut Canvas::new(out, w as usize, h as usize)),
    }
    ok
}

/// A small picture of a background, for Settings.
pub fn thumbnail(background: &Background, prefs: &Prefs, w: i32, h: i32) -> Vec<u32> {
    let mut out = vec![prefs.color; (w * h) as usize];
    let source = match background {
        Background::Solid => return out,
        Background::Builtin(i) => builtin(*i),
        Background::Picture(path) => match load(path) {
            Some(img) => Source::Image(img),
            None => return out,
        },
    };
    match source {
        Source::Image(img) => fit(&img, prefs.fit, prefs.color, &mut out, w, h),
        Source::Drawn(draw) => {
            // drawn at a screen size and shrunk, so it looks the same
            let (bw, bh) = (w * 6, h * 6);
            let mut big = vec![0u32; (bw * bh) as usize];
            draw(&mut Canvas::new(&mut big, bw as usize, bh as usize));
            Canvas::new(&mut out, w as usize, h as usize).blit_smooth(
                Rect::new(0, 0, w, h),
                &big,
                bw,
                bh,
            );
        }
    }
    out
}

enum Source {
    Image(Image),
    Drawn(fn(&mut Canvas)),
}

fn builtin(i: usize) -> Source {
    let i = if i < BUILTIN.len() { i } else { 0 };
    match BUILTIN[i].1 {
        Builtin::File(data) => match picture::decode(data) {
            Some(img) => Source::Image(img),
            None => Source::Drawn(super::draw_wallpaper),
        },
        Builtin::Drawn(draw) => Source::Drawn(draw),
    }
}

fn load(path: &str) -> Option<Image> {
    let data = fs::read(path).ok()?;
    picture::decode(&data)
}

/// Fit a picture into `out` (`w` x `h`); `bg` shows where it doesn't
/// cover, and behind see-through pixels.
pub fn fit(img: &Image, how: Fit, bg: Color, out: &mut [u32], w: i32, h: i32) {
    let (iw, ih) = (img.width as i64, img.height as i64);
    let (w64, h64) = (w as i64, h as i64);
    out[..(w * h) as usize].fill(bg);
    if iw == 0 || ih == 0 {
        return;
    }
    let full = Rect::new(0, 0, img.width as i32, img.height as i32);
    match how {
        Fit::Fill => {
            // the part of the picture with the screen's shape
            let crop = if iw * h64 > ih * w64 {
                let cw = (ih * w64 / h64) as i32;
                Rect::new((iw as i32 - cw) / 2, 0, cw, ih as i32)
            } else {
                let ch = (iw * h64 / w64) as i32;
                Rect::new(0, (ih as i32 - ch) / 2, iw as i32, ch)
            };
            resample(img, crop, out, w, h, Rect::new(0, 0, w, h), bg);
        }
        Fit::Fit => {
            let dst = if iw * h64 > ih * w64 {
                let dh = (ih * w64 / iw) as i32;
                Rect::new(0, (h - dh) / 2, w, dh)
            } else {
                let dw = (iw * h64 / ih) as i32;
                Rect::new((w - dw) / 2, 0, dw, h)
            };
            resample(img, full, out, w, h, dst, bg);
        }
        Fit::Stretch => resample(img, full, out, w, h, Rect::new(0, 0, w, h), bg),
        Fit::Center => {
            let dst = Rect::new((w - full.w) / 2, (h - full.h) / 2, full.w, full.h);
            resample(img, full, out, w, h, dst, bg);
        }
        Fit::Tile => {
            for y in 0..h as usize {
                let sy = y % img.height;
                for x in 0..w as usize {
                    let p = img.pixels[sy * img.width + x % img.width];
                    out[y * w as usize + x] = over(p, bg);
                }
            }
        }
    }
}

/// A pixel with alpha over a colour.
fn over(p: u32, bg: Color) -> u32 {
    let a = p >> 24;
    if a >= 255 {
        p & 0xff_ffff
    } else {
        super::canvas::mix(bg, p & 0xff_ffff, a)
    }
}

/// Scale the `crop` part of `img` onto `dst` of `out` (`w` wide, `h`
/// high, clipped to it). Shrinking averages every pixel a new one
/// covers, so photos stay smooth; growing blends the four nearest
/// pixels (bilinear), so small pictures don't turn into blocks.
fn resample(img: &Image, crop: Rect, out: &mut [u32], w: i32, h: i32, dst: Rect, bg: Color) {
    if dst.w <= 0 || dst.h <= 0 || crop.w <= 0 || crop.h <= 0 {
        return;
    }
    let area = dst.intersect(&Rect::new(0, 0, w, h));
    let stride = img.width;
    let px = |x: i32, y: i32| over(img.pixels[y as usize * stride + x as usize], bg);
    if crop.w >= 2 * dst.w && crop.h >= 2 * dst.h {
        for y in area.y..area.bottom() {
            let y0 = crop.y + ((y - dst.y) as i64 * crop.h as i64 / dst.h as i64) as i32;
            let y1 = crop.y + ((y - dst.y + 1) as i64 * crop.h as i64 / dst.h as i64) as i32;
            let y1 = y1.max(y0 + 1).min(crop.bottom());
            for x in area.x..area.right() {
                let x0 = crop.x + ((x - dst.x) as i64 * crop.w as i64 / dst.w as i64) as i32;
                let x1 = crop.x + ((x - dst.x + 1) as i64 * crop.w as i64 / dst.w as i64) as i32;
                let x1 = x1.max(x0 + 1).min(crop.right());
                let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let p = px(sx, sy);
                        r += (p >> 16) & 0xff;
                        g += (p >> 8) & 0xff;
                        b += p & 0xff;
                        n += 1;
                    }
                }
                out[(y * w + x) as usize] = (r / n) << 16 | (g / n) << 8 | (b / n);
            }
        }
        return;
    }
    // source positions in 1/256 pixels, for pixel centres
    let at = |d: i32, dn: i32, c0: i32, cn: i32| -> (i32, i32, u32) {
        let f = ((2 * d as i64 + 1) * cn as i64 * 128 / dn as i64 - 128).max(0);
        let i = (f >> 8) as i32;
        let (i0, i1) = ((c0 + i).min(c0 + cn - 1), (c0 + i + 1).min(c0 + cn - 1));
        (i0, i1, (f & 0xff) as u32)
    };
    let cols: Vec<(i32, i32, u32)> = (area.x..area.right())
        .map(|x| at(x - dst.x, dst.w, crop.x, crop.w))
        .collect();
    for y in area.y..area.bottom() {
        let (y0, y1, ty) = at(y - dst.y, dst.h, crop.y, crop.h);
        for (k, x) in (area.x..area.right()).enumerate() {
            let (x0, x1, tx) = cols[k];
            let top = lerp_px(px(x0, y0), px(x1, y0), tx);
            let bottom = lerp_px(px(x0, y1), px(x1, y1), tx);
            out[(y * w + x) as usize] = lerp_px(top, bottom, ty);
        }
    }
}

fn lerp_px(a: u32, b: u32, t: u32) -> u32 {
    if t == 0 || a == b {
        return a;
    }
    let ch = |s: u32| {
        let (x, y) = ((a >> s) & 0xff, (b >> s) & 0xff);
        ((x * (256 - t) + y * t) >> 8) << s
    };
    ch(16) | ch(8) | ch(0)
}
