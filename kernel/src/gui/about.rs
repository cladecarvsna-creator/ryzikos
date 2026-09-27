//! "About RyzikOS": the logo, the name, the version and a few facts
//! about this computer, with an OK button.

use alloc::format;
use alloc::string::String;

use super::canvas::{Canvas, Rect};
use super::settings::Info;
use super::text::{HEADING, UI_BOLD};
use super::{icons, theme, App, MouseEvent, MouseKind};

pub const CLIENT_W: i32 = 560;
pub const CLIENT_H: i32 = 400;

/// The version people see: major and minor of the kernel crate's.
pub const VERSION: &str = "1.0";
const _: () = assert!(
    env!("CARGO_PKG_VERSION").as_bytes()[0] == VERSION.as_bytes()[0],
    "keep VERSION in step with Cargo.toml"
);

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
        icons::get().draw_logo(c, 64, 28, 26);
        c.draw_text_in(&HEADING, 104, 36, &format!("RyzikOS {}", VERSION), theme::text());
        c.fill_rect(24, 110, CLIENT_W - 48, 1, theme::stroke());

        let mut y = 128;
        c.draw_text_in(&UI_BOLD, 32, y, "RyzikOS", theme::text());
        y += 22;
        c.draw_text(32, y, &format!("Version {} (build {})", VERSION, env!("CARGO_PKG_VERSION")), theme::text());
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
