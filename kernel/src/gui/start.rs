//! The RyzikOS launcher, opened with the logo in the dock or the Super
//! key: the user and round buttons to lock, sign out, restart and shut
//! down along the top, a wide search box, every app in a grid of big
//! icons, and the programs downloaded in the browser as a row of chips.
//!
//! Typing while the launcher is open searches the apps; Enter starts the
//! best match.

use super::anim::{self, Fader, ONE};
use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::icons::{Icons, LARGE};
use super::text::{UI, UI_BOLD};
use super::theme;
use super::{App, APPS};
use crate::keyboard::Key;
use crate::{users, StackString};
use alloc::string::String;
use alloc::vec::Vec;

pub const W: i32 = 620;
pub const H: i32 = 476;
const RADIUS: i32 = 24;
const MAX_RECENT: usize = 4;
const MAX_TARGETS: usize = 24;
const MAX_PROGRAMS: usize = 6;

/// Apps in a row of the grid, and the size of each cell.
const PER_ROW: usize = 6;
const CELL_W: i32 = 96;
const CELL_H: i32 = 96;
/// Where the search box, the grid and the row of programs start, from
/// the top.
const SEARCH_Y: i32 = 78;
const GRID_Y: i32 = 158;
const PROGRAMS_Y: i32 = 382;

/// What the desktop should do after an event.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// The launcher changed and must be drawn again.
    Redraw,
    Open(App),
    /// Run installed program `i` (see `StartMenu::program`).
    Program(usize),
    /// Open the browser's Programs page.
    GetPrograms,
    Close,
    Restart,
    ShutDown,
    Lock,
    SignOut,
}

/// Something in the launcher that can be clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    App(App),
    /// A chip in the row of programs.
    Program(usize),
    GetPrograms,
    Lock,
    SignOut,
    Restart,
    ShutDown,
}

/// The round buttons in the footer, from the left.
const POWER: [(Target, &str); 4] = [
    (Target::Lock, "Lock"),
    (Target::SignOut, "Sign out"),
    (Target::Restart, "Restart"),
    (Target::ShutDown, "Shut down"),
];

pub struct StartMenu {
    pub open: bool,
    search: StackString<32>,
    /// Most recently opened first.
    recent: [Option<App>; MAX_RECENT],
    hover: Fader<Target>,
    /// Installed programs (paths), read when the launcher opens.
    programs: Vec<String>,
}

/// Apps in alphabetical order, for search results.
const SORTED: [App; 16] = [
    App::About,
    App::Installer,
    App::Store,
    App::Browser,
    App::Calculator,
    App::Paint,
    App::Explorer,
    App::Photos,
    App::Settings,
    App::TaskManager,
    App::Telegram,
    App::Terminal,
    App::Notepad,
    App::Video,
    App::Vpn,
    App::Welcome,
];

impl StartMenu {
    pub fn new() -> Self {
        Self {
            open: false,
            search: StackString::new(),
            recent: [None; MAX_RECENT],
            hover: Fader::new(anim::ms(120)),
            programs: Vec::new(),
        }
    }

    /// The path of installed program `i`.
    pub fn program(&self, i: usize) -> Option<&str> {
        self.programs.get(i).map(|p| p.as_str())
    }

    /// Where the launcher sits on a screen of the given size: centred
    /// above the dock, under the menu bar.
    pub fn panel(width: i32, height: i32, dock: i32, menu_bar: i32) -> Rect {
        let y = (height - dock - 12 - H).max(menu_bar + 8);
        Rect::new((width - W) / 2, y, W, H)
    }

    pub fn show(&mut self) {
        self.open = true;
        self.search.clear();
        self.hover.jump(None);
        self.programs = crate::web::installed_programs();
        self.programs.truncate(MAX_PROGRAMS);
    }

    /// Fade hover highlights. Returns whether the launcher must be redrawn.
    pub fn tick(&mut self) -> bool {
        self.hover.tick()
    }

    /// Remember an app for the row of recent apps.
    pub fn note_opened(&mut self, app: App) {
        let old = self.recent;
        self.recent[0] = Some(app);
        let mut n = 1;
        for a in old.into_iter().flatten() {
            if a != app && n < MAX_RECENT {
                self.recent[n] = Some(a);
                n += 1;
            }
        }
    }

    /// Recently opened apps, newest first.
    pub fn recent(&self) -> impl Iterator<Item = App> + '_ {
        self.recent.into_iter().flatten()
    }

    fn matches(&self) -> impl Iterator<Item = App> + '_ {
        SORTED
            .into_iter()
            .filter(|a| a.listed() && contains_ignore_case(a.title(), self.search.as_str()))
    }

    // ---- layout ------------------------------------------------------------

    fn search_box(p: Rect) -> Rect {
        Rect::new(p.x + 24, p.y + SEARCH_Y, p.w - 48, 40)
    }

    /// Round power button `i` of POWER, at the top right.
    fn power_button(p: Rect, i: usize) -> Rect {
        let x = p.right() - 24 - 4 * 40 - 3 * 10 + i as i32 * 50;
        Rect::new(x, p.y + 20, 40, 40)
    }

    /// A chip in the row of programs, `x` pixels from the left edge.
    fn chip(p: Rect, x: i32, label: &str) -> Rect {
        Rect::new(p.x + x, p.y + PROGRAMS_Y + 30, UI.width(label) + 52, 40)
    }

    fn program_name(path: &str) -> &str {
        let name = crate::fs::file_name(path);
        name.strip_suffix(crate::web::PROGRAM_EXT).unwrap_or(name)
    }

    /// Everything clickable, and where it is.
    fn targets(&self, p: Rect) -> ([(Target, Rect); MAX_TARGETS], usize) {
        let mut out = [(Target::Lock, Rect::default()); MAX_TARGETS];
        let mut n = 0;
        let mut push = |t: Target, r: Rect| {
            if n < MAX_TARGETS {
                out[n] = (t, r);
                n += 1;
            }
        };
        for (i, (t, _)) in POWER.into_iter().enumerate() {
            push(t, Self::power_button(p, i));
        }

        if !self.search.as_str().is_empty() {
            for (i, app) in self.matches().enumerate() {
                push(
                    Target::App(app),
                    Rect::new(p.x + 24, p.y + GRID_Y + i as i32 * 52, p.w - 48, 48),
                );
            }
            return (out, n);
        }
        let left = p.x + (p.w - PER_ROW as i32 * CELL_W) / 2;
        // two rows of six; Welcome and the installer are found by search
        let grid = APPS
            .into_iter()
            .filter(|a| a.listed() && !matches!(a, App::Welcome | App::Installer | App::About));
        for (i, app) in grid.take(2 * PER_ROW).enumerate() {
            let (col, row) = ((i % PER_ROW) as i32, (i / PER_ROW) as i32);
            push(
                Target::App(app),
                Rect::new(
                    left + col * CELL_W,
                    p.y + GRID_Y + row * (CELL_H + 8),
                    CELL_W,
                    CELL_H,
                ),
            );
        }
        let mut x = 24;
        for (i, path) in self.programs.iter().enumerate() {
            let r = Self::chip(p, x, Self::program_name(path));
            if r.right() > p.right() - 24 {
                break;
            }
            push(Target::Program(i), r);
            x += r.w + 10;
        }
        let get = Self::chip(p, x, "Get programs");
        if get.right() <= p.right() - 24 {
            push(Target::GetPrograms, get);
        }
        (out, n)
    }

    fn target_at(&self, p: Rect, x: i32, y: i32) -> Option<Target> {
        let (targets, n) = self.targets(p);
        targets[..n]
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|&(t, _)| t)
    }

    // ---- input -------------------------------------------------------------

    /// Update the hover highlight. Returns whether it changed.
    pub fn set_hover(&mut self, p: Rect, x: i32, y: i32) -> bool {
        let hover = self.target_at(p, x, y);
        self.hover.set(hover)
    }

    pub fn on_click(&mut self, p: Rect, x: i32, y: i32) -> Action {
        match self.target_at(p, x, y) {
            Some(Target::App(app)) => Action::Open(app),
            Some(Target::Program(i)) => Action::Program(i),
            Some(Target::GetPrograms) => Action::GetPrograms,
            Some(Target::Lock) => Action::Lock,
            Some(Target::SignOut) => Action::SignOut,
            Some(Target::Restart) => Action::Restart,
            Some(Target::ShutDown) => Action::ShutDown,
            None => Action::None,
        }
    }

    pub fn on_key(&mut self, key: Key) -> Action {
        match key {
            Key::Escape | Key::Super => Action::Close,
            Key::Enter => match self.matches().next() {
                Some(app) if !self.search.as_str().is_empty() => Action::Open(app),
                _ => Action::None,
            },
            Key::Backspace => {
                self.search.pop();
                Action::Redraw
            }
            Key::Char(c) if !c.is_control() && self.search.len() + c.len_utf8() < 32 => {
                let mut buf = [0u8; 4];
                self.search.push_str(c.encode_utf8(&mut buf));
                self.hover.jump(None);
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, p: Rect, icons: &Icons, blink: bool) {
        c.shadow(p, RADIUS, 16, 4, 120);
        {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(p, RADIUS);
            m.fill_round_alpha(p, 0, theme::panel(), 250);
            // a band of the accent colour behind the user and the buttons
            let band = Rect::new(p.x, p.y, p.w, 150);
            m.vertical_gradient(
                band,
                mix(theme::panel(), theme::accent(), 60),
                theme::panel(),
            );
            self.draw_header(&mut m, p, icons);
            self.draw_search(&mut m, p, blink);
            if self.search.as_str().is_empty() {
                self.draw_grid(&mut m, p, icons);
            } else {
                self.draw_results(&mut m, p, icons);
            }
        }
        c.outline_round(p, RADIUS, theme::frame());
    }

    fn highlight(&self, c: &mut Canvas, target: Target, r: Rect, radius: i32) {
        let level = self.hover.level(target);
        if level > 0 {
            c.fill_round_alpha(r, radius, theme::control_lit(), level);
            c.outline_round_alpha(r, radius, theme::stroke(), level);
        }
    }

    /// Hover level of a target as a `mix` amount.
    fn lit(&self, target: Target) -> u32 {
        (self.hover.level(target) * 255 / ONE) as u32
    }

    fn draw_search(&self, c: &mut Canvas, p: Rect, blink: bool) {
        let r = Self::search_box(p);
        c.fill_round(r, r.h / 2, theme::control_lit());
        c.outline_round(r, r.h / 2, theme::frame());
        super::search::magnifier(c, r.x + 20, r.y + r.h / 2 - 2, 1, theme::text_dim());
        let ty = r.y + (r.h - UI.line_height) / 2;
        if self.search.as_str().is_empty() {
            c.draw_text(r.x + 40, ty, "Search apps", theme::text_dim());
            if blink {
                c.fill_rect(r.x + 40, ty, 1, UI.line_height, theme::text());
            }
        } else {
            let w = c.draw_text(r.x + 40, ty, self.search.as_str(), theme::text());
            if blink {
                c.fill_rect(r.x + 41 + w, ty, 1, UI.line_height, theme::text());
            }
        }
    }

    fn draw_results(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        let heading = "Best match";
        c.draw_text_in(&UI_BOLD, p.x + 32, p.y + GRID_Y - 30, heading, theme::text());
        let (targets, n) = self.targets(p);
        let mut first = true;
        for &(t, r) in &targets[..n] {
            let Target::App(app) = t else {
                continue;
            };
            if first {
                // Enter opens this one
                c.fill_round(r, 12, theme::accent_light());
                first = false;
            }
            self.highlight(c, t, r, 12);
            icons.draw_medium(c, app, r.x + 12, r.y + 12);
            c.draw_text(r.x + 48, r.y + 6, app.title(), theme::text());
            c.draw_text(r.x + 48, r.y + 25, "App", theme::text_dim());
        }
        if first {
            let text = "No apps match your search";
            c.draw_text(p.x + 32, p.y + GRID_Y, text, theme::text_dim());
        }
    }

    fn draw_grid(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        c.draw_text_in(&UI_BOLD, p.x + 32, p.y + GRID_Y - 30, "Apps", theme::text());
        let rec_y = p.y + PROGRAMS_Y;
        c.fill_rect(p.x + 24, rec_y - 12, p.w - 48, 1, theme::stroke());
        c.draw_text_in(&UI_BOLD, p.x + 32, rec_y, "Programs", theme::text());
        let (targets, n) = self.targets(p);
        for &(t, r) in &targets[..n] {
            match t {
                Target::App(app) => {
                    self.highlight(c, t, r, 16);
                    // icons rise a little under the mouse, as in the dock
                    let lift = 3 * self.hover.level(t) / ONE;
                    let x = r.x + (r.w - LARGE as i32) / 2;
                    icons.draw_large(c, app, x, r.y + 10 - lift);
                    let label = Rect::new(r.x, r.y + 64, r.w, 20);
                    // a short name, so neighbours don't run together
                    let title = match app {
                        App::About => "About",
                        _ => app.title(),
                    };
                    c.text_centered(label, title, theme::text());
                }
                Target::Program(i) => {
                    let face = mix(theme::control(), theme::control_lit(), self.lit(t));
                    c.fill_round(r, r.h / 2, face);
                    c.outline_round(r, r.h / 2, theme::stroke());
                    let name = Self::program_name(&self.programs[i]);
                    // the first letter on a round tile stands for the icon
                    let tile = Rect::new(r.x + 10, r.y + 8, 24, 24);
                    c.fill_round(tile, 12, theme::accent());
                    let mut buf = [0u8; 4];
                    let first = name.chars().next().unwrap_or('?').to_ascii_uppercase();
                    c.text_centered_in(&UI_BOLD, tile, first.encode_utf8(&mut buf), theme::on_accent());
                    c.draw_text(r.x + 42, r.y + 11, name, theme::text());
                }
                Target::GetPrograms => {
                    let face = mix(theme::accent_light(), theme::control_lit(), self.lit(t) / 2);
                    c.fill_round(r, r.h / 2, face);
                    c.outline_round(r, r.h / 2, mix(theme::accent(), theme::panel(), 140));
                    let (cx, cy) = (r.x + 22, r.y + 20);
                    c.fill_rect(cx - 6, cy - 1, 12, 2, theme::accent());
                    c.fill_rect(cx - 1, cy - 6, 2, 12, theme::accent());
                    c.draw_text(r.x + 42, r.y + 11, "Get programs", theme::accent());
                }
                _ => {}
            }
        }
    }

    /// The user at the top left, the power buttons at the top right.
    fn draw_header(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        let f = Rect::new(p.x, p.y + 2, p.w, 76);
        let avatar = Rect::new(f.x + 24, f.y + 18, 40, 40);
        c.fill_round(avatar, 20, theme::accent());
        let name = users::current_name().unwrap_or_default();
        let mut initial = [0u8; 4];
        let initial = name
            .as_str()
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .encode_utf8(&mut initial);
        c.text_centered_in(&UI_BOLD, avatar, initial, theme::on_accent());
        // the logo sits on the avatar's corner
        icons.draw_logo(c, 20, avatar.right() - 14, avatar.bottom() - 16);
        c.draw_text_in(&UI_BOLD, f.x + 76, f.y + 18, name.as_str(), theme::text());

        // under the name: what the button under the mouse does
        let mut hint = "RyzikOS 1.0";
        for (i, (t, label)) in POWER.into_iter().enumerate() {
            let r = Self::power_button(p, i);
            let lit = self.lit(t);
            if lit > 0 {
                hint = label;
            }
            let bg = mix(theme::control(), theme::control_lit(), lit);
            let bg = if t == Target::ShutDown && lit > 0 {
                mix(bg, WARM, lit)
            } else {
                bg
            };
            c.fill_round(r, 20, bg);
            c.outline_round(r, 20, theme::stroke());
            let ink = if t == Target::ShutDown && lit > 128 {
                0xffffff
            } else {
                theme::text()
            };
            let (x, y) = (r.x + 20, r.y + 20);
            match t {
                Target::Lock => lock_symbol(c, x, y, ink, bg),
                Target::SignOut => sign_out_symbol(c, x, y, ink),
                Target::Restart => restart_symbol(c, x, y, ink, bg),
                _ => power_symbol(c, x, y, ink, bg),
            }
        }
        c.draw_text(f.x + 76, f.y + 38, hint, theme::text_dim());
    }
}

/// The warm orange of the RyzikOS logo, for Shut down.
const WARM: Color = rgb(0xff, 0x9f, 0x43);

/// A power symbol centred at (x, y): a ring open at the top and a bar.
pub fn power_symbol(c: &mut Canvas, x: i32, y: i32, fg: Color, bg: Color) {
    let ring = Rect::new(x - 8, y - 8, 16, 16);
    c.outline_round(ring, 8, fg);
    c.outline_round(ring.inset(1), 7, fg);
    c.fill_rect(x - 3, y - 9, 6, 7, bg);
    c.fill_rect(x - 1, y - 10, 2, 9, fg);
}

/// A circular arrow centred at (x, y).
pub fn restart_symbol(c: &mut Canvas, x: i32, y: i32, fg: Color, bg: Color) {
    let ring = Rect::new(x - 8, y - 8, 16, 16);
    c.outline_round(ring, 8, fg);
    c.outline_round(ring.inset(1), 7, fg);
    c.fill_rect(x, y - 9, 8, 7, bg);
    c.fill_round(Rect::new(x + 1, y - 10, 6, 6), 2, fg);
}

/// A padlock centred at (x, y).
fn lock_symbol(c: &mut Canvas, x: i32, y: i32, fg: Color, bg: Color) {
    let shackle = Rect::new(x - 5, y - 9, 10, 12);
    c.outline_round(shackle, 5, fg);
    c.outline_round(shackle.inset(1), 4, fg);
    c.fill_rect(x - 3, y - 4, 6, 4, bg);
    c.fill_round(Rect::new(x - 8, y - 2, 16, 11), 2, fg);
    c.fill_rect(x - 1, y + 2, 2, 3, bg);
}

/// An arrow leaving a door, centred at (x, y).
fn sign_out_symbol(c: &mut Canvas, x: i32, y: i32, fg: Color) {
    c.fill_rect(x - 8, y - 8, 2, 16, fg);
    c.fill_rect(x - 8, y - 8, 8, 2, fg);
    c.fill_rect(x - 8, y + 6, 8, 2, fg);
    c.fill_rect(x - 3, y - 1, 12, 2, fg);
    for i in 0..4 {
        c.fill_rect(x + 5 - i, y - 4 + i, 2, 1, fg);
        c.fill_rect(x + 5 - i, y + 3 - i, 2, 1, fg);
    }
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.len() > h.len() {
        return false;
    }
    (0..=h.len() - n.len()).any(|i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}
