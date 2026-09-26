//! 8x16 bitmap font (Terminus Font), with ASCII, Latin-1, Cyrillic and
//! box drawing characters.

#[path = "font_data.rs"]
#[rustfmt::skip]
mod data;

pub const WIDTH: usize = 8;
pub const HEIGHT: usize = 16;

/// Shown for characters the font does not have.
const MISSING: [u8; HEIGHT] = [
    0x00, 0x00, 0x7e, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x7e, 0x00, 0x00, 0x00, 0x00,
];

/// Rows of the glyph for `c`, top to bottom; bit 7 is the leftmost pixel.
pub fn glyph(c: char) -> &'static [u8; HEIGHT] {
    let code = c as u32;
    for &(first, last, index) in data::RANGES {
        if (first..=last).contains(&code) {
            return &data::GLYPHS[index + (code - first) as usize];
        }
    }
    &MISSING
}
