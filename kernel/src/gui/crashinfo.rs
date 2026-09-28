//! "Why did my computer restart?": after the blue screen, the next
//! sign-in opens this window with the crash report the panic handler
//! left on the disk (see crash.rs), in plain words first and every
//! detail a click away.

use alloc::format;
use alloc::string::String;

use super::canvas::{Canvas, Rect};
use super::installer::wrap;
use super::text::{HEADING, MONO, UI, UI_BOLD};
use super::{theme, App, MouseEvent, MouseKind};
use crate::crash::{self, Report};
use crate::keyboard::Key;

pub const CLIENT_W: i32 = 760;
pub const CLIENT_H: i32 = 560;

/// The blue of the blue screen.
const BLUE: u32 = 0x0d2663;
const BAND_H: i32 = 136;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Details,
    Copy,
    Close,
}

const BUTTONS: [Button; 3] = [Button::Details, Button::Copy, Button::Close];

fn button_rect(b: Button) -> Rect {
    let y = CLIENT_H - 24 - 34;
    match b {
        Button::Details => Rect::new(32, y, 190, 34),
        Button::Copy => Rect::new(32 + 190 + 12, y, 150, 34),
        Button::Close => Rect::new(CLIENT_W - 32 - 140, y, 140, 34),
    }
}

pub struct CrashInfo {
    report: Option<Report>,
    details: bool,
    copied: bool,
    pressed: Option<Button>,
}

impl CrashInfo {
    pub fn new() -> Self {
        Self {
            report: None,
            details: false,
            copied: false,
            pressed: None,
        }
    }

    /// Read the report again, when the window opens.
    pub fn start(&mut self) {
        self.report = Report::last();
        self.details = false;
        self.copied = false;
        if let Some(r) = &self.report {
            crate::serial::write_str("crash: showing the report, ");
            crate::serial::write_str(r.get("code"));
            crate::serial::write_str("\n");
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<Button> {
        BUTTONS
            .into_iter()
            .filter(|&b| b == Button::Close || self.report.is_some())
            .find(|&b| button_rect(b).contains(x, y))
    }

    fn press(&mut self, b: Button) {
        match b {
            Button::Details => self.details = !self.details,
            Button::Copy => {
                if let Some(r) = &self.report {
                    let mut text = String::from("RyzikOS crash report\n");
                    text.push_str(&r.text());
                    super::widgets::copy(&text);
                    self.copied = true;
                }
            }
            Button::Close => {
                super::request_close(App::Crash);
            }
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        let hit = self.hit(ev.x, ev.y);
        match ev.kind {
            MouseKind::Down { right: false } => {
                self.pressed = hit;
                hit.is_some()
            }
            MouseKind::Up => {
                let Some(b) = self.pressed.take() else {
                    return false;
                };
                if hit == Some(b) {
                    self.press(b);
                }
                true
            }
            _ => false,
        }
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        match key {
            Key::Enter | Key::Escape => {
                self.press(Button::Close);
                true
            }
            _ => false,
        }
    }

    pub fn draw(&self, c: &mut Canvas) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, theme::face());
        c.fill(Rect::new(0, 0, CLIENT_W, BAND_H), BLUE);
        let white = 0xffffff;
        // a ring with an exclamation mark, as on the blue screen
        c.fill_round(Rect::new(30, 30, 40, 40), 20, white);
        c.fill_round(Rect::new(34, 34, 32, 32), 16, BLUE);
        let bang = HEADING.width("!");
        c.draw_text_in(&HEADING, 50 - bang / 2, 32, "!", white);
        c.draw_text_in(&HEADING, 84, 30, "Why did my computer restart?", white);
        let Some(r) = &self.report else {
            wrap(
                c,
                84,
                78,
                CLIENT_W - 84 - 32,
                "No crashes: RyzikOS on this disk has never stopped because of an error.",
                white,
            );
            theme::accent_button(
                c,
                button_rect(Button::Close),
                "OK",
                self.pressed == Some(Button::Close),
            );
            return;
        };
        let code = r.get("code");
        let when = if r.get("date").is_empty() {
            String::new()
        } else {
            format!(" on {} at {}", r.get("date"), r.get("time"))
        };
        let line = format!(
            "RyzikOS stopped because of an error{} and restarted the computer. Here is what happened.",
            when
        );
        wrap(c, 84, 78, CLIENT_W - 84 - 32, &line, white);

        if self.details {
            self.draw_details(c, r);
        } else {
            self.draw_summary(c, code, r);
        }

        let label = if self.details { "Summary" } else { "Details" };
        theme::button(
            c,
            button_rect(Button::Details),
            label,
            self.pressed == Some(Button::Details),
        );
        let copy = if self.copied { "Copied" } else { "Copy" };
        theme::button(
            c,
            button_rect(Button::Copy),
            copy,
            self.pressed == Some(Button::Copy),
        );
        theme::accent_button(
            c,
            button_rect(Button::Close),
            "OK",
            self.pressed == Some(Button::Close),
        );
    }

    fn draw_summary(&self, c: &mut Canvas, code: &str, r: &Report) {
        let w = CLIENT_W - 64;
        let mut y = BAND_H + 24;
        c.draw_text_in(&UI_BOLD, 32, y, "What happened", theme::text());
        y = wrap(
            c,
            32,
            y + UI.line_height + 8,
            w,
            crash::explain(code),
            theme::text(),
        ) + 14;
        c.draw_text_in(&UI_BOLD, 32, y, "What to do", theme::text());
        y = wrap(
            c,
            32,
            y + UI.line_height + 8,
            w,
            crash::advice(code),
            theme::text(),
        ) + 14;
        c.draw_text_in(&UI_BOLD, 32, y, "Error code", theme::text());
        y += UI.line_height + 8;
        c.draw_text_in(&MONO, 32, y, code, theme::accent());
        y += MONO.line_height + 14;
        let app = r.get("app");
        if !app.is_empty() {
            let line = format!("The window in front was {}.", app);
            wrap(c, 32, y, w, &line, theme::text_dim());
        }
    }

    fn draw_details(&self, c: &mut Canvas, r: &Report) {
        let label_w = 180;
        let x = 32 + label_w;
        let w = CLIENT_W - x - 32;
        let mut y = BAND_H + 20;
        let bottom = button_rect(Button::Close).y - 12;
        for (key, label) in crash::DETAILS {
            let value = r.get(key);
            if value.is_empty() || y > bottom - UI.line_height {
                continue;
            }
            c.draw_text_in(&UI_BOLD, 32, y, label, theme::text_dim());
            y = wrap_hard(c, x, y, w, value, theme::text()) + 6;
        }
    }
}

/// Like `wrap`, but also breaks words longer than the line, such as
/// file paths and hex numbers.
fn wrap_hard(c: &mut Canvas, x: i32, mut y: i32, w: i32, text: &str, color: u32) -> i32 {
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() {
            String::from(word)
        } else {
            format!("{} {}", line, word)
        };
        if UI.width(&candidate) <= w {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            c.draw_text(x, y, &line, color);
            y += UI.line_height + 4;
        }
        line = String::new();
        for ch in word.chars() {
            line.push(ch);
            if UI.width(&line) > w {
                line.pop();
                c.draw_text(x, y, &line, color);
                y += UI.line_height + 4;
                line = String::from(ch);
            }
        }
    }
    if !line.is_empty() {
        c.draw_text(x, y, &line, color);
        y += UI.line_height + 4;
    }
    y
}
