//! Search, opened from the dock: typing in the box at the top of the
//! panel finds apps (by their English and Russian names) and
//! files and folders on the disk. Results come in groups under the best
//! match, the right side shows the chosen one with what can be done
//! with it, and Enter opens it.
//!
//! The disk is read once into a list of names when the search opens, and
//! again only after files change, so typing stays quick.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use super::canvas::{Canvas, Rect};
use super::icons::{Icons, LARGE, MEDIUM};
use super::text::{UI, UI_BOLD};
use super::theme;
use super::widgets;
use super::{App, APPS};
use crate::fs;
use crate::keyboard::Key;
use crate::serial;

pub const W: i32 = 800;
pub const H: i32 = 636;
/// The strip at the top with the search box.
const FIELD: i32 = 60;
const RADIUS: i32 = 16;
/// How much of the disk is read, at most.
const MAX_ENTRIES: usize = 4000;
const MAX_DEPTH: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    All,
    Apps,
    Documents,
    Folders,
}

const TABS: [(Tab, &str); 4] = [
    (Tab::All, "All"),
    (Tab::Apps, "Apps"),
    (Tab::Documents, "Documents"),
    (Tab::Folders, "Folders"),
];

/// Something that was found.
#[derive(Clone, PartialEq, Eq)]
pub enum Hit {
    App(App),
    Path { path: String, dir: bool },
}

impl Hit {
    fn kind(&self) -> Tab {
        match self {
            Hit::App(_) => Tab::Apps,
            Hit::Path { dir: true, .. } => Tab::Folders,
            Hit::Path { .. } => Tab::Documents,
        }
    }

    fn name(&self) -> String {
        match self {
            Hit::App(a) => String::from(a.title()),
            Hit::Path { path, .. } => String::from(fs::file_name(path)),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Hit::App(_) => "App",
            Hit::Path { dir: true, .. } => "File folder",
            Hit::Path { path, .. } => {
                let name = fs::file_name(path);
                match name.rfind('.') {
                    Some(i) if fs::same_name(&name[i + 1..], "txt") => "Text Document",
                    _ => "File",
                }
            }
        }
    }

    /// Where it is, the Windows way, for files and folders.
    fn location(&self) -> Option<String> {
        match self {
            Hit::App(_) => None,
            Hit::Path { path, .. } => Some(fs::display(&fs::parent(path))),
        }
    }
}

/// What the desktop should do after an event.
pub enum Action {
    None,
    Redraw,
    Close,
    Open(Hit),
    /// Show the folder a file or folder is in.
    Location(String),
    Pin(App),
    Unpin(App),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Tab(Tab),
    Result(usize),
    Top(usize),
    Open,
    Location,
    Pin,
}

struct Entry {
    lower: String,
    path: String,
    dir: bool,
}

pub struct Search {
    pub open: bool,
    pub query: String,
    tab: Tab,
    index: Vec<Entry>,
    /// `fs::changes()` when the disk was read.
    indexed: Option<u32>,
    /// In the order shown: the best match, then apps, folders, files.
    results: Vec<Hit>,
    selected: usize,
    hover: Option<Target>,
}

impl Search {
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            tab: Tab::All,
            index: Vec::new(),
            indexed: None,
            results: Vec::new(),
            selected: 0,
            hover: None,
        }
    }

    /// Where the panel sits: centred above the dock, under the menu bar.
    pub fn panel(width: i32, height: i32, dock: i32, menu_bar: i32) -> Rect {
        let y = (height - dock - 12 - H).max(menu_bar + 8);
        Rect::new((width - W) / 2, y, W, H)
    }

    /// The search box at the top of the panel `p`.
    pub fn field(p: Rect) -> Rect {
        Rect::new(p.x + 24, p.y + 16, p.w - 48, 40)
    }

    /// The panel under the search box.
    fn body(p: Rect) -> Rect {
        Rect::new(p.x, p.y + FIELD, p.w, p.h - FIELD)
    }

    pub fn show(&mut self) {
        self.open = true;
        self.query.clear();
        self.tab = Tab::All;
        self.hover = None;
        self.reindex();
        self.update();
    }

    /// Forget the files, after another user signs in.
    pub fn forget(&mut self) {
        self.index.clear();
        self.indexed = None;
    }

    fn reindex(&mut self) {
        if self.indexed == Some(fs::changes()) {
            return;
        }
        self.indexed = Some(fs::changes());
        self.index.clear();
        let mut queue = VecDeque::new();
        queue.push_back((String::from("/"), 0));
        while let Some((dir, depth)) = queue.pop_front() {
            let Ok(items) = fs::list(&dir) else {
                continue;
            };
            for item in items {
                if self.index.len() >= MAX_ENTRIES {
                    break;
                }
                let path = fs::join(&dir, &item.name);
                if item.dir && depth < MAX_DEPTH {
                    queue.push_back((path.clone(), depth + 1));
                }
                self.index.push(Entry {
                    lower: item.name.to_lowercase(),
                    path,
                    dir: item.dir,
                });
            }
        }
        let mut line = crate::StackString::<48>::new();
        let _ = writeln!(line, "search: indexed {} items", self.index.len());
        serial::write_str(line.as_str());
    }

    /// Find what matches the query.
    fn update(&mut self) {
        self.selected = 0;
        self.results.clear();
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return;
        }
        let mut found: Vec<(u32, Hit)> = Vec::new();
        if matches!(self.tab, Tab::All | Tab::Apps) {
            for app in APPS.into_iter().filter(|a| a.listed()) {
                let title = app.title().to_lowercase();
                // other words count less than the name itself
                let best = core::iter::once(score(&title, &q))
                    .chain(
                        app.keywords()
                            .split(' ')
                            .map(|w| score(w, &q).map(|s| s.min(2))),
                    )
                    .flatten()
                    .max();
                if let Some(s) = best {
                    // apps win a tie with files
                    found.push((s * 2 + 1, Hit::App(app)));
                }
            }
        }
        for e in &self.index {
            let wanted = match self.tab {
                Tab::All => true,
                Tab::Apps => false,
                Tab::Folders => e.dir,
                Tab::Documents => !e.dir,
            };
            if !wanted {
                continue;
            }
            if let Some(s) = score(&e.lower, &q) {
                let hit = Hit::Path {
                    path: e.path.clone(),
                    dir: e.dir,
                };
                found.push((s * 2, hit));
            }
        }
        // best first; shorter names first among equals
        found.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.name().len().cmp(&b.1.name().len()))
        });
        let Some((_, best)) = found.first().cloned() else {
            return;
        };
        self.results.push(best.clone());
        for kind in [Tab::Apps, Tab::Folders, Tab::Documents] {
            let limit = if self.tab == Tab::All { 5 } else { 12 };
            let group = found
                .iter()
                .filter(|(_, h)| h.kind() == kind && *h != best)
                .take(limit)
                .map(|(_, h)| h.clone());
            self.results.extend(group);
        }
    }

    // ---- layout ----------------------------------------------------------------

    fn tab_rects(p: Rect) -> [(Tab, &'static str, Rect); 4] {
        let mut x = p.x + 24;
        TABS.map(|(t, label)| {
            let w = UI.width(label) + 28;
            let r = Rect::new(x, p.y + 14, w, 34);
            x += w + 4;
            (t, label, r)
        })
    }

    fn list_area(p: Rect) -> Rect {
        Rect::new(p.x + 16, p.y + 64, 408, p.h - 80)
    }

    fn detail_area(p: Rect) -> Rect {
        Rect::new(p.x + 440, p.y + 64, p.w - 456, p.h - 80)
    }

    /// Result rows that fit, with the heading drawn above each group.
    fn rows(&self, p: Rect) -> Vec<(usize, Rect, Option<&'static str>)> {
        let area = Self::list_area(p);
        let mut out = Vec::new();
        let mut y = area.y;
        for (i, hit) in self.results.iter().enumerate() {
            let heading = if i == 0 {
                Some("Best match")
            } else if hit.kind() != self.results[i - 1].kind() || i == 1 {
                Some(match hit.kind() {
                    Tab::Apps => "Apps",
                    Tab::Folders => "Folders",
                    _ => "Documents",
                })
            } else {
                None
            };
            let top = y + if heading.is_some() { 30 } else { 0 };
            let h = if i == 0 { 64 } else { 40 };
            if top + h > area.bottom() {
                break;
            }
            out.push((i, Rect::new(area.x, top, area.w, h), heading));
            y = top + h + 2;
        }
        out
    }

    /// The buttons under the chosen result.
    fn actions(&self, p: Rect) -> Vec<(Target, Rect)> {
        let Some(hit) = self.results.get(self.selected) else {
            return Vec::new();
        };
        let d = Self::detail_area(p);
        let mut targets = alloc::vec![Target::Open];
        match hit {
            Hit::App(_) => targets.push(Target::Pin),
            Hit::Path { .. } => targets.push(Target::Location),
        }
        targets
            .into_iter()
            .enumerate()
            .map(|(k, t)| {
                (
                    t,
                    Rect::new(d.x + 12, d.y + 206 + k as i32 * 42, d.w - 24, 38),
                )
            })
            .collect()
    }

    fn top_apps(p: Rect, top: &[App]) -> Vec<(usize, Rect)> {
        (0..top.len().min(6))
            .map(|i| {
                let r = Rect::new(p.x + 32 + i as i32 * 124, p.y + 108, 116, 100);
                (i, r)
            })
            .collect()
    }

    fn target_at(&self, p: Rect, top: &[App], x: i32, y: i32) -> Option<Target> {
        if let Some((t, _, _)) = Self::tab_rects(p)
            .into_iter()
            .find(|(_, _, r)| r.contains(x, y))
        {
            return Some(Target::Tab(t));
        }
        if self.query.trim().is_empty() {
            return Self::top_apps(p, top)
                .into_iter()
                .find(|(_, r)| r.contains(x, y))
                .map(|(i, _)| Target::Top(i));
        }
        if let Some((i, _, _)) = self.rows(p).into_iter().find(|(_, r, _)| r.contains(x, y)) {
            return Some(Target::Result(i));
        }
        self.actions(p)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(t, _)| t)
    }

    // ---- input -----------------------------------------------------------------

    pub fn set_hover(&mut self, p: Rect, top: &[App], x: i32, y: i32) -> bool {
        let p = Self::body(p);
        let hover = self.target_at(p, top, x, y);
        core::mem::replace(&mut self.hover, hover) != hover
    }

    pub fn on_click(&mut self, p: Rect, top: &[App], pins: &[App], x: i32, y: i32) -> Action {
        let p = Self::body(p);
        match self.target_at(p, top, x, y) {
            Some(Target::Tab(t)) => {
                self.tab = t;
                self.update();
                Action::Redraw
            }
            Some(Target::Top(i)) => Action::Open(Hit::App(top[i])),
            Some(Target::Result(i)) => {
                if self.selected == i {
                    Action::Open(self.results[i].clone())
                } else {
                    self.selected = i;
                    Action::Redraw
                }
            }
            Some(Target::Open) => self.open_selected(),
            Some(Target::Location) => match self.results.get(self.selected) {
                Some(Hit::Path { path, .. }) => Action::Location(path.clone()),
                _ => Action::None,
            },
            Some(Target::Pin) => match self.results.get(self.selected) {
                Some(Hit::App(a)) if pins.contains(a) => Action::Unpin(*a),
                Some(Hit::App(a)) => Action::Pin(*a),
                _ => Action::None,
            },
            None => Action::None,
        }
    }

    fn open_selected(&self) -> Action {
        match self.results.get(self.selected) {
            Some(hit) => Action::Open(hit.clone()),
            None => Action::None,
        }
    }

    pub fn on_key(&mut self, key: Key) -> Action {
        match key {
            Key::Escape => Action::Close,
            Key::Enter => self.open_selected(),
            Key::Down if self.selected + 1 < self.results.len() => {
                self.selected += 1;
                Action::Redraw
            }
            Key::Up if self.selected > 0 => {
                self.selected -= 1;
                Action::Redraw
            }
            Key::Char('\t') => {
                // the next tab
                let i = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
                self.tab = TABS[(i + 1) % TABS.len()].0;
                self.update();
                Action::Redraw
            }
            Key::Backspace => {
                self.query.pop();
                self.reindex();
                self.update();
                Action::Redraw
            }
            Key::Char(c) if !c.is_control() && self.query.chars().count() < 40 => {
                self.query.push(c);
                self.reindex();
                self.update();
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    // ---- drawing ---------------------------------------------------------------

    fn highlight(&self, c: &mut Canvas, t: Target, r: Rect) {
        if self.hover == Some(t) {
            c.fill_round(r, 6, theme::hover());
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        c: &mut Canvas,
        p: Rect,
        icons: &Icons,
        top: &[App],
        pins: &[App],
        blink: bool,
    ) {
        c.shadow(p, RADIUS, 16, 4, 120);
        {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(p, RADIUS);
            m.fill(p, theme::panel());
            self.draw_field(&mut m, Self::field(p), blink);
            self.draw_inside(&mut m, Self::body(p), icons, top, pins);
        }
        c.outline_round(p, RADIUS, theme::frame());
    }

    /// The search box: a pill with the magnifier, the words typed and
    /// the caret.
    fn draw_field(&self, c: &mut Canvas, r: Rect, blink: bool) {
        c.fill_round(r, r.h / 2, theme::control_lit());
        c.outline_round(r, r.h / 2, theme::accent());
        magnifier(c, r.x + 22, r.y + r.h / 2 - 2, 1, theme::text());
        let ty = r.y + (r.h - UI.line_height) / 2;
        let q = self.query.as_str();
        if q.is_empty() {
            let hint = "Search apps, files and folders";
            c.draw_text(r.x + 42, ty, hint, theme::text_dim());
            if blink {
                c.fill_rect(r.x + 42, ty, 1, UI.line_height, theme::text());
            }
        } else {
            // the end of the text when it is too long
            let mut shown = q;
            while UI.width(shown) > r.w - 64 {
                let mut it = shown.chars();
                it.next();
                shown = it.as_str();
            }
            let w = c.draw_text(r.x + 42, ty, shown, theme::text());
            if blink {
                c.fill_rect(r.x + 43 + w, ty, 1, UI.line_height, theme::text());
            }
        }
    }

    fn draw_inside(&self, m: &mut Canvas, p: Rect, icons: &Icons, top: &[App], pins: &[App]) {
        for (t, label, r) in Self::tab_rects(p) {
            self.highlight(m, Target::Tab(t), r);
            let on = t == self.tab;
            let color = if on { theme::text() } else { theme::text_dim() };
            m.text_centered_in(if on { &UI_BOLD } else { &UI }, r, label, color);
            if on {
                let w = UI.width(label);
                let bar = Rect::new(r.x + (r.w - w) / 2, r.bottom() - 3, w, 3);
                m.fill_round(bar, 1, theme::accent());
            }
        }
        m.fill_rect(p.x, p.y + 52, p.w, 1, theme::stroke());

        if self.query.trim().is_empty() {
            self.draw_start(m, p, icons, top);
        } else if self.results.is_empty() {
            let r = Rect::new(p.x, p.y + 200, p.w, 24);
            m.text_centered(r, "No results. Try other words.", theme::text_dim());
        } else {
            self.draw_results(m, p, icons);
            self.draw_detail(m, p, icons, pins);
        }
    }

    fn draw_start(&self, c: &mut Canvas, p: Rect, icons: &Icons, top: &[App]) {
        c.draw_text_in(&UI_BOLD, p.x + 40, p.y + 76, "Top apps", theme::text());
        for (i, r) in Self::top_apps(p, top) {
            self.highlight(c, Target::Top(i), r);
            icons.draw(c, top[i], LARGE, r.x + (r.w - 48) / 2, r.y + 12);
            let label = Rect::new(r.x, r.y + 68, r.w, 20);
            c.text_centered(label, top[i].title(), theme::text());
        }
        let card = Rect::new(p.x + 32, p.y + 240, p.w - 64, 120);
        c.fill_round(card, 8, theme::light());
        c.outline_round(card, 8, theme::stroke());
        magnifier(c, card.x + 36, card.y + 44, 2, theme::accent());
        c.draw_text_in(
            &UI_BOLD,
            card.x + 84,
            card.y + 34,
            "Search apps, files and folders",
            theme::text(),
        );
        let lines = [
            "Type a name. Russian names of apps work too, like \"блокнот\".",
            "Tab switches between All, Apps, Documents and Folders.",
        ];
        for (k, l) in lines.into_iter().enumerate() {
            c.draw_text(
                card.x + 84,
                card.y + 60 + k as i32 * 22,
                l,
                theme::text_dim(),
            );
        }
    }

    fn draw_icon(c: &mut Canvas, icons: &Icons, hit: &Hit, size: i32, x: i32, y: i32) {
        match hit {
            Hit::App(a) => icons.draw(c, *a, size as usize, x, y),
            Hit::Path { dir: true, .. } => widgets::folder_icon(c, x, y, size),
            Hit::Path { .. } => widgets::file_icon(c, x, y, size),
        }
    }

    fn draw_results(&self, c: &mut Canvas, p: Rect, icons: &Icons) {
        for (i, r, heading) in self.rows(p) {
            if let Some(h) = heading {
                c.draw_text_in(&UI_BOLD, r.x + 12, r.y - 26, h, theme::text());
            }
            let hit = &self.results[i];
            if i == self.selected {
                c.fill_round(r, 6, theme::accent_light());
                c.fill_round(Rect::new(r.x, r.y + 10, 3, r.h - 20), 1, theme::accent());
            } else {
                self.highlight(c, Target::Result(i), r);
            }
            let name = fit(&hit.name(), r.w - 70);
            if i == 0 {
                Self::draw_icon(c, icons, hit, 48, r.x + 12, r.y + 8);
                c.draw_text_in(&UI_BOLD, r.x + 72, r.y + 12, &name, theme::text());
                c.draw_text(r.x + 72, r.y + 34, hit.type_name(), theme::text_dim());
            } else {
                Self::draw_icon(c, icons, hit, 24, r.x + 14, r.y + 8);
                c.draw_text(r.x + 50, r.y + 10, &name, theme::text());
            }
        }
    }

    fn draw_detail(&self, c: &mut Canvas, p: Rect, icons: &Icons, pins: &[App]) {
        let d = Self::detail_area(p);
        c.fill_round(d, 8, theme::light());
        c.outline_round(d, 8, theme::stroke());
        let Some(hit) = self.results.get(self.selected) else {
            return;
        };
        let big = MEDIUM as i32 * 2;
        Self::draw_icon(c, icons, hit, big, d.x + (d.w - big) / 2, d.y + 28);
        let name = fit(&hit.name(), d.w - 24);
        let row = |i: i32| Rect::new(d.x, d.y + 96 + i * 24, d.w, 22);
        c.text_centered_in(&UI_BOLD, row(0), &name, theme::text());
        c.text_centered(row(1), hit.type_name(), theme::text_dim());
        if let Some(loc) = hit.location() {
            c.text_centered(row(2), &fit(&loc, d.w - 24), theme::text_dim());
        }
        c.fill_rect(d.x + 12, d.y + 192, d.w - 24, 1, theme::stroke());
        for (t, r) in self.actions(p) {
            self.highlight(c, t, r);
            let label = match (t, hit) {
                (Target::Open, _) => "Open",
                (Target::Location, _) => "Open file location",
                (Target::Pin, Hit::App(a)) if pins.contains(a) => "Remove from Dock",
                _ => "Keep in Dock",
            };
            let (ix, iy) = (r.x + 14, r.y + 11);
            match t {
                Target::Open => {
                    // a box with an arrow out of it
                    c.outline_round(Rect::new(ix, iy + 2, 14, 14), 2, theme::text());
                    c.line(ix + 7, iy + 9, ix + 15, iy + 1, theme::accent());
                    c.line(ix + 10, iy + 1, ix + 15, iy + 1, theme::accent());
                    c.line(ix + 15, iy + 1, ix + 15, iy + 6, theme::accent());
                }
                Target::Location => widgets::folder_icon(c, ix, iy, 16),
                _ => {
                    // a pin
                    c.fill_round(Rect::new(ix + 4, iy, 8, 9), 2, theme::text());
                    c.fill_rect(ix + 2, iy + 8, 12, 2, theme::text());
                    c.fill_rect(ix + 7, iy + 10, 2, 6, theme::text());
                }
            }
            c.draw_text(r.x + 44, r.y + 9, label, theme::text());
        }
    }
}

/// How well `name` (lowercase) matches `q` (lowercase): 3 if it starts
/// with it, 2 if a word in it does, 1 if it is anywhere in it.
fn score(name: &str, q: &str) -> Option<u32> {
    if name.starts_with(q) {
        return Some(3);
    }
    let at = name.find(q)?;
    let before = name[..at].chars().next_back();
    Some(match before {
        Some(' ' | '_' | '-' | '.' | '(') => 2,
        _ => 1,
    })
}

/// A magnifying glass with its lens centred at (x, y), `k` times the
/// size of the one in the search box.
pub fn magnifier(c: &mut Canvas, x: i32, y: i32, k: i32, ink: u32) {
    let r = 6 * k;
    let lens = Rect::new(x - r, y - r, 2 * r, 2 * r);
    for i in 0..k.max(1) {
        c.outline_round(lens.inset(i), r - i, ink);
    }
    for i in 0..(k + 1) {
        let (x0, y0) = (x + r * 7 / 10 + i, y + r * 7 / 10);
        c.line(x0, y0, x0 + 4 * k, y0 + 4 * k, ink);
    }
}

/// Cut text to fit `w` pixels, ending with "…".
pub fn fit(text: &str, w: i32) -> String {
    if UI.width(text) <= w {
        return String::from(text);
    }
    let mut s = String::new();
    for ch in text.chars() {
        s.push(ch);
        if UI.width(&s) + UI.width("…") > w {
            s.pop();
            break;
        }
    }
    s.push('…');
    s
}
