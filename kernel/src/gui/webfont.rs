//! Fonts for web pages, rasterised at run time in any size from the
//! DejaVu TrueType files (fonts/) with ab_glyph, and cached.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use ab_glyph::{point, Font as _, FontRef, PxScale, ScaleFont};

use super::canvas::{Canvas, Color};
use crate::sync::IrqMutex;

/// A font face: bold, italic and monospace variants of DejaVu Sans.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Face {
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

static SANS: &[u8] = include_bytes!("../../../fonts/DejaVuSans.ttf");
static SANS_BOLD: &[u8] = include_bytes!("../../../fonts/DejaVuSans-Bold.ttf");
static SANS_ITALIC: &[u8] = include_bytes!("../../../fonts/DejaVuSans-Oblique.ttf");
static SANS_BOLD_ITALIC: &[u8] = include_bytes!("../../../fonts/DejaVuSans-BoldOblique.ttf");
static MONO: &[u8] = include_bytes!("../../../fonts/DejaVuSansMono.ttf");
static MONO_BOLD: &[u8] = include_bytes!("../../../fonts/DejaVuSansMono-Bold.ttf");

impl Face {
    fn index(self) -> usize {
        match (self.mono, self.bold, self.italic) {
            (true, false, _) => 4,
            (true, true, _) => 5,
            (false, false, false) => 0,
            (false, true, false) => 1,
            (false, false, true) => 2,
            (false, true, true) => 3,
        }
    }
}

struct Glyph {
    /// Offset of the coverage map from the pen position and baseline.
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    /// Advance in 1/16 pixels.
    advance: i32,
    coverage: Vec<u8>,
}

struct Cache {
    fonts: Option<[FontRef<'static>; 6]>,
    glyphs: BTreeMap<(u8, u16, char), Glyph>,
}

static CACHE: IrqMutex<Cache> = IrqMutex::new(Cache {
    fonts: None,
    glyphs: BTreeMap::new(),
});

/// Font sizes are whole pixels in this range.
fn size_px(size: f32) -> u16 {
    (size + 0.5).clamp(6.0, 120.0) as u16
}

impl Cache {
    fn fonts(&mut self) -> &[FontRef<'static>; 6] {
        self.fonts.get_or_insert_with(|| {
            let f = |b: &'static [u8]| FontRef::try_from_slice(b).expect("bad built-in font");
            [
                f(SANS),
                f(SANS_BOLD),
                f(SANS_ITALIC),
                f(SANS_BOLD_ITALIC),
                f(MONO),
                f(MONO_BOLD),
            ]
        })
    }

    fn glyph(&mut self, face: usize, size: u16, c: char) -> &Glyph {
        let key = (face as u8, size, c);
        if !self.glyphs.contains_key(&key) {
            if self.glyphs.len() > 30_000 {
                self.glyphs.clear();
            }
            let g = self.rasterise(face, size, c);
            self.glyphs.insert(key, g);
        }
        &self.glyphs[&key]
    }

    fn rasterise(&mut self, face: usize, size: u16, c: char) -> Glyph {
        let font = &self.fonts()[face];
        let scale = PxScale::from(size as f32);
        let scaled = font.as_scaled(scale);
        let mut id = font.glyph_id(c);
        if id.0 == 0 && !c.is_whitespace() && !c.is_control() {
            // not in the font: show a question mark
            id = font.glyph_id('?');
        }
        let advance = (scaled.h_advance(id) * 16.0 + 0.5) as i32;
        let glyph = id.with_scale_and_position(scale, point(0.0, 0.0));
        let mut out = Glyph {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            advance,
            coverage: Vec::new(),
        };
        if c.is_whitespace() || c.is_control() {
            return out;
        }
        if let Some(outline) = font.outline_glyph(glyph) {
            let b = outline.px_bounds();
            let (w, h) = ((b.max.x - b.min.x) as u16, (b.max.y - b.min.y) as u16);
            let mut coverage = alloc::vec![0u8; w as usize * h as usize];
            outline.draw(|x, y, v| {
                let i = y as usize * w as usize + x as usize;
                if i < coverage.len() {
                    coverage[i] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            });
            out.x = b.min.x as i16;
            out.y = b.min.y as i16;
            out.w = w;
            out.h = h;
            out.coverage = coverage;
        }
        out
    }
}

/// Vertical metrics of a font size, in pixels.
#[derive(Clone, Copy, Debug)]
pub struct VMetrics {
    pub ascent: i32,
    pub descent: i32,
}

pub fn vmetrics(face: Face, size: f32) -> VMetrics {
    let mut cache = CACHE.lock();
    let font = &cache.fonts()[face.index()];
    let scaled = font.as_scaled(PxScale::from(size_px(size) as f32));
    VMetrics {
        ascent: (scaled.ascent() + 0.5) as i32,
        descent: (-scaled.descent() + 0.5) as i32,
    }
}

/// Characters that are drawn as something else.
fn substitute(c: char) -> Option<char> {
    Some(match c {
        '\u{a0}' | '\u{2009}' | '\u{202f}' | '\u{2002}' | '\u{2003}' | '\t' => ' ',
        '\u{ad}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{200e}' | '\u{200f}' | '\u{feff}' => return None,
        c => c,
    })
}

/// Width of `text` in 1/16 pixels.
pub fn width16(face: Face, size: f32, text: &str) -> i32 {
    let size = size_px(size);
    let face = face.index();
    let mut cache = CACHE.lock();
    text.chars()
        .filter_map(substitute)
        .map(|c| cache.glyph(face, size, c).advance)
        .sum()
}

/// Draw `text` with its baseline at `baseline`. Returns the width.
pub fn draw(c: &mut Canvas, face: Face, size: f32, x: i32, baseline: i32, text: &str, color: Color) -> i32 {
    let size = size_px(size);
    let face = face.index();
    let mut cache = CACHE.lock();
    let mut pen = x * 16;
    for ch in text.chars().filter_map(substitute) {
        let g = cache.glyph(face, size, ch);
        let gx = (pen + 8) / 16 + g.x as i32;
        let gy = baseline + g.y as i32;
        c.draw_coverage(gx, gy, g.w as i32, g.h as i32, &g.coverage, color);
        pen += g.advance;
    }
    (pen + 8) / 16 - x
}
