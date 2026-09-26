//! Pieces Notepad and File Explorer share: the clipboard, a one-line
//! text box, drop-down menus, message boxes, scroll bars and file icons.

use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::text::UI;
use super::theme;
use crate::keyboard::{self, Key};
use crate::sync::IrqMutex;

/// Text copied with Ctrl+C or Ctrl+X, for every app.
static CLIPBOARD: IrqMutex<String> = IrqMutex::new(String::new());

pub fn copy(text: &str) {
    let mut clip = CLIPBOARD.lock();
    clip.clear();
    clip.push_str(text);
}

pub fn paste() -> String {
    CLIPBOARD.lock().clone()
}

// ---- one-line text box --------------------------------------------------------

/// What a key did to a text box.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FieldEvent {
    None,
    Changed,
    Enter,
    Escape,
}

/// A one-line text box: a caret, a selection, clipboard keys.
#[derive(Clone, Default)]
pub struct TextField {
    pub text: Vec<char>,
    pub cursor: usize,
    /// The other end of the selection.
    pub anchor: usize,
    /// How far the text is scrolled to the left, in pixels.
    scroll: i32,
}

impl TextField {
    pub fn new(text: &str) -> Self {
        let text: Vec<char> = text.chars().collect();
        let n = text.len();
        Self {
            text,
            cursor: n,
            anchor: n,
            scroll: 0,
        }
    }

    pub fn string(&self) -> String {
        self.text.iter().collect()
    }

    pub fn set(&mut self, text: &str) {
        *self = Self::new(text);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// Select characters `from..to`.
    pub fn select(&mut self, from: usize, to: usize) {
        self.anchor = from.min(self.text.len());
        self.cursor = to.min(self.text.len());
    }

    fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }

    fn delete_selection(&mut self) -> bool {
        let (a, b) = self.range();
        if a == b {
            return false;
        }
        self.text.drain(a..b);
        self.cursor = a;
        self.anchor = a;
        true
    }

    fn insert(&mut self, s: &str) {
        self.delete_selection();
        for c in s.chars().filter(|c| !c.is_control()) {
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
        self.anchor = self.cursor;
    }

    fn move_to(&mut self, pos: usize) {
        self.cursor = pos.min(self.text.len());
        if !keyboard::shift_held() {
            self.anchor = self.cursor;
        }
    }

    pub fn on_key(&mut self, key: Key) -> FieldEvent {
        match key {
            Key::Enter => return FieldEvent::Enter,
            Key::Escape => return FieldEvent::Escape,
            Key::Char(c) if !c.is_control() => {
                let mut buf = [0u8; 4];
                self.insert(c.encode_utf8(&mut buf));
            }
            Key::Backspace => {
                if !self.delete_selection() && self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                    self.anchor = self.cursor;
                }
            }
            Key::Delete => {
                if !self.delete_selection() && self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            Key::Left => {
                let (a, b) = self.range();
                if a != b && !keyboard::shift_held() {
                    self.move_to(a);
                } else {
                    self.move_to(self.cursor.saturating_sub(1));
                }
            }
            Key::Right => {
                let (a, b) = self.range();
                if a != b && !keyboard::shift_held() {
                    self.move_to(b);
                } else {
                    self.move_to(self.cursor + 1);
                }
            }
            Key::Home => self.move_to(0),
            Key::End => self.move_to(self.text.len()),
            Key::Ctrl('a') => self.select_all(),
            Key::Ctrl('c') | Key::Ctrl('x') => {
                let (a, b) = self.range();
                if a < b {
                    let s: String = self.text[a..b].iter().collect();
                    copy(&s);
                    if matches!(key, Key::Ctrl('x')) {
                        self.delete_selection();
                    }
                }
            }
            Key::Ctrl('v') => {
                let clip = paste();
                let line = clip.lines().next().unwrap_or("");
                self.insert(line);
            }
            _ => return FieldEvent::None,
        }
        FieldEvent::Changed
    }

    /// Put the caret where the box `r` was clicked.
    pub fn click(&mut self, r: Rect, x: i32) {
        let target = x - r.x - 8 + self.scroll;
        let mut pen = 0;
        let mut pos = self.text.len();
        for (i, &c) in self.text.iter().enumerate() {
            let w = (UI.advance16(c) as i32 + 8) / 16;
            if pen + w / 2 > target {
                pos = i;
                break;
            }
            pen += w;
        }
        self.cursor = pos;
        if !keyboard::shift_held() {
            self.anchor = pos;
        }
    }

    fn width_to(&self, n: usize) -> i32 {
        let s: i32 = self.text[..n].iter().map(|&c| UI.advance16(c) as i32).sum();
        (s + 8) / 16
    }

    /// Draw it in `r`. The caret shows when `caret` is set.
    pub fn draw(&mut self, c: &mut Canvas, r: Rect, focused: bool, caret: bool) {
        c.fill_round(r, 4, theme::light());
        c.outline_round(r, 4, theme::stroke());
        if focused {
            c.fill_rect(r.x + 1, r.bottom() - 2, r.w - 2, 2, theme::accent());
        }
        // keep the caret in view
        let inner = r.w - 16;
        let caret_x = self.width_to(self.cursor);
        if caret_x - self.scroll > inner {
            self.scroll = caret_x - inner;
        }
        if caret_x < self.scroll {
            self.scroll = caret_x;
        }
        let mut t = c.sub(Rect::new(0, 0, c.width, c.height));
        t.clip_to(r.inset(2));
        let x0 = r.x + 8 - self.scroll;
        let y = r.y + (r.h - UI.line_height) / 2;
        let (a, b) = self.range();
        if focused && a != b {
            let (xa, xb) = (self.width_to(a), self.width_to(b));
            t.fill(
                Rect::new(x0 + xa, y, xb - xa, UI.line_height),
                theme::selection(),
            );
        }
        let s: String = self.text.iter().collect();
        t.draw_text(x0, y, &s, theme::text());
        if focused && caret {
            t.fill_rect(x0 + caret_x, y, 1, UI.line_height, theme::text());
        }
    }
}

// ---- menus --------------------------------------------------------------------

pub const MENU_ROW: i32 = 32;
pub const MENU_W: i32 = 240;

/// A menu item: its label, shortcut text and whether it can be used.
/// An empty label is a separator.
pub type Item<'a> = (&'a str, &'a str, bool);

pub fn menu_height(items: &[Item]) -> i32 {
    items
        .iter()
        .map(|i| if i.0.is_empty() { 9 } else { MENU_ROW })
        .sum::<i32>()
        + 8
}

pub fn menu_rect(x: i32, y: i32, items: &[Item]) -> Rect {
    Rect::new(x, y, MENU_W, menu_height(items))
}

/// Which item of a menu at `r` is at a point.
pub fn menu_item_at(r: Rect, items: &[Item], x: i32, y: i32) -> Option<usize> {
    if !r.contains(x, y) {
        return None;
    }
    let mut top = r.y + 4;
    for (i, item) in items.iter().enumerate() {
        let h = if item.0.is_empty() { 9 } else { MENU_ROW };
        if y >= top && y < top + h {
            return (!item.0.is_empty() && item.2).then_some(i);
        }
        top += h;
    }
    None
}

pub fn draw_menu(c: &mut Canvas, r: Rect, items: &[Item], hover: Option<usize>) {
    c.shadow(r, 8, 10, 3, 90);
    c.fill_round(r, 8, theme::menu());
    c.outline_round(r, 8, theme::frame());
    let mut top = r.y + 4;
    for (i, item) in items.iter().enumerate() {
        if item.0.is_empty() {
            c.fill_rect(r.x + 1, top + 4, r.w - 2, 1, theme::stroke());
            top += 9;
            continue;
        }
        let row = Rect::new(r.x + 4, top, r.w - 8, MENU_ROW);
        if hover == Some(i) {
            c.fill_round(row, 4, theme::hover());
        }
        let color = if item.2 {
            theme::text()
        } else {
            theme::text_dim()
        };
        let ty = top + (MENU_ROW - UI.line_height) / 2;
        c.draw_text(row.x + 14, ty, item.0, color);
        if !item.1.is_empty() {
            let w = UI.width(item.1);
            c.draw_text(row.right() - 12 - w, ty, item.1, theme::text_dim());
        }
        top += MENU_ROW;
    }
}

// ---- dialogs ------------------------------------------------------------------

/// A box in the middle of `area` with a title, lines of text and buttons.
/// Returns the button rectangles.
pub fn draw_message(
    c: &mut Canvas,
    area: Rect,
    title: &str,
    lines: &[&str],
    buttons: &[&str],
    hover: Option<usize>,
) -> Vec<Rect> {
    // dim what is behind
    c.fill_round_alpha(area, 0, rgb(0x20, 0x20, 0x28), 60);
    let r = message_rect(area, lines, buttons);
    c.shadow(r, 8, 16, 4, 120);
    c.fill_round(r, 8, theme::light());
    c.outline_round(r, 8, theme::frame());
    c.draw_text_in(
        &super::text::TITLE,
        r.x + 24,
        r.y + 20,
        title,
        theme::text(),
    );
    for (i, line) in lines.iter().enumerate() {
        c.draw_text(r.x + 24, r.y + 60 + i as i32 * 22, line, theme::text());
    }
    let footer = Rect::new(r.x, r.bottom() - 72, r.w, 72);
    {
        let mut f = c.sub(Rect::new(0, 0, c.width, c.height));
        f.clip_round(r, 8);
        f.fill(footer, theme::face());
        f.fill_rect(footer.x, footer.y, footer.w, 1, theme::stroke());
    }
    let rects = message_buttons(r, buttons.len());
    for (i, (b, label)) in rects.iter().zip(buttons).enumerate() {
        if i == 0 {
            theme::accent_button(c, *b, label, hover == Some(i));
        } else {
            theme::button(c, *b, label, hover == Some(i));
        }
    }
    rects
}

pub fn message_rect(area: Rect, lines: &[&str], buttons: &[&str]) -> Rect {
    let text_w = lines.iter().map(|l| UI.width(l)).max().unwrap_or(0);
    let w = (text_w + 48)
        .max(buttons.len() as i32 * 128 + 40)
        .max(360)
        .min(area.w - 20);
    let h = 60 + lines.len() as i32 * 22 + 16 + 72;
    Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h)
}

pub fn message_buttons(r: Rect, n: usize) -> Vec<Rect> {
    let (bw, gap) = (120, 8);
    let total = n as i32 * bw + (n as i32 - 1) * gap;
    let mut x = r.right() - 24 - total;
    let mut out = Vec::new();
    for _ in 0..n {
        out.push(Rect::new(x, r.bottom() - 52, bw, 32));
        x += bw + gap;
    }
    out
}

// ---- scroll bars -----------------------------------------------------------

/// The thumb of a scroll bar in `track` showing `view` of `total` from
/// `pos`, along its long side.
pub fn thumb(track: Rect, vertical: bool, total: i32, view: i32, pos: i32) -> Rect {
    let len = if vertical { track.h } else { track.w };
    if total <= view || total <= 0 {
        return track;
    }
    let size = (len as i64 * view as i64 / total as i64).max(24) as i32;
    let size = size.min(len);
    let room = len - size;
    let at = (room as i64 * pos as i64 / (total - view).max(1) as i64) as i32;
    if vertical {
        Rect::new(track.x, track.y + at, track.w, size)
    } else {
        Rect::new(track.x + at, track.y, size, track.h)
    }
}

/// Where dragging a thumb grabbed `grab` pixels from its start to `mouse`
/// scrolls to.
pub fn thumb_drag(
    track: Rect,
    vertical: bool,
    total: i32,
    view: i32,
    mouse: i32,
    grab: i32,
) -> i32 {
    let t = thumb(track, vertical, total, view, 0);
    let (len, size, start) = if vertical {
        (track.h, t.h, track.y)
    } else {
        (track.w, t.w, track.x)
    };
    let room = (len - size).max(1);
    let at = (mouse - grab - start).clamp(0, room);
    (at as i64 * (total - view).max(0) as i64 / room as i64) as i32
}

pub fn draw_scrollbar(
    c: &mut Canvas,
    track: Rect,
    vertical: bool,
    total: i32,
    view: i32,
    pos: i32,
) {
    c.fill(track, theme::track());
    if total <= view {
        return;
    }
    let t = thumb(track, vertical, total, view, pos).inset(3);
    c.fill_round(t, 3, theme::thumb());
}

// ---- icons --------------------------------------------------------------------

/// A yellow folder, `s` pixels wide (16 to 64).
pub fn folder_icon(c: &mut Canvas, x: i32, y: i32, s: i32) {
    let back = rgb(0xe8, 0xa8, 0x18);
    let front = rgb(0xff, 0xc8, 0x3c);
    let h = s * 3 / 4;
    let top = y + (s - h) / 2;
    let r = (s / 12).max(1);
    c.fill_round(Rect::new(x, top, s * 5 / 12, h / 3), r, back);
    c.fill_round(Rect::new(x, top + h / 8, s, h - h / 8), r, back);
    c.fill_round(Rect::new(x, top + h / 4, s, h - h / 4), r, front);
    c.fill_rect(
        x + r,
        top + h / 4,
        s - 2 * r,
        (s / 32).max(1),
        rgb(0xff, 0xde, 0x86),
    );
}

/// A text page with a folded corner.
pub fn file_icon(c: &mut Canvas, x: i32, y: i32, s: i32) {
    let w = s * 3 / 4;
    let px = x + (s - w) / 2;
    let fold = s / 4;
    let page = Rect::new(px, y, w, s);
    let edge = rgb(0x88, 0x8c, 0x98);
    c.fill_round(page, (s / 16).max(1), 0xffffff);
    c.outline_round(page, (s / 16).max(1), edge);
    // the folded corner
    c.fill(
        Rect::new(page.right() - fold, y, fold, fold),
        rgb(0xe4, 0xe6, 0xec),
    );
    c.line(page.right() - fold, y, page.right() - 1, y + fold - 1, edge);
    let lines = (s / 8).max(2);
    for i in 0..lines {
        let ly = y + fold + 2 + i * (s - fold - 4) / lines;
        if ly + 1 >= page.bottom() - 1 {
            break;
        }
        let lw = if i == lines - 1 { w / 2 } else { w - w / 3 };
        c.fill_rect(px + w / 6, ly, lw, (s / 32).max(1), mix(edge, 0xffffff, 80));
    }
}
