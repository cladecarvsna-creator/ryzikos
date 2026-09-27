//! Screenshots. PrintScreen (or Win+Shift+S) freezes the screen and dims
//! it; dragging picks an area, which is saved to Pictures/Screenshots and
//! put on the clipboard, so Ctrl+V pastes it in Paint or sends it in
//! Telegram. Enter takes the whole screen, Esc gives up.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{rgb, Canvas, Rect};
use super::text::UI_BOLD;
use super::widgets::{self, ClipImage};
use super::{picture, theme};
use crate::{fs, interrupts, users};

/// How long the note after a screenshot stays, in timer ticks.
const TOAST_TICKS: u64 = 4 * interrupts::TIMER_HZ;

pub struct Snip {
    /// The frozen screen.
    shot: Vec<u32>,
    w: i32,
    h: i32,
    /// Where the drag started, and where the mouse is now.
    start: Option<(i32, i32)>,
    end: (i32, i32),
}

impl Snip {
    pub fn new(shot: Vec<u32>, w: i32, h: i32) -> Self {
        Self {
            shot,
            w,
            h,
            start: None,
            end: (0, 0),
        }
    }

    /// The area picked so far.
    pub fn selection(&self) -> Option<Rect> {
        let (x0, y0) = self.start?;
        let (x1, y1) = self.end;
        let r = Rect::new(x0.min(x1), y0.min(y1), (x0 - x1).abs() + 1, (y0 - y1).abs() + 1);
        Some(r.intersect(&Rect::new(0, 0, self.w, self.h)))
    }

    pub fn press(&mut self, x: i32, y: i32) {
        self.start = Some((x, y));
        self.end = (x, y);
    }

    /// The mouse moved with the button down: what to draw again.
    pub fn drag(&mut self, x: i32, y: i32) -> Rect {
        let before = self.dirty_rect();
        self.end = (x, y);
        before.union(&self.dirty_rect())
    }

    /// The selection with its frame and the size label under it.
    fn dirty_rect(&self) -> Rect {
        match self.selection() {
            Some(r) => Rect::new(r.x - 3, r.y - 3, r.w.max(140) + 6, r.h + 40),
            None => Rect::new(0, 0, 0, 0),
        }
    }

    pub fn whole(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    /// The pixels of `r`.
    pub fn crop(&self, r: Rect) -> ClipImage {
        let r = r.intersect(&self.whole());
        let mut pixels = Vec::with_capacity((r.w * r.h) as usize);
        for y in r.y..r.bottom() {
            let row = (y * self.w) as usize;
            pixels.extend_from_slice(&self.shot[row + r.x as usize..row + r.right() as usize]);
        }
        ClipImage {
            w: r.w as usize,
            h: r.h as usize,
            pixels,
            path: None,
        }
    }

    pub fn draw(&self, c: &mut Canvas) {
        c.blit(0, 0, self.w, self.h, &self.shot, self.w as usize);
        let black = rgb(0, 0, 0);
        let Some(sel) = self.selection() else {
            c.fill_round_alpha(self.whole(), 0, black, 140);
            let hint = "Drag to pick an area.   Enter: the whole screen.   Esc: cancel.";
            let r = Rect::new(self.w / 2 - 290, 60, 580, 40);
            c.fill_round_alpha(r, 20, black, 190);
            c.text_centered_in(&UI_BOLD, r, hint, rgb(0xff, 0xff, 0xff));
            return;
        };
        // dim everything around the selection
        let (w, h) = (self.w, self.h);
        c.fill_round_alpha(Rect::new(0, 0, w, sel.y), 0, black, 140);
        c.fill_round_alpha(Rect::new(0, sel.bottom(), w, h - sel.bottom()), 0, black, 140);
        c.fill_round_alpha(Rect::new(0, sel.y, sel.x, sel.h), 0, black, 140);
        c.fill_round_alpha(
            Rect::new(sel.right(), sel.y, w - sel.right(), sel.h),
            0,
            black,
            110,
        );
        let edge = theme::accent();
        c.fill_rect(sel.x - 2, sel.y - 2, sel.w + 4, 2, edge);
        c.fill_rect(sel.x - 2, sel.bottom(), sel.w + 4, 2, edge);
        c.fill_rect(sel.x - 2, sel.y, 2, sel.h, edge);
        c.fill_rect(sel.right(), sel.y, 2, sel.h, edge);
        let size = format!("{} x {}", sel.w, sel.h);
        let label = Rect::new(sel.x, (sel.bottom() + 8).min(h - 30), 110, 26);
        c.fill_round_alpha(label, 13, black, 190);
        c.text_centered(label, &size, rgb(0xff, 0xff, 0xff));
    }
}

/// Save a screenshot as a PNG in the user's Pictures/Screenshots.
pub fn save(img: &ClipImage) -> Result<String, &'static str> {
    let user = users::current_name().unwrap_or_default();
    let pictures = fs::join(&fs::home(user.as_str()), "Pictures");
    let dir = fs::join(&pictures, "Screenshots");
    if !fs::is_dir(&dir) {
        if !fs::is_dir(&pictures) {
            fs::create_dir(&pictures).map_err(|e| e.message())?;
        }
        fs::create_dir(&dir).map_err(|e| e.message())?;
    }
    let path = fs::join(&dir, &fs::unique_name(&dir, "Screenshot", ".png"));
    let png = picture::encode_png(&img.pixels, img.w, img.h);
    fs::write(&path, &png).map_err(|e| e.message())?;
    Ok(path)
}

/// Save the picked area, put it on the clipboard, and say what happened.
pub fn finish(mut img: ClipImage) -> String {
    let note = match save(&img) {
        Ok(path) => {
            crate::serial::write_str("\nscreenshot: saved ");
            crate::serial::write_str(&path);
            crate::serial::write_str("\n");
            img.path = Some(path);
            String::from("Screenshot copied and saved in Pictures > Screenshots")
        }
        Err(e) => format!("Screenshot copied (not saved: {})", e),
    };
    widgets::copy_image(img);
    note
}

/// A note at the bottom right for a few seconds.
pub struct Toast {
    pub text: String,
    since: u64,
}

impl Toast {
    pub fn new(text: String) -> Self {
        Self {
            text,
            since: interrupts::ticks(),
        }
    }

    pub fn expired(&self) -> bool {
        interrupts::ticks() - self.since > TOAST_TICKS
    }

    pub fn rect(&self, screen_w: i32, bottom: i32) -> Rect {
        let w = super::text::UI.width(&self.text) + 40;
        Rect::new(screen_w - w - 16, bottom - 60, w, 44)
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect) {
        c.shadow(r, 10, 12, 4, 60);
        c.fill_round(r, 10, theme::face());
        c.outline_round(r, 10, theme::stroke());
        c.text_centered(r, &self.text, theme::text());
    }
}
