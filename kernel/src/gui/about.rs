//! "About EverOS", like winver on Windows: the name, the version and a
//! few facts about this computer, with an OK button.

use alloc::format;
use alloc::string::String;

use super::canvas::{rgb, Canvas, Rect};
use super::settings::Info;
use super::text::{HEADING, UI_BOLD};
use super::{icons, theme, App, MouseEvent, MouseKind};

pub const CLIENT_W: i32 = 560;
pub const CLIENT_H: i32 = 400;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct About {
    pressed: bool,
}

fn ok_rect() -> Rect {
    Rect::new(CLIENT_W - 24 - 110, CLIENT_H - 24 - 34, 110, 34)
}

impl About {
    pub fn new() -> Self {
        Self { pressed: false }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Down { right: false } if ok_rect().contains(ev.x, ev.y) => {
                self.pressed = true;
                true
            }
            MouseKind::Up if self.pressed => {
                self.pressed = false;
                if ok_rect().contains(ev.x, ev.y) {
                    super::request_close(App::About);
                }
                true
            }
            _ => false,
        }
    }

    pub fn draw(&self, c: &mut Canvas, info: &Info) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, theme::light());

        // the logo and the name
        draw_logo(c, 32, 30);
        c.draw_text_in(&HEADING, 104, 36, "EverOS", theme::text());
        c.fill_rect(24, 110, CLIENT_W - 48, 1, theme::stroke());

        let mut y = 128;
        c.draw_text_in(&UI_BOLD, 32, y, "EverOS", theme::text());
        y += 22;
        c.draw_text(32, y, &format!("Version {}", VERSION), theme::text());
        y += 22;
        let about = "A hobby operating system for x86_64, written in assembly and Rust.";
        c.draw_text(32, y, about, theme::text());
        y += 22;
        c.draw_text(32, y, "MIT License.", theme::text_dim());
        y += 36;

        let user = crate::users::current_name();
        let rows = [
            ("Memory", format!("{} MB", info.memory_mib)),
            ("Screen", format!("{} x {}", info.screen.0, info.screen.1)),
            ("Bootloader", String::from(info.bootloader)),
            (
                "Signed in as",
                String::from(user.as_ref().map(|n| n.as_str()).unwrap_or("nobody")),
            ),
        ];
        for (label, value) in rows {
            c.draw_text(32, y, label, theme::text_dim());
            c.draw_text(160, y, &value, theme::text());
            y += 22;
        }

        // the About picture in the corner
        icons::get().draw_large(c, App::About, CLIENT_W - 32 - 48, 36);

        theme::accent_button(c, ok_rect(), "OK", self.pressed);
    }
}

/// The four blue squares of the Start button, three times bigger.
pub fn draw_logo(c: &mut Canvas, x: i32, y: i32) {
    for (i, color) in [
        rgb(0x2a, 0x9c, 0xf4),
        rgb(0x18, 0x84, 0xe8),
        rgb(0x10, 0x74, 0xd8),
        rgb(0x0a, 0x60, 0xc4),
    ]
    .into_iter()
    .enumerate()
    {
        let (col, row) = ((i % 2) as i32, (i / 2) as i32);
        c.fill_round(Rect::new(x + col * 30, y + row * 30, 28, 28), 4, color);
    }
}
