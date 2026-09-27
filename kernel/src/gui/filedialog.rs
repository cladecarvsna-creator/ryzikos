//! The Open and Save as dialogs: a folder's contents, places on the
//! left, a file name box, and Open/Save and Cancel.

use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{rgb, Canvas, Rect};
use super::text::TITLE;
use super::theme;
use super::widgets::{self, FieldEvent, TextField};
use crate::fs::{self, Info};
use crate::keyboard::Key;
use crate::{interrupts, users};

const W: i32 = 700;
const H: i32 = 480;
const ROW: i32 = 26;
const DOUBLE_CLICK: u64 = interrupts::TIMER_HZ / 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Open,
    Save,
}

pub enum Event {
    None,
    Redraw,
    Cancel,
    /// The file to open or save to.
    Chosen(String),
}

pub struct FileDialog {
    pub mode: Mode,
    dir: String,
    items: Vec<Info>,
    selected: Option<usize>,
    /// First row shown.
    scroll: usize,
    name: TextField,
    /// The list was used last, so Enter acts on its selection.
    list_focus: bool,
    last_click: (u64, usize),
    error: Option<&'static str>,
}

struct Layout {
    panel: Rect,
    up: Rect,
    path: Rect,
    places: Rect,
    list: Rect,
    name: Rect,
    ok: Rect,
    cancel: Rect,
}

fn layout(area: Rect) -> Layout {
    let panel = Rect::new(area.x + (area.w - W) / 2, area.y + (area.h - H) / 2, W, H);
    let (x, y) = (panel.x, panel.y);
    Layout {
        panel,
        up: Rect::new(x + 16, y + 56, 34, 32),
        path: Rect::new(x + 58, y + 56, W - 74, 32),
        places: Rect::new(x + 16, y + 100, 140, H - 210),
        list: Rect::new(x + 164, y + 100, W - 180, H - 210),
        name: Rect::new(x + 110, y + H - 100, W - 126, 32),
        ok: Rect::new(x + W - 272, y + H - 52, 120, 32),
        cancel: Rect::new(x + W - 144, y + H - 52, 120, 32),
    }
}

/// The places on the left: a label and a folder.
fn places() -> Vec<(&'static str, String)> {
    let home = fs::home(users::current_name().unwrap_or_default().as_str());
    let mut out = Vec::new();
    out.push(("Home", home.clone()));
    for lib in ["Desktop", "Documents", "Downloads", "Pictures", "Videos"] {
        out.push((lib, fs::join(&home, lib)));
    }
    out.push(("System Disk", String::from("/")));
    if fs::disc_label().is_some() {
        out.push(("Disc", String::from(fs::DISC_PATH)));
    }
    out
}

impl FileDialog {
    /// Start in `dir` with `name` in the name box.
    pub fn new(mode: Mode, dir: &str, name: &str) -> Self {
        let mut d = Self {
            mode,
            dir: String::new(),
            items: Vec::new(),
            selected: None,
            scroll: 0,
            name: TextField::new(name),
            list_focus: false,
            last_click: (0, usize::MAX),
            error: None,
        };
        d.go(dir);
        // like Windows, the name is selected without the extension
        let base = name.rfind('.').unwrap_or(name.len());
        d.name.select(0, name[..base].chars().count());
        d
    }

    fn go(&mut self, dir: &str) {
        match fs::list(dir) {
            Ok(items) => {
                self.dir = String::from(dir);
                self.items = items;
                self.selected = None;
                self.scroll = 0;
                self.error = None;
            }
            Err(e) => self.error = Some(e.message()),
        }
    }

    fn rows(list: Rect) -> usize {
        (list.h / ROW) as usize
    }

    fn select(&mut self, i: usize, list: Rect) {
        self.selected = Some(i);
        self.list_focus = true;
        if !self.items[i].dir {
            let name = self.items[i].name.clone();
            self.name.set(&name);
        }
        let rows = Self::rows(list);
        if i < self.scroll {
            self.scroll = i;
        } else if i >= self.scroll + rows {
            self.scroll = i + 1 - rows;
        }
    }

    /// Act on the selected item: enter a folder or choose a file.
    fn activate(&mut self, i: usize) -> Event {
        let item = &self.items[i];
        let path = fs::join(&self.dir, &item.name);
        if item.dir {
            self.go(&path);
            Event::Redraw
        } else {
            Event::Chosen(path)
        }
    }

    /// Open or Save was pressed.
    fn accept(&mut self) -> Event {
        let mut name = self.name.string();
        let trimmed = name.trim().trim_end_matches('.');
        if trimmed.is_empty() {
            return Event::None;
        }
        name = String::from(trimmed);
        let path = if name.contains(['\\', '/']) || name.starts_with("C:") {
            fs::parse(&name)
        } else {
            fs::join(&self.dir, &name)
        };
        if fs::is_dir(&path) {
            self.go(&path);
            self.name.set("");
            return Event::Redraw;
        }
        match self.mode {
            Mode::Open if !fs::exists(&path) => {
                self.error = Some(fs::Error::NotFound.message());
                Event::Redraw
            }
            Mode::Open => Event::Chosen(path),
            Mode::Save => {
                let file = String::from(fs::file_name(&path));
                if !fs::valid_name(&file) {
                    self.error = Some(fs::Error::BadName.message());
                    return Event::Redraw;
                }
                // "Text documents": add .txt when there is no extension
                let mut path = path;
                if !file.contains('.') {
                    path.push_str(".txt");
                }
                Event::Chosen(path)
            }
        }
    }

    pub fn on_key(&mut self, key: Key, area: Rect) -> Event {
        let list = layout(area).list;
        match key {
            Key::Escape => return Event::Cancel,
            Key::Enter => {
                if let Some(i) = self.selected.filter(|_| self.list_focus) {
                    return self.activate(i);
                }
                return self.accept();
            }
            Key::Up | Key::Down if !self.items.is_empty() => {
                let i = match (self.selected, key) {
                    (None, _) => 0,
                    (Some(i), Key::Up) => i.saturating_sub(1),
                    (Some(i), _) => (i + 1).min(self.items.len() - 1),
                };
                self.select(i, list);
                return Event::Redraw;
            }
            Key::Backspace if self.name.text.is_empty() => {
                let up = fs::parent(&self.dir);
                self.go(&up);
                return Event::Redraw;
            }
            _ => {}
        }
        match self.name.on_key(key) {
            FieldEvent::None => Event::None,
            _ => {
                self.list_focus = false;
                self.error = None;
                Event::Redraw
            }
        }
    }

    pub fn on_wheel(&mut self, clicks: i32, area: Rect) -> bool {
        let rows = Self::rows(layout(area).list);
        let max = self.items.len().saturating_sub(rows);
        let old = self.scroll;
        self.scroll = (self.scroll as i32 + clicks * 3).clamp(0, max as i32) as usize;
        old != self.scroll
    }

    pub fn on_click(&mut self, area: Rect, x: i32, y: i32) -> Event {
        let l = layout(area);
        if !l.panel.contains(x, y) {
            return Event::None;
        }
        if l.cancel.contains(x, y) {
            return Event::Cancel;
        }
        if l.ok.contains(x, y) {
            return self.accept();
        }
        if l.up.contains(x, y) {
            let up = fs::parent(&self.dir);
            self.go(&up);
            return Event::Redraw;
        }
        if l.name.contains(x, y) {
            self.name.click(l.name, x);
            self.list_focus = false;
            return Event::Redraw;
        }
        if l.places.contains(x, y) {
            let i = ((y - l.places.y) / ROW) as usize;
            if let Some((_, path)) = places().get(i) {
                self.go(path);
            }
            return Event::Redraw;
        }
        if l.list.contains(x, y) {
            let i = self.scroll + ((y - l.list.y) / ROW) as usize;
            if i >= self.items.len() {
                self.selected = None;
                return Event::Redraw;
            }
            let now = interrupts::ticks();
            let double = self.last_click.1 == i && now - self.last_click.0 <= DOUBLE_CLICK;
            self.last_click = (now, i);
            self.select(i, l.list);
            if double {
                self.last_click = (0, usize::MAX);
                return self.activate(i);
            }
            return Event::Redraw;
        }
        Event::None
    }

    pub fn draw(&mut self, c: &mut Canvas, area: Rect, caret: bool) {
        c.fill_round_alpha(area, 0, rgb(0x20, 0x20, 0x28), 60);
        let l = layout(area);
        let p = l.panel;
        c.shadow(p, 8, 16, 4, 120);
        c.fill_round(p, 8, theme::face());
        c.outline_round(p, 8, theme::frame());
        let title = match self.mode {
            Mode::Open => "Open",
            Mode::Save => "Save as",
        };
        c.draw_text_in(&TITLE, p.x + 20, p.y + 16, title, theme::text());

        theme::button(c, l.up, "", false);
        // an arrow pointing up
        let (ax, ay) = (l.up.x + l.up.w / 2, l.up.y + 9);
        c.line(ax, ay, ax, ay + 14, theme::text());
        c.line(ax - 5, ay + 5, ax, ay, theme::text());
        c.line(ax + 5, ay + 5, ax, ay, theme::text());
        c.fill_round(l.path, 4, theme::light());
        c.outline_round(l.path, 4, theme::stroke());
        widgets::folder_icon(c, l.path.x + 8, l.path.y + 8, 16);
        let shown = fs::display(&self.dir);
        c.draw_text(l.path.x + 32, l.path.y + 7, &shown, theme::text());

        // places
        for (i, (label, path)) in places().iter().enumerate() {
            let r = Rect::new(l.places.x, l.places.y + i as i32 * ROW, l.places.w, ROW - 2);
            if fs::same_name(path, &self.dir) {
                c.fill_round(r, 4, theme::accent_light());
            }
            widgets::folder_icon(c, r.x + 6, r.y + 5, 16);
            c.draw_text(r.x + 30, r.y + 4, label, theme::text());
        }

        // the folder's contents
        c.fill_round(l.list, 4, theme::light());
        c.outline_round(l.list, 4, theme::stroke());
        {
            let mut lc = c.sub(Rect::new(0, 0, c.width, c.height));
            lc.clip_to(l.list.inset(1));
            let rows = Self::rows(l.list);
            for (k, item) in self.items.iter().enumerate().skip(self.scroll).take(rows) {
                let r = Rect::new(
                    l.list.x + 2,
                    l.list.y + (k - self.scroll) as i32 * ROW + 1,
                    l.list.w - 4,
                    ROW,
                );
                if self.selected == Some(k) {
                    lc.fill_round(r, 3, theme::selection());
                }
                if item.dir {
                    widgets::folder_icon(&mut lc, r.x + 6, r.y + 5, 16);
                } else {
                    widgets::file_icon(&mut lc, r.x + 6, r.y + 5, 16);
                }
                lc.draw_text(r.x + 30, r.y + 4, &item.name, theme::text());
            }
            if self.items.is_empty() {
                let r = Rect::new(l.list.x, l.list.y + 20, l.list.w, 20);
                lc.text_centered(r, "This folder is empty.", theme::text_dim());
            }
        }

        c.draw_text(p.x + 20, l.name.y + 7, "File name:", theme::text());
        self.name.draw(c, l.name, true, caret);
        if let Some(e) = self.error {
            c.draw_text(p.x + 20, l.ok.y + 7, e, theme::error());
        }
        let ok = match self.mode {
            Mode::Open => "Open",
            Mode::Save => "Save",
        };
        theme::accent_button(c, l.ok, ok, false);
        theme::button(c, l.cancel, "Cancel", false);
    }
}
