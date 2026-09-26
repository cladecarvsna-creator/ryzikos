//! Smooth, anti-aliased text. The glyphs are rasterised ahead of time by
//! scripts/gen-aa-font.py into 8-bit coverage maps (font_data.rs).

pub use super::font_data::{CLOCK, HEADING, LARGE, MONO, TITLE, UI, UI_BOLD};

#[derive(Clone, Copy)]
pub struct Glyph {
    pub code: u32,
    pub w: u8,
    pub h: u8,
    /// Offset of the coverage map from the pen position and line top.
    pub x: i8,
    pub y: i8,
    /// Distance to the next glyph, in 1/16 pixels.
    pub advance: u16,
    /// Start of the coverage map in `Font::alpha`.
    pub offset: u32,
}

pub struct Font {
    pub line_height: i32,
    /// Sorted by code point.
    pub glyphs: &'static [Glyph],
    pub alpha: &'static [u8],
}

impl Font {
    pub fn glyph(&self, c: char) -> Option<&Glyph> {
        let code = c as u32;
        self.glyphs
            .binary_search_by_key(&code, |g| g.code)
            .ok()
            .map(|i| &self.glyphs[i])
    }

    /// Coverage map of a glyph, `w * h` bytes.
    pub fn coverage(&self, g: &Glyph) -> &[u8] {
        let start = g.offset as usize;
        &self.alpha[start..start + g.w as usize * g.h as usize]
    }

    /// Width of a string in pixels.
    pub fn width(&self, text: &str) -> i32 {
        let sixteenths: u32 = text.chars().map(|c| self.advance16(c) as u32).sum();
        (sixteenths as i32 + 8) / 16
    }

    pub fn advance16(&self, c: char) -> u16 {
        self.glyph(c)
            .or_else(|| self.glyph('?'))
            .map_or(0, |g| g.advance)
    }
}
