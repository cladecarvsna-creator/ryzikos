//! Text with emoji pictures and links, for Telegram: split into pieces,
//! wrapped into lines, drawn, and found again under the mouse.

use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{Canvas, Color};
use super::emoji::{self, SIZE};
use super::text::Font;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Text(String),
    Emoji(u16),
    /// A line break.
    Break,
}

/// A word (with the space after it), or one emoji, or a line break.
#[derive(Clone, Debug)]
pub struct Piece {
    pub kind: Kind,
    /// Which link it belongs to.
    pub link: Option<u16>,
    pub w: i32,
}

/// A piece placed on a line: its x from the start of the line.
pub type Line = Vec<(i32, Piece)>;

/// Make one character drawable with our fonts: typographic quotes become
/// plain ones, and signs the fonts don't have become a dot.
fn push_clean(f: &Font, out: &mut String, c: char) {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{2032}' => out.push('\''),
        '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{2033}' => out.push('"'),
        '\u{2010}'..='\u{2012}' | '\u{2212}' => out.push('-'),
        '\u{2116}' => out.push_str("No."),
        '\u{2192}' => out.push_str("->"),
        '\u{2190}' => out.push_str("<-"),
        '\u{200b}'..='\u{200f}' | '\u{fe00}'..='\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}' | '\r' => {}
        '\t' => out.push(' '),
        c if f.glyph(c).is_some() => out.push(c),
        _ => {
            if !out.ends_with('\u{2022}') {
                out.push('\u{2022}');
            }
        }
    }
}

/// Split `text` into pieces. `links` are (first char, end char, link
/// number) in `text`'s chars.
pub fn pieces(f: &Font, text: &str, links: &[(usize, usize, u16)]) -> Vec<Piece> {
    let chars: Vec<char> = text.chars().collect();
    let emoji = emoji::get();
    let link_at = |i: usize| links.iter().find(|l| l.0 <= i && i < l.1).map(|l| l.2);
    let mut out: Vec<Piece> = Vec::new();
    let mut word = String::new();
    let mut word_link = None;
    let flush = |word: &mut String, link: Option<u16>, out: &mut Vec<Piece>| {
        if !word.is_empty() {
            let w = f.width(word);
            out.push(Piece {
                kind: Kind::Text(core::mem::take(word)),
                link,
                w,
            });
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let link = link_at(i);
        if link != word_link {
            flush(&mut word, word_link, &mut out);
            word_link = link;
        }
        let c = chars[i];
        if c == '\n' {
            flush(&mut word, word_link, &mut out);
            out.push(Piece {
                kind: Kind::Break,
                link: None,
                w: 0,
            });
            i += 1;
            continue;
        }
        if let Some((e, len)) = emoji.at(&chars[i..]) {
            flush(&mut word, word_link, &mut out);
            out.push(Piece {
                kind: Kind::Emoji(e),
                link,
                w: SIZE + 1,
            });
            i += len;
            continue;
        }
        push_clean(f, &mut word, c);
        if c == ' ' {
            flush(&mut word, word_link, &mut out);
        }
        i += 1;
    }
    flush(&mut word, word_link, &mut out);
    out
}

/// Put pieces on lines no wider than `max`.
pub fn wrap(f: &Font, pieces: &[Piece], max: i32) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut line: Line = Vec::new();
    let mut x = 0;
    for p in pieces {
        match &p.kind {
            Kind::Break => {
                lines.push(core::mem::take(&mut line));
                x = 0;
            }
            Kind::Emoji(_) => {
                if x + p.w > max && !line.is_empty() {
                    lines.push(core::mem::take(&mut line));
                    x = 0;
                }
                line.push((x, p.clone()));
                x += p.w;
            }
            Kind::Text(t) => {
                // a space at the end of a line may stick out
                let fits = x + f.width(t.trim_end()) <= max;
                if fits || (line.is_empty() && p.w <= max) {
                    line.push((x, p.clone()));
                    x += p.w;
                    continue;
                }
                if !line.is_empty() {
                    lines.push(core::mem::take(&mut line));
                    x = 0;
                }
                if f.width(t.trim_end()) <= max {
                    line.push((0, p.clone()));
                    x = p.w;
                    continue;
                }
                // a word longer than a line: break it anywhere
                let mut part = String::new();
                let mut pw = 0;
                for c in t.chars() {
                    let cw = f.width(c.encode_utf8(&mut [0; 4]));
                    if x + pw + cw > max && !part.is_empty() {
                        line.push((
                            x,
                            Piece {
                                kind: Kind::Text(core::mem::take(&mut part)),
                                link: p.link,
                                w: pw,
                            },
                        ));
                        lines.push(core::mem::take(&mut line));
                        x = 0;
                        pw = 0;
                    }
                    part.push(c);
                    pw += cw;
                }
                if !part.is_empty() {
                    line.push((
                        x,
                        Piece {
                            kind: Kind::Text(part),
                            link: p.link,
                            w: pw,
                        },
                    ));
                    x += pw;
                }
            }
        }
    }
    lines.push(line);
    lines
}

/// How wide a line is, without a space at its end.
pub fn line_width(f: &Font, line: &Line) -> i32 {
    match line.last() {
        Some((
            x,
            Piece {
                kind: Kind::Text(t),
                ..
            },
        )) => x + f.width(t.trim_end()),
        Some((x, p)) => x + p.w,
        None => 0,
    }
}

/// Draw a line with its left at `x` and its top at `y`; link pieces in
/// `link_color`, underlined.
pub fn draw_line(
    c: &mut Canvas,
    f: &Font,
    x: i32,
    y: i32,
    line: &Line,
    color: Color,
    link_color: Color,
) {
    let emoji = emoji::get();
    for (px, p) in line {
        let color = if p.link.is_some() { link_color } else { color };
        match &p.kind {
            Kind::Text(t) => {
                c.draw_text_in(f, x + px, y, t, color);
                if p.link.is_some() {
                    let w = f.width(t.trim_end());
                    c.fill_rect(x + px, y + f.line_height - 1, w, 1, color);
                }
            }
            Kind::Emoji(e) => emoji.draw(c, *e, x + px, y + (f.line_height - SIZE) / 2),
            Kind::Break => {}
        }
    }
}

// ---- single lines ----------------------------------------------------------------

/// The width of one line of text, with emoji.
pub fn width(f: &Font, s: &str) -> i32 {
    pieces(f, s, &[])
        .iter()
        .filter(|p| p.kind != Kind::Break)
        .map(|p| p.w)
        .sum()
}

/// Draw one line of text with emoji; line breaks become spaces. Returns
/// its width.
pub fn draw(c: &mut Canvas, f: &Font, x: i32, y: i32, s: &str, color: Color) -> i32 {
    let emoji = emoji::get();
    let mut px = x;
    for p in pieces(f, s, &[]) {
        match &p.kind {
            Kind::Text(t) => {
                c.draw_text_in(f, px, y, t, color);
            }
            Kind::Emoji(e) => emoji.draw(c, *e, px, y + (f.line_height - SIZE) / 2),
            Kind::Break => {
                px += f.width(" ");
                continue;
            }
        }
        px += p.w;
    }
    px - x
}

/// Draw one line cut to `max` pixels, with "..." where it was cut.
pub fn draw_fit(c: &mut Canvas, f: &Font, x: i32, y: i32, s: &str, max: i32, color: Color) {
    let all = pieces(f, s, &[]);
    let total: i32 = all.iter().map(|p| p.w).sum();
    let dots = f.width("\u{2026}");
    let emoji = emoji::get();
    let mut px = x;
    for p in &all {
        let room = if total <= max { max } else { max - dots };
        match &p.kind {
            Kind::Break => {
                px += f.width(" ");
            }
            Kind::Emoji(e) => {
                if px - x + p.w > room {
                    break;
                }
                emoji.draw(c, *e, px, y + (f.line_height - SIZE) / 2);
                px += p.w;
            }
            Kind::Text(t) => {
                if px - x + p.w <= room {
                    c.draw_text_in(f, px, y, t, color);
                    px += p.w;
                    continue;
                }
                let mut part = String::new();
                let mut w = 0;
                for ch in t.chars() {
                    let cw = f.width(ch.encode_utf8(&mut [0; 4]));
                    if px - x + w + cw > room {
                        break;
                    }
                    part.push(ch);
                    w += cw;
                }
                c.draw_text_in(f, px, y, &part, color);
                px += w;
                break;
            }
        }
    }
    if total > max {
        c.draw_text_in(f, px, y, "\u{2026}", color);
    }
}
