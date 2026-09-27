//! App icons. Most come from the 150x150 pictures in
//! `kernel/assets/icons/`: BMPs have their white background made
//! transparent, PNGs bring their own transparency. They are cropped to
//! the picture and shrunk with alpha to 48, 24 and 16 pixels. Apps
//! without a picture are drawn with shapes at 48 pixels and shrunk the
//! same way.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicPtr, Ordering};

use super::canvas::{mix, rgb, Canvas, Rect};
use super::{App, APPS};
use crate::serial;

/// Pictures that are not apps, for File Explorer.
#[derive(Clone, Copy)]
pub enum Pic {
    Computer,
    Drives,
    BinEmpty,
    BinFull,
}

/// Icon sizes, in pixels.
pub const LARGE: usize = 48;
pub const MEDIUM: usize = 24;
pub const SMALL: usize = 16;
const SIZES: [usize; 3] = [LARGE, MEDIUM, SMALL];

/// One picture at every size, as alpha << 24 | RGB.
struct Set([Vec<u32>; 3]);

pub struct Icons {
    apps: Vec<Set>,
    pics: [Set; 4],
    /// The RyzikOS logo at each of LOGO_SIZES.
    logo: Vec<(usize, Vec<u32>)>,
}

/// Sizes the RyzikOS logo is made at: the boot screen, About, the dock,
/// the launcher and the menu bar.
pub const LOGO_SIZES: [usize; 5] = [128, 64, 48, 32, 20];

static ICONS: AtomicPtr<Icons> = AtomicPtr::new(core::ptr::null_mut());

/// The icons, made the first time they are asked for.
pub fn get() -> &'static Icons {
    let p = ICONS.load(Ordering::Acquire);
    if !p.is_null() {
        return unsafe { &*p };
    }
    let p = Box::into_raw(Box::new(Icons::new()));
    ICONS.store(p, Ordering::Release);
    unsafe { &*p }
}

/// The picture file of an app's icon, BMP or PNG.
fn file(app: App) -> Option<&'static [u8]> {
    Some(match app {
        App::Terminal => include_bytes!("../../assets/icons/terminal.bmp"),
        App::Explorer => include_bytes!("../../assets/icons/explorer.bmp"),
        App::Notepad => include_bytes!("../../assets/icons/notepad.bmp"),
        App::Paint => include_bytes!("../../assets/icons/paint.bmp"),
        App::Settings => include_bytes!("../../assets/icons/settings.bmp"),
        App::About => include_bytes!("../../assets/icons/about.bmp"),
        App::Calculator => include_bytes!("../../assets/icons/calculator.png"),
        App::Browser => include_bytes!("../../assets/icons/browser.png"),
        App::Photos | App::Video => return None,
    })
}

impl Icons {
    fn new() -> Self {
        let mut loaded = 0;
        let apps = APPS
            .iter()
            .map(|&app| match file(app).and_then(Picture::from_file) {
                Some(p) => {
                    loaded += 1;
                    p.set()
                }
                None => {
                    // pixels left at the marker value are transparent
                    let mut big = vec![TRANSPARENT; 48 * 48];
                    draw_icon(&mut Canvas::new(&mut big, 48, 48), app, 0, 0);
                    Picture::from_drawing(&big, 48).set()
                }
            })
            .collect();
        let mut pic = |data: &[u8]| match Picture::from_file(data) {
            Some(p) => {
                loaded += 1;
                p.set()
            }
            None => Set::empty(),
        };
        let pics = [
            pic(include_bytes!("../../assets/icons/computer.bmp")),
            pic(include_bytes!("../../assets/icons/drives.bmp")),
            pic(include_bytes!("../../assets/icons/recycle-bin.png")),
            recycle_full(include_bytes!("../../assets/icons/recycle-bin.png")),
        ];
        // not counted: the boot test knows how many app pictures there are
        let logo = match Picture::from_file(include_bytes!("../../assets/ryzikos-logo.png")) {
            Some(p) => LOGO_SIZES.iter().map(|&n| (n, p.shrink(n))).collect(),
            None => Vec::new(),
        };
        // for the boot test
        let mut line = crate::StackString::<40>::new();
        let _ = write!(line, "\nicons: loaded {} pictures\n", loaded);
        serial::write_str(line.as_str());
        Icons { apps, pics, logo }
    }

    fn app(&self, app: App, size: usize) -> &[u32] {
        self.apps[app.index()].get(size)
    }

    /// Draw an app's icon, `size` pixels square (LARGE, MEDIUM or SMALL).
    pub fn draw(&self, c: &mut Canvas, app: App, size: usize, x: i32, y: i32) {
        c.blit_alpha(x, y, size as i32, size as i32, self.app(app, size));
    }

    pub fn draw_large(&self, c: &mut Canvas, app: App, x: i32, y: i32) {
        self.draw(c, app, LARGE, x, y);
    }

    pub fn draw_medium(&self, c: &mut Canvas, app: App, x: i32, y: i32) {
        self.draw(c, app, MEDIUM, x, y);
    }

    pub fn draw_small(&self, c: &mut Canvas, app: App, x: i32, y: i32) {
        self.draw(c, app, SMALL, x, y);
    }

    /// Draw the RyzikOS logo `size` pixels square (one of LOGO_SIZES).
    pub fn draw_logo(&self, c: &mut Canvas, size: usize, x: i32, y: i32) {
        self.draw_logo_faded(c, size, x, y, 256);
    }

    /// The logo faded in from black by `fade` (0 to 256).
    pub fn draw_logo_faded(&self, c: &mut Canvas, size: usize, x: i32, y: i32, fade: u32) {
        let Some((n, pixels)) = self.logo.iter().find(|(n, _)| *n == size) else {
            return;
        };
        let n = *n as i32;
        if fade >= 256 {
            c.blit_alpha(x, y, n, n, pixels);
            return;
        }
        let dim: Vec<u32> = pixels
            .iter()
            .map(|&p| (p & 0xff00_0000) | mix(0, p & 0xff_ffff, fade.min(255)))
            .collect();
        c.blit_alpha(x, y, n, n, &dim);
    }

    pub fn draw_pic(&self, c: &mut Canvas, pic: Pic, size: usize, x: i32, y: i32) {
        let pixels = self.pics[pic as usize].get(size);
        c.blit_alpha(x, y, size as i32, size as i32, pixels);
    }
}

impl Set {
    fn empty() -> Self {
        Set(SIZES.map(|s| vec![0; s * s]))
    }

    fn get(&self, size: usize) -> &[u32] {
        let i = SIZES.iter().position(|&s| s == size).unwrap_or(0);
        &self.0[i]
    }
}

const TRANSPARENT: u32 = 0xffc8_c8c8;

/// The full Recycle Bin: the bin picture with crumpled paper sticking
/// out from under its lid.
fn recycle_full(data: &[u8]) -> Set {
    let Some(img) = super::picture::decode(data) else {
        return Set::empty();
    };
    let (w, h) = (img.width, img.height);
    // the paper, drawn first so the bin covers its lower part
    let mut paper = vec![TRANSPARENT; w * h];
    {
        let mut c = Canvas::new(&mut paper, w, h);
        let (sx, sy) = (w as i32, h as i32);
        let p = |x: i32, y: i32| (x * sx / 150, y * sy / 150);
        let outline = rgb(0x2a, 0x2a, 0x30);
        c.fill_polygon(&[p(38, 34), p(60, 6), p(84, 16), p(80, 40)], outline);
        c.fill_polygon(&[p(42, 32), p(61, 10), p(80, 19), p(77, 36)], 0xf4f4f0);
        c.fill_polygon(&[p(70, 36), p(96, 4), p(118, 22), p(106, 40)], outline);
        c.fill_polygon(&[p(74, 34), p(96, 8), p(114, 23), p(103, 36)], 0xfffdf2);
        c.fill_polygon(&[p(88, 18), p(98, 10), p(104, 20)], rgb(0xd8, 0xd6, 0xcc));
    }
    let pixels: Vec<[u32; 4]> = img
        .pixels
        .iter()
        .zip(paper.iter())
        .map(|(&p, &under)| {
            let a = p >> 24;
            let rgb_of = |c: u32| [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff];
            if under == TRANSPARENT || a >= 255 {
                let c = rgb_of(p);
                [c[0], c[1], c[2], a]
            } else {
                // the bin over the paper
                let (top, back) = (rgb_of(p), rgb_of(under));
                let m = |k: usize| (top[k] * a + back[k] * (255 - a)) / 255;
                [m(0), m(1), m(2), 255]
            }
        })
        .collect();
    let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if pixels[y * w + x][3] > 8 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    if right <= left {
        return Set::empty();
    }
    Picture::square(&pixels, w, h, (left, top, right, bottom)).set()
}

/// A square picture with straight (not premultiplied) alpha.
struct Picture {
    size: usize,
    /// Red, green, blue and alpha, 0 to 255.
    pixels: Vec<[u32; 4]>,
}

impl Picture {
    fn from_drawing(pixels: &[u32], size: usize) -> Self {
        let pixels = pixels
            .iter()
            .map(|&p| {
                if p == TRANSPARENT {
                    [0; 4]
                } else {
                    [(p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff, 255]
                }
            })
            .collect();
        Picture { size, pixels }
    }

    fn from_file(data: &[u8]) -> Option<Self> {
        if data.starts_with(b"\x89PNG") {
            Self::from_png(data)
        } else {
            Self::from_bmp(data)
        }
    }

    /// A PNG with its own transparency, cropped to what is not clear.
    fn from_png(data: &[u8]) -> Option<Self> {
        let img = super::picture::decode(data)?;
        let (w, h) = (img.width, img.height);
        let pixels: Vec<[u32; 4]> = img
            .pixels
            .iter()
            .map(|&p| [(p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff, p >> 24])
            .collect();
        let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if pixels[y * w + x][3] > 8 {
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x + 1);
                    bottom = bottom.max(y + 1);
                }
            }
        }
        if right <= left {
            return None;
        }
        Some(Self::square(&pixels, w, h, (left, top, right, bottom)))
    }

    /// Crop `pixels` to a square around `bounds` (left, top, right,
    /// bottom), with a thin margin.
    fn square(
        pixels: &[[u32; 4]],
        w: usize,
        h: usize,
        (left, top, right, bottom): (usize, usize, usize, usize),
    ) -> Self {
        let side = (right - left).max(bottom - top);
        let side = side + side / 24 * 2;
        let (cx, cy) = ((left + right) / 2, (top + bottom) / 2);
        let (ox, oy) = (cx as i32 - side as i32 / 2, cy as i32 - side as i32 / 2);
        let mut square = vec![[0u32; 4]; side * side];
        for y in 0..side {
            for x in 0..side {
                let (sx, sy) = (ox + x as i32, oy + y as i32);
                if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                    square[y * side + x] = pixels[sy as usize * w + sx as usize];
                }
            }
        }
        Picture {
            size: side,
            pixels: square,
        }
    }

    /// Read an uncompressed 24 or 32 bit BMP. The white around the
    /// picture becomes transparent, and the soft edge between it and the
    /// dark outline becomes partly transparent outline.
    fn from_bmp(data: &[u8]) -> Option<Self> {
        let u16_at = |o: usize| Some(u16::from_le_bytes(data.get(o..o + 2)?.try_into().ok()?));
        let u32_at = |o: usize| Some(u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?));
        if data.get(0..2)? != b"BM" {
            return None;
        }
        let offset = u32_at(10)? as usize;
        let w = u32_at(18)? as i32 as usize;
        let h_raw = u32_at(22)? as i32;
        let bpp = u16_at(28)? as usize;
        let compression = u32_at(30)?;
        if !(bpp == 24 || bpp == 32) || compression != 0 && compression != 3 || w == 0 {
            return None;
        }
        let h = h_raw.unsigned_abs() as usize;
        let stride = (w * bpp / 8 + 3) & !3;
        let mut rgb = vec![[0u32; 3]; w * h];
        for y in 0..h {
            // rows are stored bottom up unless the height is negative
            let row = if h_raw > 0 { h - 1 - y } else { y };
            let line = data.get(offset + row * stride..offset + row * stride + w * bpp / 8)?;
            for x in 0..w {
                let p = &line[x * bpp / 8..];
                rgb[y * w + x] = [p[2] as u32, p[1] as u32, p[0] as u32];
            }
        }

        // flood the near-white background from the border
        let white = |p: [u32; 3]| p.iter().all(|&c| c >= 245);
        let mut background = vec![false; w * h];
        let mut stack = Vec::new();
        for x in 0..w {
            stack.push((x, 0));
            stack.push((x, h - 1));
        }
        for y in 0..h {
            stack.push((0, y));
            stack.push((w - 1, y));
        }
        while let Some((x, y)) = stack.pop() {
            let i = y * w + x;
            if background[i] || !white(rgb[i]) {
                continue;
            }
            background[i] = true;
            if x > 0 {
                stack.push((x - 1, y));
            }
            if x + 1 < w {
                stack.push((x + 1, y));
            }
            if y > 0 {
                stack.push((x, y - 1));
            }
            if y + 1 < h {
                stack.push((x, y + 1));
            }
        }

        // how dark the outline is: the darkest pixel next to the background
        let near = |x: usize, y: usize, reach: usize| {
            let (x0, y0) = (x.saturating_sub(reach), y.saturating_sub(reach));
            let (x1, y1) = ((x + reach).min(w - 1), (y + reach).min(h - 1));
            (y0..=y1).any(|yy| (x0..=x1).any(|xx| background[yy * w + xx]))
        };
        let luma = |p: [u32; 3]| (p[0] * 3 + p[1] * 6 + p[2]) / 10;
        let mut darkest = 256;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if !background[i] && near(x, y, 3) && luma(rgb[i]) < darkest {
                    darkest = luma(rgb[i]);
                }
            }
        }

        // straight alpha; edge pixels are the outline blended with white
        let mut pixels = vec![[0u32; 4]; w * h];
        let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if background[i] {
                    continue;
                }
                let p = rgb[i];
                let alpha = if near(x, y, 1) && darkest < 200 {
                    let a = (255 - luma(p)) * 255 / (255 - darkest);
                    a.min(255)
                } else {
                    255
                };
                if alpha == 0 {
                    continue;
                }
                pixels[i] = if alpha == 255 {
                    [p[0], p[1], p[2], 255]
                } else {
                    // take the white back out: p = a * ink + (1 - a) * white
                    let un = |c: u32| {
                        let white = (255 - alpha) as i32;
                        ((c as i32 - white) * 255 / alpha as i32).clamp(0, 255) as u32
                    };
                    [un(p[0]), un(p[1]), un(p[2]), alpha]
                };
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
        if right <= left {
            return None;
        }

        Some(Self::square(&pixels, w, h, (left, top, right, bottom)))
    }

    fn set(&self) -> Set {
        Set(SIZES.map(|s| self.shrink(s)))
    }

    /// Shrink to `n` pixels square by averaging the area each new pixel
    /// covers, weighting colours by alpha so the edges stay clean.
    fn shrink(&self, n: usize) -> Vec<u32> {
        let m = self.size;
        // in units where a source pixel is n wide and a new one m wide
        let mut out = vec![0u32; n * n];
        for oy in 0..n {
            let (y0, y1) = (oy * m, oy * m + m);
            for ox in 0..n {
                let (x0, x1) = (ox * m, ox * m + m);
                let mut sum = [0u64; 4];
                for sy in y0 / n..y1.div_ceil(n) {
                    let wy = (y1.min(sy * n + n) - y0.max(sy * n)) as u64;
                    for sx in x0 / n..x1.div_ceil(n) {
                        let wx = (x1.min(sx * n + n) - x0.max(sx * n)) as u64;
                        let p = self.pixels[sy * m + sx];
                        let wa = wx * wy * p[3] as u64;
                        sum[0] += p[0] as u64 * wa;
                        sum[1] += p[1] as u64 * wa;
                        sum[2] += p[2] as u64 * wa;
                        sum[3] += wa;
                    }
                }
                if sum[3] == 0 {
                    continue;
                }
                let alpha = sum[3] / (m * m) as u64;
                let c = |k: usize| (sum[k] / sum[3]) as u32;
                out[oy * n + ox] = (alpha as u32) << 24 | c(0) << 16 | c(1) << 8 | c(2);
            }
        }
        out
    }
}

/// A 48x48 app icon.
pub fn draw_icon(c: &mut Canvas, app: App, x: i32, y: i32) {
    let tile = Rect::new(x + 2, y + 2, 44, 44);
    match app {
        App::Terminal => {
            c.fill_round(tile, 8, rgb(0x2b, 0x2d, 0x36));
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.fill_rect(tile.x, tile.y, tile.w, 10, rgb(0x4a, 0x4e, 0x5c));
            }
            c.outline_round(tile, 8, rgb(0x16, 0x18, 0x1e));
            c.draw_text(x + 10, y + 20, ">_", rgb(0xe8, 0xe8, 0xf0));
        }
        App::Explorer => {
            // a yellow folder with a blue band, like Windows 11
            let back = rgb(0xe8, 0xa4, 0x10);
            let front = rgb(0xff, 0xc8, 0x3c);
            c.fill_round(Rect::new(x + 3, y + 7, 18, 10), 3, back);
            c.fill_round(Rect::new(x + 3, y + 10, 42, 30), 4, back);
            c.fill_round(Rect::new(x + 3, y + 15, 42, 27), 4, front);
            c.fill_round(Rect::new(x + 3, y + 30, 42, 12), 4, rgb(0x1c, 0x8c, 0xe8));
            c.fill_rect(x + 3, y + 30, 42, 4, rgb(0x1c, 0x8c, 0xe8));
            c.fill_rect(x + 6, y + 15, 36, 1, rgb(0xff, 0xe0, 0x90));
        }
        App::Notepad => {
            // a blue notepad with lines and a spiral
            let page = Rect::new(x + 8, y + 5, 32, 39);
            c.fill_round(page, 4, rgb(0x2a, 0x7c, 0xe0));
            c.fill_round(page.inset(3).offset(0, 3), 2, rgb(0xfa, 0xfb, 0xfe));
            for i in 0..5 {
                let ly = y + 17 + i * 5;
                c.fill_rect(
                    x + 15,
                    ly,
                    if i == 4 { 10 } else { 18 },
                    2,
                    rgb(0x5c, 0x6c, 0x84),
                );
            }
            for i in 0..4 {
                c.fill_round(
                    Rect::new(x + 13 + i * 7, y + 2, 3, 8),
                    1,
                    rgb(0x40, 0x44, 0x50),
                );
            }
        }
        App::Paint => {
            c.fill_round(tile, 8, rgb(0xfa, 0xfa, 0xfc));
            c.outline_round(tile, 8, rgb(0xb8, 0xbc, 0xc8));
            c.fill_round(Rect::new(x + 9, y + 9, 13, 13), 6, rgb(0xe8, 0x3c, 0x3c));
            c.fill_round(Rect::new(x + 24, y + 10, 13, 13), 6, rgb(0x2c, 0xb8, 0x5c));
            c.fill_round(Rect::new(x + 14, y + 23, 13, 13), 6, rgb(0x1c, 0x8c, 0xf0));
            for i in 0..3 {
                c.line(
                    x + 28 + i,
                    y + 42,
                    x + 41 + i,
                    y + 29,
                    rgb(0xa8, 0x6a, 0x2c),
                );
            }
            c.fill_round(Rect::new(x + 38, y + 25, 6, 6), 2, rgb(0x40, 0x40, 0x48));
        }
        App::Calculator => {
            c.fill_round(tile, 8, rgb(0x3a, 0x3e, 0x4c));
            c.outline_round(tile, 8, rgb(0x20, 0x22, 0x2c));
            c.fill_round(Rect::new(x + 9, y + 8, 30, 9), 2, rgb(0xd8, 0xe4, 0xf4));
            for row in 0..3 {
                for col in 0..3 {
                    let color = if (row, col) == (2, 2) {
                        rgb(0x3a, 0x9c, 0xff)
                    } else {
                        rgb(0xe8, 0xe8, 0xf0)
                    };
                    let r = Rect::new(x + 9 + col * 11, y + 20 + row * 8, 8, 6);
                    c.fill_round(r, 2, color);
                }
            }
        }
        App::Photos => {
            // a landscape: sky, sun and hills on a warm tile
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.vertical_gradient(tile, rgb(0x4c, 0xb8, 0xff), rgb(0xb8, 0xe4, 0xff));
                s.fill_polygon(
                    &[(x, y + 40), (x + 16, y + 22), (x + 30, y + 36), (x + 30, y + 48), (x, y + 48)],
                    rgb(0x2e, 0xa0, 0x56),
                );
                s.fill_polygon(
                    &[(x + 12, y + 48), (x + 32, y + 26), (x + 48, y + 42), (x + 48, y + 48)],
                    rgb(0x1c, 0x7c, 0x44),
                );
            }
            c.fill_round(Rect::new(x + 31, y + 8, 11, 11), 5, rgb(0xff, 0xd0, 0x40));
            c.outline_round(tile, 8, rgb(0x1c, 0x6c, 0xb0));
        }
        App::Video => {
            // a play button on a red-orange tile with film holes
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.vertical_gradient(tile, rgb(0xff, 0x6a, 0x3c), rgb(0xd8, 0x1c, 0x4c));
            }
            for i in 0..5 {
                c.fill_round(Rect::new(x + 6 + i * 8, y + 4, 5, 4), 1, rgb(0xff, 0xe0, 0xd8));
                c.fill_round(Rect::new(x + 6 + i * 8, y + 40, 5, 4), 1, rgb(0xff, 0xe0, 0xd8));
            }
            c.fill_polygon(&[(x + 18, y + 14), (x + 18, y + 34), (x + 35, y + 24)], 0xffffff);
            c.outline_round(tile, 8, rgb(0x90, 0x10, 0x30));
        }
        App::Browser => {
            // a globe on a blue tile
            {
                let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
                s.clip_round(tile, 8);
                s.vertical_gradient(tile, rgb(0x3c, 0x9c, 0xff), rgb(0x10, 0x5c, 0xd8));
            }
            let (cx, cy) = (x + 24, y + 24);
            c.fill_round(
                Rect::new(cx - 14, cy - 14, 28, 28),
                14,
                rgb(0xf4, 0xf8, 0xff),
            );
            let line = rgb(0x1c, 0x6c, 0xe0);
            c.outline_round(Rect::new(cx - 14, cy - 14, 28, 28), 14, line);
            c.outline_round(Rect::new(cx - 6, cy - 14, 12, 28), 6, line);
            c.fill_rect(cx, cy - 13, 1, 26, line);
            c.fill_rect(cx - 13, cy, 26, 1, line);
            c.fill_rect(cx - 11, cy - 7, 22, 1, line);
            c.fill_rect(cx - 11, cy + 7, 22, 1, line);
            c.outline_round(tile, 8, rgb(0x0c, 0x40, 0xa0));
        }
        // these have pictures, so they are never drawn
        App::Settings | App::About => c.fill_round(tile, 8, rgb(0x80, 0x80, 0x88)),
    }
}
