//! Welcome: opens after the first sign-in on a freshly installed
//! RyzikOS, with tiles for the first things to do. The launcher opens it
//! again later.

use alloc::format;

use super::canvas::{Canvas, Rect};
use super::icons::{self, LARGE};
use super::installer::wrap;
use super::settings::Page;
use super::text::{HEADING, UI_BOLD};
use super::{theme, App, MouseEvent, MouseKind};
use crate::keyboard::Key;

pub const CLIENT_W: i32 = 760;
pub const CLIENT_H: i32 = 540;

const TILE_W: i32 = 336;
const TILE_H: i32 = 84;

/// Where a tile goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Go {
    App(App),
    Settings(Page),
}

const TILES: [(App, Go, &str, &str); 6] = [
    (
        App::Settings,
        Go::Settings(Page::Personalization),
        "Make it yours",
        "Theme, accent color, wallpaper",
    ),
    (
        App::Store,
        Go::App(App::Store),
        "Get programs",
        "Games and tools in the App Store",
    ),
    (
        App::Browser,
        Go::App(App::Browser),
        "Go online",
        "The web, with tabs and HTTPS",
    ),
    (
        App::Explorer,
        Go::App(App::Explorer),
        "Your files",
        "Documents, Pictures, other disks",
    ),
    (
        App::About,
        Go::Settings(Page::Update),
        "Stay up to date",
        "New versions come from GitHub",
    ),
    (
        App::Terminal,
        Go::App(App::Terminal),
        "Command line",
        "Type help to see what it can do",
    ),
];

pub struct Welcome {
    pressed: Option<usize>,
    hover: Option<usize>,
    /// A Settings page for the desktop to open.
    pub open_page: Option<Page>,
}

fn tile_rect(i: usize) -> Rect {
    let col = (i % 2) as i32;
    let row = (i / 2) as i32;
    Rect::new(32 + col * (TILE_W + 24), 164 + row * (TILE_H + 14), TILE_W, TILE_H)
}

fn done_rect() -> Rect {
    Rect::new(CLIENT_W - 32 - 150, CLIENT_H - 26 - 34, 150, 34)
}

/// The index for the "Get started" button, after the tiles.
const DONE: usize = TILES.len();

impl Welcome {
    pub fn new() -> Self {
        Self {
            pressed: None,
            hover: None,
            open_page: None,
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        if done_rect().contains(x, y) {
            return Some(DONE);
        }
        (0..TILES.len()).find(|&i| tile_rect(i).contains(x, y))
    }

    fn press(&mut self, i: usize) {
        match TILES.get(i).map(|t| t.1) {
            Some(Go::App(app)) => {
                super::request_open(app);
            }
            Some(Go::Settings(page)) => self.open_page = Some(page),
            None => {
                super::request_close(App::Welcome);
            }
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        let hit = self.hit(ev.x, ev.y);
        match ev.kind {
            MouseKind::Move => {
                let changed = hit != self.hover;
                self.hover = hit;
                changed
            }
            MouseKind::Down { right: false } => {
                self.pressed = hit;
                hit.is_some()
            }
            MouseKind::Up => {
                let Some(i) = self.pressed.take() else {
                    return false;
                };
                if hit == Some(i) {
                    self.press(i);
                }
                true
            }
            _ => false,
        }
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        if matches!(key, Key::Enter | Key::Escape) {
            self.press(DONE);
            return true;
        }
        false
    }

    pub fn draw(&self, c: &mut Canvas) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, theme::face());
        // a band in the accent color with the logo
        let band = Rect::new(0, 0, CLIENT_W, 132);
        c.fill(band, theme::accent_base());
        let pics = icons::get();
        pics.draw_logo(c, 64, 32, 34);
        let white = 0xffffff;
        c.draw_text_in(&HEADING, 116, 36, "Welcome to RyzikOS", white);
        let line = format!(
            "Version {} is installed and ready. Here are a few things to try first.",
            crate::update::version()
        );
        wrap(c, 116, 80, CLIENT_W - 116 - 32, &line, white);

        for (i, (icon, _, title, note)) in TILES.iter().enumerate() {
            let r = tile_rect(i);
            let lit = self.hover == Some(i) || self.pressed == Some(i);
            c.fill_round(r, 8, if lit { theme::accent_light() } else { theme::light() });
            c.outline_round(r, 8, if lit { theme::accent() } else { theme::stroke() });
            pics.draw(c, *icon, LARGE, r.x + 18, r.y + (r.h - 48) / 2);
            c.draw_text_in(&UI_BOLD, r.x + 82, r.y + 22, title, theme::text());
            c.draw_text(r.x + 82, r.y + 44, note, theme::text_dim());
        }

        c.draw_text(
            32,
            CLIENT_H - 26 - 26,
            "Welcome opens again from the launcher.",
            theme::text_dim(),
        );
        theme::accent_button(c, done_rect(), "Get started", self.pressed == Some(DONE));
    }
}
