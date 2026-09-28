//! Emoji as little pictures: Twemoji (by Twitter, CC-BY 4.0), made into
//! one picture by scripts/make-emoji.py, with their code points in
//! emoji.txt in the same order. Text is searched for the longest
//! sequence of code points that has a picture; variation selectors don't
//! count, and skin tones are drawn as the plain yellow emoji.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

use super::canvas::{Canvas, Rect};
use super::picture;

/// Each picture is this many pixels square.
pub const SIZE: i32 = 20;
/// Pictures in a row of emoji.png.
const COLUMNS: usize = 64;
/// The longest sequence to look for (families are the longest).
const LONGEST: usize = 10;

const SHEET: &[u8] = include_bytes!("../../assets/emoji/emoji.png");
const LIST: &str = include_str!("../../assets/emoji/emoji.txt");

/// The picker's groups, in order, with a sample emoji for each tab.
pub const GROUPS: [(&str, char); 9] = [
    ("smileys", '\u{1f600}'),
    ("people", '\u{1f44b}'),
    ("nature", '\u{1f43b}'),
    ("food", '\u{1f34e}'),
    ("travel", '\u{1f697}'),
    ("activities", '\u{26bd}'),
    ("objects", '\u{1f4a1}'),
    ("symbols", '\u{2764}'),
    ("flags", '\u{1f3c1}'),
];

pub struct Emoji {
    /// SIZE x SIZE pixels for each emoji.
    pixels: Vec<Vec<u32>>,
    /// Code points (without FE0F) to the emoji's number.
    by_points: BTreeMap<Vec<u32>, u16>,
    /// Each emoji's code points, and its group's place in [`GROUPS`].
    pub list: Vec<(Vec<char>, u8)>,
}

static EMOJI: AtomicPtr<Emoji> = AtomicPtr::new(core::ptr::null_mut());

/// The emoji, read the first time they are asked for.
pub fn get() -> &'static Emoji {
    let p = EMOJI.load(Ordering::Acquire);
    if !p.is_null() {
        return unsafe { &*p };
    }
    let p = Box::into_raw(Box::new(Emoji::new()));
    EMOJI.store(p, Ordering::Release);
    unsafe { &*p }
}

impl Emoji {
    fn new() -> Emoji {
        let sheet = picture::decode(SHEET);
        let mut pixels = Vec::new();
        let mut by_points = BTreeMap::new();
        let mut list = Vec::new();
        for (i, line) in LIST.lines().enumerate() {
            let mut parts = line.split(' ');
            let points: Vec<u32> = parts
                .next()
                .unwrap_or("")
                .split('-')
                .filter_map(|h| u32::from_str_radix(h, 16).ok())
                .collect();
            let group = parts.next().unwrap_or("");
            let group = GROUPS.iter().position(|g| g.0 == group).unwrap_or(0) as u8;
            let mut px = alloc::vec![0u32; (SIZE * SIZE) as usize];
            if let Some(s) = &sheet {
                let (sx, sy) = ((i % COLUMNS) * SIZE as usize, (i / COLUMNS) * SIZE as usize);
                for y in 0..SIZE as usize {
                    for x in 0..SIZE as usize {
                        if let Some(&p) = s.pixels.get((sy + y) * s.width + sx + x) {
                            px[y * SIZE as usize + x] = p;
                        }
                    }
                }
            }
            pixels.push(px);
            list.push((
                points.iter().filter_map(|&p| char::from_u32(p)).collect(),
                group,
            ));
            by_points.insert(points, i as u16);
        }
        Emoji {
            pixels,
            by_points,
            list,
        }
    }

    /// The emoji that starts `chars`, if any: its number and how many
    /// chars it takes.
    pub fn at(&self, chars: &[char]) -> Option<(u16, usize)> {
        let first = *chars.first()? as u32;
        let keycap = first == 0x23 || first == 0x2a || (0x30..=0x39).contains(&first);
        // everything before this is text, apart from keycaps like #️⃣
        if first < 0x2000 && !keycap {
            return None;
        }
        let mut points = Vec::with_capacity(LONGEST);
        let mut best: Option<(u16, usize)> = None;
        for (n, &c) in chars.iter().enumerate().take(LONGEST * 3) {
            let p = c as u32;
            if p == 0xfe0f || (0x1f3fb..=0x1f3ff).contains(&p) {
                // a variation selector or a skin tone right after an emoji
                // belongs to it (skin tones are drawn plain)
                if let Some((e, end)) = best {
                    if end == n {
                        best = Some((e, n + 1));
                    }
                }
                continue;
            }
            points.push(p);
            if points.len() > LONGEST {
                break;
            }
            if let Some(&e) = self.by_points.get(&points) {
                best = Some((e, n + 1));
            }
        }
        // a lone digit, # or * is text
        if keycap && best.is_some_and(|(_, len)| len == 1) {
            return None;
        }
        best
    }

    /// Draw emoji number `e` with its top left at (x, y).
    pub fn draw(&self, c: &mut Canvas, e: u16, x: i32, y: i32) {
        if let Some(px) = self.pixels.get(e as usize) {
            c.blit_alpha(x, y, SIZE, SIZE, px);
        }
    }

    /// Draw emoji number `e` in the middle of `r`.
    pub fn draw_in(&self, c: &mut Canvas, e: u16, r: Rect) {
        self.draw(c, e, r.x + (r.w - SIZE) / 2, r.y + (r.h - SIZE) / 2);
    }

    /// The text of emoji number `e`, to put in a message.
    pub fn text(&self, e: u16) -> alloc::string::String {
        let Some((chars, _)) = self.list.get(e as usize) else {
            return alloc::string::String::new();
        };
        let mut s: alloc::string::String = chars.iter().collect();
        // a single symbol from the older blocks wants FE0F to be shown as
        // an emoji and not as text by other apps
        if chars.len() == 1 && (chars[0] as u32) < 0x1f000 {
            s.push('\u{fe0f}');
        }
        s
    }
}
