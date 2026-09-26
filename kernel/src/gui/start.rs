//! The start menu, in the style of Windows 11: a search box, pinned apps,
//! recently opened apps, an "All apps" list and a power button.
//!
//! Typing while the menu is open searches the apps; Enter starts the
//! best match.

use super::anim::{self, Fader, ONE};
use super::canvas::{mix, Canvas, Color, Rect};
use super::icons::Icons;
use super::text::{UI, UI_BOLD};
use super::theme;
use super::{App, APPS};
use crate::keyboard::Key;
use crate::{users, StackString};

pub const W: i32 = 640;
pub const H: i32 = 540;
const RADIUS: i32 = 8;
const FOOTER: i32 = 64;
const MAX_RECENT: usize = 4;
const MAX_TARGETS: usize = 20;

/// What the desktop should do after an event.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// The menu changed and must be drawn again.
    Redraw,
    Open(App),
    Close,
    Restart,
    ShutDown,
    Lock,
    SignOut,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Pinned,
    AllApps,
}

/// Something in the menu that can be clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    App(App),
    AllApps,
    Back,
    Power,
    Restart,
    ShutDown,
    User,
    Lock,
    SignOut,
    /// The Sign out button in the footer.
    SignOutButton,
}

pub struct StartMenu {
    pub open: bool,
    view: View,
    search: StackString<32>,
    power_open: bool,
    user_open: bool,
    /// Most recently opened first.
    recent: [Option<App>; MAX_RECENT],
    hover: Fader<Target>,
}

/// Apps in alphabetical order, for "All apps".
const SORTED: [App; 9] = [
    App::About,
    App::Browser,
    App::Calculator,
    App::Explorer,
    App::Demo,
    App::Notepad,
    App::Paint,
    App::Settings,
    App::Terminal,
];

/// Pinned apps in a row, and where "Recommended" starts below them.
const PINNED_PER_ROW: usize = 6;
const RECOMMENDED_Y: i32 = 316;

impl StartMenu {
    pub fn new() -> Self {
        Self {
            open: false,
            view: View::Pinned,
            search: StackString::new(),
            power_open: false,
            user_open: false,
            recent: [None; MAX_RECENT],
            hover: Fader::new(anim::ms(120)),
        }
    }

    /// Where the menu panel sits on a screen of the given size.
    pub fn panel(width: i32, height: i32, taskbar: i32) -> Rect {
        Rect::new((width - W) / 2, height - taskbar - 12 - H, W, H)
    }

    pub fn show(&mut self) {
        self.open = true;
        self.view = View::Pinned;
        self.search.clear();
        self.power_open = false;
        self.user_open = false;
        self.hover.jump(None);
    }

    /// Fade hover highlights. Returns whether the menu must be redrawn.
    pub fn tick(&mut self) -> bool {
        self.hover.tick()
    }

    /// Remember an app for "Recommended".
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
            .filter(|a| contains_ignore_case(a.title(), self.search.as_str()))
    }

    // ---- layout ------------------------------------------------------------

    fn search_box(p: Rect) -> Rect {
        Rect::new(p.x + 32, p.y + 24, p.w - 64, 36)
    }

    fn footer(p: Rect) -> Rect {
        Rect::new(p.x, p.bottom() - FOOTER, p.w, FOOTER)
    }

    fn power_flyout(p: Rect) -> Rect {
        Rect::new(p.right() - 196, p.bottom() - FOOTER - 92, 180, 88)
    }

    fn user_flyout(p: Rect) -> Rect {
        Rect::new(p.x + 32, p.bottom() - FOOTER - 92, 180, 88)
    }

    /// Everything clickable, and where it is.
    fn targets(&self, p: Rect) -> ([(Target, Rect); MAX_TARGETS], usize) {
        let mut out = [(Target::Back, Rect::default()); MAX_TARGETS];
        let mut n = 0;
        let mut push = |t: Target, r: Rect| {
            if n < MAX_TARGETS {
                out[n] = (t, r);
                n += 1;
            }
        };
        if self.power_open {
            let f = Self::power_flyout(p);
            push(Target::Restart, Rect::new(f.x + 4, f.y + 4, f.w - 8, 38));
            push(Target::ShutDown, Rect::new(f.x + 4, f.y + 46, f.w - 8, 38));
        }
        if self.user_open {
            let f = Self::user_flyout(p);
            push(Target::Lock, Rect::new(f.x + 4, f.y + 4, f.w - 8, 38));
            push(Target::SignOut, Rect::new(f.x + 4, f.y + 46, f.w - 8, 38));
        }
        let footer = Self::footer(p);
        push(
            Target::User,
            Rect::new(footer.x + 32, footer.y + 10, 200, 44),
        );
        push(
            Target::Power,
            Rect::new(footer.right() - 64, footer.y + 12, 40, 40),
        );
        push(
            Target::SignOutButton,
            Rect::new(footer.right() - 184, footer.y + 12, 112, 40),
        );

        if !self.search.as_str().is_empty() {
            for (i, app) in self.matches().enumerate() {
                push(
                    Target::App(app),
                    Rect::new(p.x + 32, p.y + 112 + i as i32 * 52, p.w - 64, 48),
                );
            }
            return (out, n);
        }
        match self.view {
            View::Pinned => {
                push(
                    Target::AllApps,
                    Rect::new(p.right() - 144, p.y + 80, 112, 28),
                );
                for (i, app) in APPS.into_iter().enumerate() {
                    let (col, row) = ((i % PINNED_PER_ROW) as i32, (i / PINNED_PER_ROW) as i32);
                    push(
                        Target::App(app),
                        Rect::new(p.x + 32 + col * 96, p.y + 120 + row * 90, 96, 88),
                    );
                }
                for (i, app) in self.recent.into_iter().flatten().enumerate() {
                    let (col, row) = ((i % 2) as i32, (i / 2) as i32);
                    let w = (p.w - 64) / 2;
                    push(
                        Target::App(app),
                        Rect::new(
                            p.x + 32 + col * w,
                            p.y + RECOMMENDED_Y + 28 + row * 56,
                            w - 8,
                            52,
                        ),
                    );
                }
            }
            View::AllApps => {
                push(Target::Back, Rect::new(p.right() - 120, p.y + 80, 88, 28));
                for (i, app) in SORTED.into_iter().enumerate() {
                    push(
                        Target::App(app),
                        Rect::new(p.x + 32, p.y + 120 + i as i32 * 48, p.w - 64, 44),
                    );
                }
            }
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
        let target = self.target_at(p, x, y);
        if self.power_open && !matches!(target, Some(Target::Restart | Target::ShutDown)) {
            self.power_open = false;
            return Action::Redraw;
        }
        if self.user_open && !matches!(target, Some(Target::Lock | Target::SignOut)) {
            self.user_open = false;
            return Action::Redraw;
        }
        match target {
            Some(Target::App(app)) => Action::Open(app),
            Some(Target::AllApps) => {
                self.view = View::AllApps;
                Action::Redraw
            }
            Some(Target::Back) => {
                self.view = View::Pinned;
                Action::Redraw
            }
            Some(Target::Power) => {
                self.power_open = true;
                Action::Redraw
            }
            Some(Target::User) => {
                self.user_open = true;
                Action::Redraw
            }
            Some(Target::Restart) => Action::Restart,
            Some(Target::ShutDown) => Action::ShutDown,
            Some(Target::Lock) => Action::Lock,
            Some(Target::SignOut | Target::SignOutButton) => Action::SignOut,
            None => Action::None,
        }
    }

    pub fn on_key(&mut self, key: Key) -> Action {
        match key {
            Key::Escape | Key::Super => {
                if self.power_open || self.user_open {
                    self.power_open = false;
                    self.user_open = false;
                    Action::Redraw
                } else {
                    Action::Close
                }
            }
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
            self.draw_search(&mut m, p, blink);
            if !self.search.as_str().is_empty() {
                self.draw_results(&mut m, p, icons);
            } else {
                match self.view {
                    View::Pinned => self.draw_pinned(&mut m, p, icons),
                    View::AllApps => self.draw_all_apps(&mut m, p, icons),
                }
            }
            self.draw_footer(&mut m, p);
        }
        c.outline_round(p, RADIUS, theme::frame());
    }

    fn highlight(&self, c: &mut Canvas, target: Target, r: Rect) {
        let level = self.hover.level(target);
        if level > 0 {
            c.fill_round_alpha(r, 6, theme::control_lit(), level);
            c.outline_round_alpha(r, 6, theme::stroke(), level);
        }
    }

    /// A button face that lights up to white under the mouse.
    fn face(&self, target: Target, base: Color) -> Color {
        mix(base, theme::control_lit(), self.lit(target))
    }

    /// Hover level of a target as a `mix` amount.
    fn lit(&self, target: Target) -> u32 {
        (self.hover.level(target) * 255 / ONE) as u32
    }

    fn draw_search(&self, c: &mut Canvas, p: Rect, blink: bool) {
        let r = Self::search_box(p);
        c.fill_round(r, r.h / 2, theme::light());
        c.outline_round(r, r.h / 2, theme::frame());
        c.fill_rect(r.x + 18, r.bottom() - 1, r.w - 36, 1, theme::accent());
        // magnifying glass
        let lens = Rect::new(r.x + 16, r.y + 10, 12, 12);
        c.outline_round(lens, 6, theme::text());
        c.outline_round(lens.inset(1), 5, theme::text());
        for i in 0..2 {
            c.line(
                r.x + 26 + i,
                r.y + 21,
                r.x + 30 + i,
                r.y + 25,
                theme::text(),
            );
        }
        let ty = r.y + (r.h - UI.line_height) / 2;
        if self.search.as_str().is_empty() {
            c.draw_text(r.x + 40, ty, "Type here to search apps", theme::text_dim());
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
        c.draw_text_in(&UI_BOLD, p.x + 40, p.y + 84, "Best match", theme::text());
        let (targets, n) = self.targets(p);
        let mut first = true;
        for &(t, r) in &targets[..n] {
            let Target::App(app) = t else {
                continue;
            };
            if first {
                // Enter opens this one
                c.fill_round(r, 6, theme::accent_light());
                first = false;
            }
            self.highlight(c, t, r);
            icons.draw_medium(c, app, r.x + 12, r.y + 12);
            c.draw_text(r.x + 48, r.y + 6, app.title(), theme::text());
            c.draw_text(r.x + 48, r.y + 25, "App", theme::text_dim());
        }
        if first {
            let text = "No apps match your search";
            c.draw_text(p.x + 40, p.y + 120, text, theme::text_dim());
        }
    }

    fn draw_pinned(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        c.draw_text_in(&UI_BOLD, p.x + 56, p.y + 86, "Pinned", theme::text());
        let rec_y = p.y + RECOMMENDED_Y;
        c.draw_text_in(&UI_BOLD, p.x + 56, rec_y, "Recommended", theme::text());
        let (targets, n) = self.targets(p);
        let mut recent = 0;
        for &(t, r) in &targets[..n] {
            match t {
                Target::AllApps => {
                    let face = self.face(t, theme::control());
                    c.fill_round(r, 4, face);
                    c.outline_round(r, 4, theme::stroke());
                    c.text_centered(r, "All apps  ›", theme::text());
                }
                // pinned apps are in the grid, recent ones below it
                Target::App(app) if r.y < rec_y => {
                    self.highlight(c, t, r);
                    icons.draw_large(c, app, r.x + 24, r.y + 8);
                    c.text_centered(
                        Rect::new(r.x, r.y + 60, r.w, 20),
                        app.title(),
                        theme::text(),
                    );
                }
                Target::App(app) => {
                    recent += 1;
                    self.highlight(c, t, r);
                    icons.draw_medium(c, app, r.x + 12, r.y + 14);
                    c.draw_text(r.x + 48, r.y + 7, app.title(), theme::text());
                    c.draw_text(r.x + 48, r.y + 26, "Recently opened", theme::text_dim());
                }
                _ => {}
            }
        }
        if recent == 0 {
            let text = "Apps you open will show up here.";
            c.draw_text(p.x + 56, rec_y + 36, text, theme::text_dim());
        }
    }

    fn draw_all_apps(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        c.draw_text_in(&UI_BOLD, p.x + 56, p.y + 86, "All apps", theme::text());
        let (targets, n) = self.targets(p);
        for &(t, r) in &targets[..n] {
            match t {
                Target::Back => {
                    let face = self.face(t, theme::control());
                    c.fill_round(r, 4, face);
                    c.outline_round(r, 4, theme::stroke());
                    c.text_centered(r, "‹  Back", theme::text());
                }
                Target::App(app) => {
                    self.highlight(c, t, r);
                    icons.draw_medium(c, app, r.x + 12, r.y + 10);
                    c.draw_text(r.x + 48, r.y + 13, app.title(), theme::text());
                }
                _ => {}
            }
        }
    }

    fn draw_footer(&self, c: &mut Canvas, p: Rect) {
        let f = Self::footer(p);
        c.fill(f, theme::footer());
        c.fill_rect(f.x, f.y, f.w, 1, theme::stroke());
        let (targets, n) = self.targets(p);
        for &(t, r) in &targets[..n] {
            match t {
                Target::Power | Target::User => {
                    let open = if t == Target::Power {
                        self.power_open
                    } else {
                        self.user_open
                    };
                    let lit = if open { 255 } else { self.lit(t) };
                    let bg = mix(theme::footer(), theme::control_lit(), lit);
                    if lit > 0 {
                        c.fill_round(r, 6, bg);
                    }
                    if t == Target::Power {
                        power_symbol(c, r.x + 20, r.y + 20, theme::text(), bg);
                    }
                }
                Target::SignOutButton => {
                    let bg = mix(theme::footer(), theme::control_lit(), self.lit(t));
                    c.fill_round(r, 6, bg);
                    c.outline_round(r, 6, theme::stroke());
                    sign_out_symbol(c, r.x + 22, r.y + 20);
                    c.draw_text(r.x + 40, r.y + 11, "Sign out", theme::text());
                }
                _ => {}
            }
        }
        let avatar = Rect::new(f.x + 44, f.y + 16, 32, 32);
        c.fill_round(avatar, 16, theme::accent());
        let name = users::current_name().unwrap_or_default();
        let mut initial = [0u8; 4];
        let initial = name
            .as_str()
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .encode_utf8(&mut initial);
        c.text_centered_in(&UI_BOLD, avatar, initial, 0xffffff);
        c.draw_text(f.x + 88, f.y + 23, name.as_str(), theme::text());

        for (open, fl) in [
            (self.power_open, Self::power_flyout(p)),
            (self.user_open, Self::user_flyout(p)),
        ] {
            if !open {
                continue;
            }
            c.shadow(fl, 8, 10, 2, 90);
            c.fill_round(fl, 8, theme::menu());
            c.outline_round(fl, 8, theme::stroke());
            for &(t, r) in &targets[..n] {
                let label = match t {
                    Target::Restart => "Restart",
                    Target::ShutDown => "Shut down",
                    Target::Lock => "Lock",
                    Target::SignOut => "Sign out",
                    _ => continue,
                };
                let bg = mix(theme::menu(), theme::hover(), self.lit(t));
                if self.lit(t) > 0 {
                    c.fill_round(r, 5, bg);
                }
                match t {
                    Target::Restart => restart_symbol(c, r.x + 20, r.y + 19, theme::text(), bg),
                    Target::ShutDown => power_symbol(c, r.x + 20, r.y + 19, theme::text(), bg),
                    Target::Lock => lock_symbol(c, r.x + 20, r.y + 19, bg),
                    _ => sign_out_symbol(c, r.x + 20, r.y + 19),
                }
                c.draw_text(r.x + 40, r.y + 10, label, theme::text());
            }
        }
    }
}

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
fn lock_symbol(c: &mut Canvas, x: i32, y: i32, bg: Color) {
    let shackle = Rect::new(x - 5, y - 9, 10, 12);
    c.outline_round(shackle, 5, theme::text());
    c.outline_round(shackle.inset(1), 4, theme::text());
    c.fill_rect(x - 3, y - 4, 6, 4, bg);
    c.fill_round(Rect::new(x - 8, y - 2, 16, 11), 2, theme::text());
    c.fill_rect(x - 1, y + 2, 2, 3, bg);
}

/// An arrow leaving a door, centred at (x, y).
fn sign_out_symbol(c: &mut Canvas, x: i32, y: i32) {
    c.fill_rect(x - 8, y - 8, 2, 16, theme::text());
    c.fill_rect(x - 8, y - 8, 8, 2, theme::text());
    c.fill_rect(x - 8, y + 6, 8, 2, theme::text());
    c.fill_rect(x - 3, y - 1, 12, 2, theme::text());
    for i in 0..4 {
        c.fill_rect(x + 5 - i, y - 4 + i, 2, 1, theme::text());
        c.fill_rect(x + 5 - i, y + 3 - i, 2, 1, theme::text());
    }
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.len() > h.len() {
        return false;
    }
    (0..=h.len() - n.len()).any(|i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}
