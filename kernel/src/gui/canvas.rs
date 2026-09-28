//! Drawing into a pixel buffer in memory. Pixels are `0x00RRGGBB`.
//!
//! A `Canvas` is a view into a buffer with its own origin and clip
//! rectangle, so a window can draw in its own coordinates without
//! touching anything outside its area.

use super::text::{self, Font};
use crate::font;

/// A colour as `0x00RRGGBB`.
pub type Color = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Mix two colours; `t` goes from 0 (all `a`) to 255 (all `b`).
pub fn mix(a: Color, b: Color, t: u32) -> Color {
    let t = t.min(255);
    let channel = |shift: u32| {
        let (x, y) = ((a >> shift) & 0xff, (b >> shift) & 0xff);
        ((x * (255 - t) + y * t) / 255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    /// The smallest rectangle covering both. Empty rectangles are ignored.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Shrink by `n` pixels on every side.
    pub fn inset(&self, n: i32) -> Rect {
        Rect::new(self.x + n, self.y + n, self.w - 2 * n, self.h - 2 * n)
    }
}

pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    stride: usize,
    /// Where (0, 0) of this view is in the buffer.
    ox: i32,
    oy: i32,
    /// Drawable area in buffer coordinates.
    clip: Rect,
    /// Optional rounded clip: a rectangle in buffer coordinates and, per
    /// corner row, how many pixels are cut off at each end.
    round: Option<RoundClip>,
    pub width: i32,
    pub height: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Self {
        assert!(pixels.len() >= width * height);
        Self {
            pixels,
            stride: width,
            ox: 0,
            oy: 0,
            clip: Rect::new(0, 0, width as i32, height as i32),
            round: None,
            width: width as i32,
            height: height as i32,
        }
    }

    /// A view of the area `r` (in this view's coordinates) that draws
    /// relative to `r`'s corner and only inside `r` and the current clip.
    pub fn sub(&mut self, r: Rect) -> Canvas<'_> {
        let area = r.offset(self.ox, self.oy);
        Canvas {
            pixels: self.pixels,
            stride: self.stride,
            ox: area.x,
            oy: area.y,
            clip: self.clip.intersect(&area),
            round: self.round,
            width: r.w,
            height: r.h,
        }
    }

    /// Move the pixels inside `r` up by `dy` (down if it is negative),
    /// for scrolling. The rows that move in keep their old pixels; the
    /// caller draws them again.
    pub fn shift_up(&mut self, r: Rect, dy: i32) {
        let area = r.offset(self.ox, self.oy).intersect(&self.clip);
        if area.is_empty() || dy == 0 || dy.abs() >= area.h {
            return;
        }
        let (x, w) = (area.x as usize, area.w as usize);
        let row = |y: i32| y as usize * self.stride + x;
        if dy > 0 {
            for y in area.y..area.bottom() - dy {
                let src = row(y + dy);
                self.pixels.copy_within(src..src + w, row(y));
            }
        } else {
            for y in (area.y - dy..area.bottom()).rev() {
                let src = row(y + dy);
                self.pixels.copy_within(src..src + w, row(y));
            }
        }
    }

    /// The whole buffer's address and size, to tell buffers apart.
    pub fn buffer_id(&self) -> (usize, usize) {
        (self.pixels.as_ptr() as usize, self.pixels.len())
    }

    /// The area that can be drawn on, in this view's coordinates.
    pub fn clip_rect(&self) -> Rect {
        self.clip.offset(-self.ox, -self.oy)
    }

    /// Limit drawing to `r` (in this view's coordinates) as well.
    pub fn clip_to(&mut self, r: Rect) {
        self.clip = self.clip.intersect(&r.offset(self.ox, self.oy));
    }

    /// Limit drawing to `r` with its corners rounded by `radius`.
    pub fn clip_round(&mut self, r: Rect, radius: i32) {
        self.clip_to(r);
        let r = r.offset(self.ox, self.oy);
        let radius = radius.clamp(0, MAX_RADIUS as i32);
        let mut cut = [0; MAX_RADIUS];
        for (row, c) in cut.iter_mut().enumerate().take(radius as usize) {
            // first column whose pixel centre is inside the corner circle
            let dy2 = 2 * (radius - row as i32) - 1;
            let mut i = 0;
            while i < radius {
                let dx2 = 2 * (radius - i) - 1;
                if dx2 * dx2 + dy2 * dy2 <= 4 * radius * radius {
                    break;
                }
                i += 1;
            }
            *c = i;
        }
        self.round = Some(RoundClip {
            rect: r,
            radius,
            cut,
        });
    }

    /// The columns `[x0, x1)` a buffer row may draw on.
    fn span(&self, y: i32) -> (i32, i32) {
        let (mut x0, mut x1) = (self.clip.x, self.clip.right());
        if let Some(rc) = &self.round {
            let row = if y < rc.rect.y + rc.radius {
                y - rc.rect.y
            } else if y >= rc.rect.bottom() - rc.radius {
                rc.rect.bottom() - 1 - y
            } else {
                -1
            };
            if row >= 0 && row < rc.radius {
                let cut = rc.cut[row as usize];
                x0 = x0.max(rc.rect.x + cut);
                x1 = x1.min(rc.rect.right() - cut);
            }
        }
        (x0, x1)
    }

    /// Mix `c` into a buffer pixel with coverage `a` (0 to 256).
    fn blend(&mut self, x: i32, y: i32, c: Color, a: i32) {
        if a <= 0 || y < self.clip.y || y >= self.clip.bottom() {
            return;
        }
        let (x0, x1) = self.span(y);
        if x < x0 || x >= x1 {
            return;
        }
        let p = &mut self.pixels[y as usize * self.stride + x as usize];
        *p = blended(*p, c, a);
    }

    /// The columns `[x0, x1)` of buffer row `y` that may be drawn on, or
    /// an empty range when the row is outside the clip. Loops find this
    /// once per row instead of `blend` finding it for every pixel.
    fn row_span(&self, y: i32) -> (i32, i32) {
        if y < self.clip.y || y >= self.clip.bottom() {
            return (0, 0);
        }
        self.span(y)
    }

    /// `blend` for the pixels of one row whose span is already known.
    #[inline]
    fn blend_in(&mut self, span: (i32, i32), x: i32, y: i32, c: Color, a: i32) {
        if a > 0 && x >= span.0 && x < span.1 {
            let p = &mut self.pixels[y as usize * self.stride + x as usize];
            *p = blended(*p, c, a);
        }
    }

    /// Blend `c` with coverage `a` over buffer columns `[x0, x1)` of row `y`.
    fn blend_run(&mut self, x0: i32, x1: i32, y: i32, c: Color, a: i32) {
        let (s0, s1) = self.row_span(y);
        let (x0, x1) = (x0.max(s0), x1.min(s1));
        if a <= 0 || x0 >= x1 {
            return;
        }
        let row = y as usize * self.stride;
        let run = &mut self.pixels[row + x0 as usize..row + x1 as usize];
        if a >= 256 {
            run.fill(c);
            return;
        }
        // mix(p, c, t) with c's share worked out once for the whole run
        let t = (a * 255 / 256) as u32;
        let (cr, cg, cb) = ((c >> 16 & 0xff) * t, (c >> 8 & 0xff) * t, (c & 0xff) * t);
        let s = 255 - t;
        for p in run {
            let q = *p;
            *p = ((q >> 16 & 0xff) * s + cr) / 255 << 16
                | ((q >> 8 & 0xff) * s + cg) / 255 << 8
                | ((q & 0xff) * s + cb) / 255;
        }
    }

    /// Mix `c` into the pixel at (`x`, `y`) with coverage `a` (0 to 256).
    pub fn blend_at(&mut self, x: i32, y: i32, c: Color, a: i32) {
        self.blend(x + self.ox, y + self.oy, c, a);
    }

    /// Whether anything inside `r` could be drawn.
    pub fn visible(&self, r: Rect) -> bool {
        !self.clip.intersect(&r.offset(self.ox, self.oy)).is_empty()
    }

    pub fn pixel(&mut self, x: i32, y: i32, c: Color) {
        let (x, y) = (x + self.ox, y + self.oy);
        if self.clip.contains(x, y) {
            let (x0, x1) = self.span(y);
            if x >= x0 && x < x1 {
                self.pixels[y as usize * self.stride + x as usize] = c;
            }
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        let r = Rect::new(x + self.ox, y + self.oy, w, h).intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        for row in r.y..r.bottom() {
            let (x0, x1) = self.span(row);
            let (x0, x1) = (x0.max(r.x), x1.min(r.right()));
            if x0 < x1 {
                let start = row as usize * self.stride;
                self.pixels[start + x0 as usize..start + x1 as usize].fill(c);
            }
        }
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        self.fill_rect(r.x, r.y, r.w, r.h, c);
    }

    pub fn vertical_gradient(&mut self, r: Rect, top: Color, bottom: Color) {
        let first = (self.clip.y - self.oy).max(r.y);
        let last = (self.clip.bottom() - self.oy).min(r.bottom());
        for y in first..last {
            let t = ((y - r.y) * 255 / r.h.max(1)) as u32;
            self.fill_rect(r.x, y, r.w, 1, mix(top, bottom, t));
        }
    }

    /// Bresenham line, calling `plot` for every point.
    pub fn trace_line(x0: i32, y0: i32, x1: i32, y1: i32, mut plot: impl FnMut(i32, i32)) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        loop {
            plot(x, y);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, c: Color) {
        Self::trace_line(x0, y0, x1, y1, |x, y| self.pixel(x, y, c));
    }

    pub fn fill_circle(&mut self, cx: i32, cy: i32, radius: i32, c: Color) {
        for dy in -radius..=radius {
            // widest dx on this row
            let mut dx = 0;
            while (dx + 1) * (dx + 1) + dy * dy <= radius * radius {
                dx += 1;
            }
            self.fill_rect(cx - dx, cy + dy, 2 * dx + 1, 1, c);
        }
    }

    /// Draw one 8x16 bitmap character (the boot console font).
    pub fn draw_bitmap_char(&mut self, x: i32, y: i32, ch: char, c: Color) {
        if !self.visible(Rect::new(x, y, font::WIDTH as i32, font::HEIGHT as i32)) {
            return;
        }
        for (row, bits) in font::glyph(ch).iter().enumerate() {
            for col in 0..font::WIDTH {
                if bits & (0x80 >> col) != 0 {
                    self.pixel(x + col as i32, y + row as i32, c);
                }
            }
        }
    }

    /// Draw one anti-aliased character with its line top at `y`. Returns
    /// false when the font has no glyph for it.
    pub fn draw_glyph(&mut self, f: &Font, x: i32, y: i32, ch: char, c: Color) -> bool {
        let Some(g) = f.glyph(ch) else {
            return false;
        };
        let (gx, gy) = (x + g.x as i32, y + g.y as i32);
        let (w, h) = (g.w as i32, g.h as i32);
        if w == 0 || !self.visible(Rect::new(gx, gy, w, h)) {
            return true;
        }
        self.draw_coverage(gx, gy, w, h, f.coverage(g), c);
        true
    }

    /// Blend `c` through a `w` x `h` coverage map (0 to 255 per pixel)
    /// with its corner at (`x`, `y`): how text is drawn.
    pub fn draw_coverage(&mut self, x: i32, y: i32, w: i32, h: i32, coverage: &[u8], c: Color) {
        if w <= 0 || !self.visible(Rect::new(x, y, w, h)) {
            return;
        }
        for row in 0..h {
            let by = y + row + self.oy;
            let span = self.row_span(by);
            if span.0 >= span.1 {
                continue;
            }
            for col in 0..w {
                let a = coverage[(row * w + col) as usize] as i32;
                if a != 0 {
                    let a = if a == 255 { 256 } else { a };
                    self.blend_in(span, x + col + self.ox, by, c, a);
                }
            }
        }
    }

    /// Draw text in a font and return its width in pixels.
    pub fn draw_text_in(&mut self, f: &Font, x: i32, y: i32, s: &str, c: Color) -> i32 {
        let mut pen = x * 16;
        for ch in s.chars() {
            // one lookup per character: its glyph, or '?' for both
            // drawing and advancing, as draw_glyph and advance16 do
            let Some(g) = f.glyph(ch).or_else(|| f.glyph('?')) else {
                continue;
            };
            let (gx, gy) = ((pen + 8) / 16 + g.x as i32, y + g.y as i32);
            self.draw_coverage(gx, gy, g.w as i32, g.h as i32, f.coverage(g), c);
            pen += g.advance as i32;
        }
        (pen + 8) / 16 - x
    }

    /// Draw text in the normal UI font and return its width in pixels.
    pub fn draw_text(&mut self, x: i32, y: i32, s: &str, c: Color) -> i32 {
        self.draw_text_in(&text::UI, x, y, s, c)
    }

    /// Draw text centred in `r`.
    pub fn text_centered(&mut self, r: Rect, s: &str, c: Color) {
        self.text_centered_in(&text::UI, r, s, c);
    }

    pub fn text_centered_in(&mut self, f: &Font, r: Rect, s: &str, c: Color) {
        let x = r.x + (r.w - f.width(s)) / 2;
        let y = r.y + (r.h - f.line_height) / 2;
        self.draw_text_in(f, x, y, s, c);
    }

    /// Copy a `w` x `h` block of pixels with row length `src_stride`.
    pub fn blit(&mut self, x: i32, y: i32, w: i32, h: i32, src: &[u32], src_stride: usize) {
        let dst = Rect::new(x + self.ox, y + self.oy, w, h);
        let r = dst.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let (sx, sy) = ((r.x - dst.x) as usize, (r.y - dst.y) as usize);
        for row in 0..r.h as usize {
            let y = r.y + row as i32;
            let (x0, x1) = self.span(y);
            let (x0, x1) = (x0.max(r.x), x1.min(r.right()));
            if x0 >= x1 {
                continue;
            }
            let skip = (x0 - r.x) as usize;
            let n = (x1 - x0) as usize;
            let from = (sy + row) * src_stride + sx + skip;
            let to = y as usize * self.stride + x0 as usize;
            self.pixels[to..to + n].copy_from_slice(&src[from..from + n]);
        }
    }

    /// Copy pixels with alpha in the top byte (255 is opaque).
    pub fn blit_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, src: &[u32]) {
        if !self.visible(Rect::new(x, y, w, h)) {
            return;
        }
        for row in 0..h {
            let by = y + row + self.oy;
            let span = self.row_span(by);
            if span.0 >= span.1 {
                continue;
            }
            for col in 0..w {
                let p = src[(row * w + col) as usize];
                let a = (p >> 24) as i32;
                if a != 0 {
                    let a = if a == 255 { 256 } else { a };
                    self.blend_in(span, x + col + self.ox, by, p & 0xff_ffff, a);
                }
            }
        }
    }

    /// Fill a rectangle with rounded, anti-aliased corners, blended with
    /// `alpha` (0 to 256).
    pub fn fill_round_alpha(&mut self, r: Rect, radius: i32, c: Color, alpha: i32) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(0);
        let b = r.offset(self.ox, self.oy);
        let area = b.intersect(&self.clip);
        if area.is_empty() {
            return;
        }
        for y in area.y..area.bottom() {
            // distance into a corner row, if any, and the corner centre row
            let corner_cy = if y < b.y + radius {
                Some(b.y + radius)
            } else if y >= b.bottom() - radius {
                Some(b.bottom() - radius)
            } else {
                None
            };
            let Some(cy) = corner_cy else {
                self.blend_run(area.x, area.right(), y, c, alpha);
                continue;
            };
            // the straight middle of the row in one go, then the corners
            let (left, right) = (b.x + radius, b.right() - radius);
            self.blend_run(area.x.max(left), area.right().min(right), y, c, alpha);
            let span = self.row_span(y);
            for x in (area.x..area.right().min(left)).chain(area.x.max(right)..area.right()) {
                let cx = if x < left { left } else { right };
                let d = distance256(2 * x + 1 - 2 * cx, 2 * y + 1 - 2 * cy);
                let cover = (radius * 256 - d + 128).clamp(0, 256);
                self.blend_in(span, x, y, c, cover * alpha / 256);
            }
        }
    }

    pub fn fill_round(&mut self, r: Rect, radius: i32, c: Color) {
        self.fill_round_alpha(r, radius, c, 256);
    }

    /// A one pixel, anti-aliased outline with rounded corners.
    pub fn outline_round(&mut self, r: Rect, radius: i32, c: Color) {
        let radius = radius.min(r.w / 2).min(r.h / 2).max(1);
        self.fill_rect(r.x + radius, r.y, r.w - 2 * radius, 1, c);
        self.fill_rect(r.x + radius, r.bottom() - 1, r.w - 2 * radius, 1, c);
        self.fill_rect(r.x, r.y + radius, 1, r.h - 2 * radius, c);
        self.fill_rect(r.right() - 1, r.y + radius, 1, r.h - 2 * radius, c);
        let b = r.offset(self.ox, self.oy);
        let corners = [
            (b.x, b.y, b.x + radius, b.y + radius),
            (b.right() - radius, b.y, b.right() - radius, b.y + radius),
            (b.x, b.bottom() - radius, b.x + radius, b.bottom() - radius),
            (
                b.right() - radius,
                b.bottom() - radius,
                b.right() - radius,
                b.bottom() - radius,
            ),
        ];
        let ring = radius * 256 - 128;
        for (x0, y0, cx, cy) in corners {
            for y in y0..y0 + radius {
                for x in x0..x0 + radius {
                    let d = distance256(2 * x + 1 - 2 * cx, 2 * y + 1 - 2 * cy);
                    let cover = (256 - (d - ring).abs()).clamp(0, 256);
                    self.blend(x, y, c, cover);
                }
            }
        }
    }

    /// A soft shadow around the rounded rectangle `r`, fading out over
    /// `spread` pixels and shifted down by `drop`.
    pub fn shadow(&mut self, r: Rect, radius: i32, spread: i32, drop: i32, strength: i32) {
        let s = r.offset(self.ox, self.oy + drop);
        let outer = Rect::new(
            s.x - spread,
            s.y - spread,
            s.w + 2 * spread,
            s.h + 2 * spread,
        );
        let area = outer.intersect(&self.clip);
        let inner = r.offset(self.ox, self.oy).inset(radius);
        for y in area.y..area.bottom() {
            let (x0, x1) = self.row_span(y);
            let (x0, x1) = (x0.max(area.x), x1.min(area.right()));
            // the window covers the middle of the shadow: skip over it
            let (skip0, skip1) = if !inner.is_empty() && y >= inner.y && y < inner.bottom() {
                let skip0 = inner.x.clamp(x0, x1.max(x0));
                (skip0, inner.right().clamp(skip0, x1.max(skip0)))
            } else {
                (x1, x1)
            };
            for x in (x0..skip0).chain(skip1..x1) {
                // distance from the rounded rectangle, in 1/256 pixels
                let dx = (s.x + radius - x).max(x - (s.right() - 1 - radius)).max(0);
                let dy = (s.y + radius - y).max(y - (s.bottom() - 1 - radius)).max(0);
                // straight edges need no square root, only the corners do
                let d = match (dx, dy) {
                    (0, d) | (d, 0) => d * 256,
                    _ => distance256(2 * dx, 2 * dy),
                } - radius * 256;
                let t = 256 - (d.max(0) / spread).min(256);
                let a = strength * t * t / 65536;
                if a > 0 {
                    let p = &mut self.pixels[y as usize * self.stride + x as usize];
                    *p = mix(*p, 0, a as u32);
                }
            }
        }
    }

    /// Draw a `sw` x `sh` image stretched to `dst`, blended with `alpha`
    /// (0 to 256). Used for windows and menus that zoom and fade.
    pub fn blit_scaled(&mut self, dst: Rect, src: &[u32], sw: i32, sh: i32, alpha: i32) {
        let d = dst.offset(self.ox, self.oy);
        let area = d.intersect(&self.clip);
        if area.is_empty() || alpha <= 0 || sw <= 0 || sh <= 0 {
            return;
        }
        // source position per destination pixel in 1/65536 steps
        let step_x = ((sw as i64) << 16) / d.w as i64;
        let step_y = ((sh as i64) << 16) / d.h as i64;
        let a = alpha.min(256) as u32;
        for y in area.y..area.bottom() {
            let (x0, x1) = self.span(y);
            let (x0, x1) = (x0.max(area.x), x1.min(area.right()));
            if x0 >= x1 {
                continue;
            }
            let sy = ((((y - d.y) as i64 * step_y) >> 16) as usize).min(sh as usize - 1);
            let src_row = &src[sy * sw as usize..(sy + 1) * sw as usize];
            let row = y as usize * self.stride;
            let mut sx = (x0 - d.x) as i64 * step_x;
            for x in x0..x1 {
                let p = src_row[((sx >> 16) as usize).min(sw as usize - 1)];
                let out = &mut self.pixels[row + x as usize];
                *out = if a >= 256 { p } else { fast_mix(*out, p, a) };
                sx += step_x;
            }
        }
    }

    /// Draw a `sw` x `sh` image shrunk into `dst`, each new pixel the
    /// average of the ones it covers, so small copies of windows (Task
    /// View, Alt+Tab) stay readable. For growing, use `blit_scaled`.
    pub fn blit_smooth(&mut self, dst: Rect, src: &[u32], sw: i32, sh: i32) {
        let d = dst.offset(self.ox, self.oy);
        let area = d.intersect(&self.clip);
        if area.is_empty() || sw <= 0 || sh <= 0 {
            return;
        }
        let (sw, sh) = (sw as usize, sh as usize);
        // source columns per destination column, found once
        let mut cols = [(0usize, 0usize); 1920];
        for x in area.x..area.right().min(area.x + 1920) {
            let i = (x - area.x) as usize;
            let x0 = (x - d.x) as usize * sw / d.w as usize;
            let x1 = ((x - d.x + 1) as usize * sw / d.w as usize).clamp(x0 + 1, sw);
            cols[i] = (x0.min(sw - 1), x1);
        }
        for y in area.y..area.bottom() {
            let (x0c, x1c) = self.span(y);
            let (x0c, x1c) = (x0c.max(area.x), x1c.min(area.right()));
            let y0 = (y - d.y) as usize * sh / d.h as usize;
            let y1 = ((y - d.y + 1) as usize * sh / d.h as usize).clamp(y0 + 1, sh);
            let y0 = y0.min(sh - 1);
            let row = y as usize * self.stride;
            for x in x0c..x1c {
                let (sx0, sx1) = cols[((x - area.x) as usize).min(1919)];
                let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
                for sy in y0..y1 {
                    for &p in &src[sy * sw + sx0..sy * sw + sx1] {
                        r += (p >> 16) & 0xff;
                        g += (p >> 8) & 0xff;
                        b += p & 0xff;
                        n += 1;
                    }
                }
                self.pixels[row + x as usize] = (r / n) << 16 | (g / n) << 8 | (b / n);
            }
        }
    }

    /// Like `outline_round`, blended with `alpha` (0 to 256).
    pub fn outline_round_alpha(&mut self, r: Rect, radius: i32, c: Color, alpha: i32) {
        if alpha >= 256 {
            self.outline_round(r, radius, c);
            return;
        }
        let radius = radius.min(r.w / 2).min(r.h / 2).max(1);
        let b = r.offset(self.ox, self.oy);
        for x in b.x + radius..b.right() - radius {
            self.blend(x, b.y, c, alpha);
            self.blend(x, b.bottom() - 1, c, alpha);
        }
        for y in b.y + radius..b.bottom() - radius {
            self.blend(b.x, y, c, alpha);
            self.blend(b.right() - 1, y, c, alpha);
        }
        let ring = radius * 256 - 128;
        for (x0, y0) in [
            (b.x, b.y),
            (b.right() - radius, b.y),
            (b.x, b.bottom() - radius),
            (b.right() - radius, b.bottom() - radius),
        ] {
            let cx = if x0 == b.x { b.x + radius } else { x0 };
            let cy = if y0 == b.y { b.y + radius } else { y0 };
            for y in y0..y0 + radius {
                for x in x0..x0 + radius {
                    let d = distance256(2 * x + 1 - 2 * cx, 2 * y + 1 - 2 * cy);
                    let cover = (256 - (d - ring).abs()).clamp(0, 256);
                    self.blend(x, y, c, cover * alpha / 256);
                }
            }
        }
    }

    /// Fill a polygon (even-odd rule, pixel centres), without smoothing.
    pub fn fill_polygon(&mut self, points: &[(i32, i32)], c: Color) {
        let top = points.iter().map(|p| p.1).min().unwrap_or(0);
        let bottom = points.iter().map(|p| p.1).max().unwrap_or(0);
        for y in top..bottom {
            // crossings of the line through this row's pixel centres, in
            // doubled coordinates to stay in integers
            let cy = 2 * y + 1;
            let mut xs = [0i32; 16];
            let mut n = 0;
            for i in 0..points.len() {
                let (x0, y0) = points[i];
                let (x1, y1) = points[(i + 1) % points.len()];
                let (y0, y1) = (2 * y0, 2 * y1);
                if ((y0 <= cy && cy < y1) || (y1 <= cy && cy < y0)) && n < xs.len() {
                    xs[n] = x0 + (x1 - x0) * (cy - y0) / (y1 - y0);
                    n += 1;
                }
            }
            xs[..n].sort_unstable();
            for pair in xs[..n].chunks(2) {
                if let [a, b] = pair {
                    self.fill_rect(*a, y, b - a, 1, c);
                }
            }
        }
    }
}

/// A pixel `p` with `c` blended over it at coverage `a` (1 to 256).
#[inline]
fn blended(p: Color, c: Color, a: i32) -> Color {
    if a >= 256 {
        c
    } else {
        mix(p, c, (a * 255 / 256) as u32)
    }
}

/// `mix` for whole buffers: red and blue share one multiply. `t` goes
/// from 0 (all `a`) to 256 (all `b`).
#[inline]
pub fn fast_mix(a: Color, b: Color, t: u32) -> Color {
    let s = 256 - t;
    let rb = ((a & 0xff00ff) * s + (b & 0xff00ff) * t) >> 8;
    let g = ((a & 0xff00) * s + (b & 0xff00) * t) >> 8;
    (rb & 0xff00ff) | (g & 0xff00)
}

const MAX_RADIUS: usize = 16;

#[derive(Clone, Copy)]
struct RoundClip {
    rect: Rect,
    radius: i32,
    cut: [i32; MAX_RADIUS],
}

/// Length of the vector (dx/2, dy/2) in 1/256 pixels. Taking doubled
/// coordinates lets callers measure from pixel centres.
fn distance256(dx2: i32, dy2: i32) -> i32 {
    let (x, y) = (dx2 as i64, dy2 as i64);
    let squared = (x * x + y * y) as u64 * 16384;
    isqrt(squared) as i32
}

pub fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    // start from a power of two at or above the root: a handful of steps
    // instead of halving down from `n` (the answer is the same)
    let mut x = 1u64 << (64 - (n - 1).leading_zeros()).div_ceil(2);
    let mut y = (x + n / x) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Width of text in the normal UI font.
pub fn text_width(s: &str) -> i32 {
    text::UI.width(s)
}

/// A few rectangles that need drawing again. Changes far apart stay
/// separate, so a blinking caret and a clock do not repaint everything
/// between them.
#[derive(Clone, Copy, Default)]
pub struct Dirty {
    rects: [Rect; Dirty::MAX],
    len: usize,
}

impl Dirty {
    const MAX: usize = 8;

    pub fn add(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        let mut r = r;
        // swallow every rectangle it touches, then add it
        let mut i = 0;
        while i < self.len {
            let other = self.rects[i];
            if !r.inset(-8).intersect(&other).is_empty() {
                r = r.union(&other);
                self.len -= 1;
                self.rects[i] = self.rects[self.len];
                i = 0;
            } else {
                i += 1;
            }
        }
        if self.len == Self::MAX {
            // full: merge into the one that grows least
            let area = |r: &Rect| r.w as i64 * r.h as i64;
            let best = (0..self.len)
                .min_by_key(|&i| area(&self.rects[i].union(&r)) - area(&self.rects[i]))
                .unwrap_or(0);
            self.rects[best] = self.rects[best].union(&r);
            return;
        }
        self.rects[self.len] = r;
        self.len += 1;
    }

    /// Hand out the rectangles and start again.
    pub fn take(&mut self) -> ([Rect; Dirty::MAX], usize) {
        let out = (self.rects, self.len);
        self.len = 0;
        out
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
