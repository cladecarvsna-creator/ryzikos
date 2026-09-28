//! App Store: programs for RyzikOS from the `programs` folder of the
//! RyzikOS repository. The catalog and the programs are downloaded over
//! the web; without the internet it falls back to the catalog RyzikOS
//! was built with and installs from the disc.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::text::{HEADING, UI, UI_BOLD};
use super::{theme, widgets, MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::fs;
use crate::keyboard::Key;
use crate::web::{self, url::Url, CatalogEntry, PROGRAM_EXT};

pub const CLIENT_W: i32 = 1000;
pub const CLIENT_H: i32 = 680;

/// The window's size now; it opens at CLIENT_W x CLIENT_H.
fn cw() -> i32 {
    super::client_w(super::App::Store)
}
fn ch() -> i32 {
    super::client_h(super::App::Store)
}

const HEAD_H: i32 = 104;
const TABS_Y: i32 = HEAD_H + 14;
const LIST_Y: i32 = HEAD_H + 62;
const STATUS_H: i32 = 30;
const CARD_W: i32 = 470;
const CARD_H: i32 = 132;
const GAP: i32 = 16;

/// Cards side by side: as many as fit, up to three.
fn columns() -> usize {
    ((cw() - 40 + GAP) / (CARD_W + GAP)).clamp(1, 3) as usize
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Games,
    Tools,
    Installed,
}

const TABS: [(Tab, &str); 4] = [
    (Tab::All, "All"),
    (Tab::Games, "Games"),
    (Tab::Tools, "Tools"),
    (Tab::Installed, "Installed"),
];

/// Where the catalog came from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Loading,
    Web,
    /// No internet: the catalog built into RyzikOS.
    BuiltIn,
}

/// A button on a card.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Install,
    Open,
    Remove,
}

/// What a download fiber hands back: the file's bytes or why not.
type Result_ = Rc<RefCell<Option<Result<Vec<u8>, String>>>>;

struct Job {
    fiber: Fiber,
    out: Result_,
    /// The program being installed, or None for the catalog.
    file: Option<String>,
}

pub struct Store {
    entries: Vec<CatalogEntry>,
    source: Source,
    tab: Tab,
    installed: Vec<String>,
    status: String,
    jobs: Vec<Job>,
    /// Stopped jobs, run until they notice.
    draining: Vec<Fiber>,
    scroll: i32,
    /// Dragging the scroll bar's thumb, grabbed this far from its top.
    thumb_grab: Option<i32>,
    hover: Option<(usize, Act)>,
    started: bool,
    /// A program to run, for the desktop to pick up.
    pub open_request: Option<String>,
}

fn download(url: String) -> Job {
    let out: Result_ = Rc::new(RefCell::new(None));
    let o = out.clone();
    let fiber = Fiber::new(move || {
        let result = match Url::parse(&url) {
            Some(u) => match web::http::get(&u, None) {
                Ok(r) if (200..300).contains(&r.status) => Ok(r.body),
                Ok(r) => Err(format!("the server answered {}", r.status)),
                Err(e) => Err(e),
            },
            None => Err(String::from("bad address")),
        };
        *o.borrow_mut() = Some(result);
    });
    Job {
        fiber,
        out,
        file: None,
    }
}

/// A color for a program's tile, from its name.
fn tile_color(name: &str) -> u32 {
    const COLORS: [u32; 8] = [
        rgb(0x4c, 0x7d, 0xff),
        rgb(0x2e, 0xa0, 0x56),
        rgb(0xe0, 0x6c, 0x2b),
        rgb(0x9b, 0x4d, 0xe0),
        rgb(0xd8, 0x3a, 0x6a),
        rgb(0x0f, 0x8b, 0xb3),
        rgb(0xc9, 0x9a, 0x00),
        rgb(0x5b, 0x6b, 0x88),
    ];
    let h = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    COLORS[h as usize % COLORS.len()]
}

/// Cut `text` to fit `w` pixels, with an ellipsis.
fn fit(text: &str, w: i32) -> String {
    if UI.width(text) <= w {
        return String::from(text);
    }
    let mut s = String::new();
    for c in text.chars() {
        s.push(c);
        if UI.width(&s) + UI.width("…") > w {
            s.pop();
            break;
        }
    }
    s.push('…');
    s
}

/// Break `text` into lines no wider than `w`, at most `max` of them.
fn wrap(text: &str, w: i32, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let tried = if line.is_empty() {
            String::from(word)
        } else {
            format!("{} {}", line, word)
        };
        if UI.width(&tried) > w && !line.is_empty() {
            lines.push(core::mem::replace(&mut line, String::from(word)));
        } else {
            line = tried;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max {
        lines.truncate(max);
        let last = lines.pop().unwrap_or_default();
        lines.push(fit(&format!("{} …", last), w));
    }
    lines
}

fn client() -> Rect {
    Rect::new(0, 0, cw(), ch())
}

fn tab_rect(i: usize) -> Rect {
    let mut x = 24;
    for (k, (_, label)) in TABS.iter().enumerate() {
        let w = UI.width(label) + 32;
        if k == i {
            return Rect::new(x, TABS_Y, w, 34);
        }
        x += w + 8;
    }
    Rect::default()
}

fn refresh_rect() -> Rect {
    Rect::new(cw() - 24 - 170, TABS_Y, 170, 34)
}

fn list_rect() -> Rect {
    Rect::new(0, LIST_Y, cw(), ch() - LIST_Y - STATUS_H)
}

/// The scroll bar, in the margin right of the cards.
fn track() -> Rect {
    let list = list_rect();
    Rect::new(cw() - 18, list.y + 6, 12, list.h - 12)
}

impl Store {
    /// The window got a new size.
    pub fn resized(&mut self) {
        self.scroll = self.scroll.clamp(0, self.max_scroll());
    }

    pub fn new() -> Self {
        Self {
            entries: web::parse_catalog(web::CATALOG_TEXT),
            source: Source::BuiltIn,
            tab: Tab::All,
            installed: Vec::new(),
            status: String::new(),
            jobs: Vec::new(),
            draining: Vec::new(),
            scroll: 0,
            thumb_grab: None,
            hover: None,
            started: false,
            open_request: None,
        }
    }

    /// The window opened: fetch the newest catalog the first time.
    pub fn start(&mut self) {
        self.read_installed();
        if !self.started {
            self.started = true;
            self.fetch_catalog();
        }
    }

    fn fetch_catalog(&mut self) {
        if self.jobs.iter().any(|j| j.file.is_none()) {
            return;
        }
        self.source = Source::Loading;
        self.status = String::from("Getting the newest catalog from the internet...");
        self.jobs.push(download(format!("{}catalog.txt", web::CATALOG_BASE)));
    }

    fn read_installed(&mut self) {
        self.installed = web::installed_programs()
            .iter()
            .map(|p| String::from(fs::file_name(p)))
            .collect();
    }

    fn is_installed(&self, file: &str) -> bool {
        self.installed.iter().any(|n| n.eq_ignore_ascii_case(file))
    }

    fn installing(&self, file: &str) -> bool {
        self.jobs.iter().any(|j| j.file.as_deref() == Some(file))
    }

    /// Whether downloads are running, so the desktop keeps ticking.
    pub fn busy(&self) -> bool {
        !self.jobs.is_empty() || !self.draining.is_empty()
    }

    /// Run the downloads a little. Returns true if something changed.
    pub fn tick(&mut self) -> bool {
        for f in self.draining.iter_mut() {
            f.resume();
        }
        self.draining.retain(|f| !f.done());
        let mut changed = false;
        let mut i = 0;
        while i < self.jobs.len() {
            if !self.jobs[i].fiber.resume() {
                i += 1;
                continue;
            }
            let job = self.jobs.remove(i);
            let result = job.out.borrow_mut().take().unwrap_or(Err(String::from("stopped")));
            match job.file {
                None => self.catalog_arrived(result),
                Some(file) => self.program_arrived(&file, result),
            }
            changed = true;
        }
        changed
    }

    fn catalog_arrived(&mut self, result: Result<Vec<u8>, String>) {
        let entries = result
            .ok()
            .map(|b| web::parse_catalog(&String::from_utf8_lossy(&b)))
            .filter(|e| !e.is_empty());
        match entries {
            Some(e) => {
                self.entries = e;
                self.source = Source::Web;
                self.status = format!("{} programs in the catalog", self.entries.len());
            }
            None => {
                self.entries = web::parse_catalog(web::CATALOG_TEXT);
                self.source = Source::BuiltIn;
                self.status = String::from(
                    "No internet: showing the catalog built into RyzikOS. Programs install from the disc if they can't be downloaded.",
                );
            }
        }
        crate::serial::write_str(match self.source {
            Source::Web => "\nstore: catalog from the internet\n",
            _ => "\nstore: built-in catalog\n",
        });
    }

    fn program_arrived(&mut self, file: &str, result: Result<Vec<u8>, String>) {
        let name = self.name_of(file);
        let dir = web::programs_folder();
        let _ = fs::create_dir(&dir);
        let to = fs::join(&dir, file);
        let data = match result {
            Ok(d) if !d.is_empty() => Ok(d),
            Ok(_) => Err(String::from("the file was empty")),
            Err(e) => {
                // no internet: the RyzikOS disc carries the programs too
                let on_disc = fs::drives()
                    .into_iter()
                    .filter(|d| d.ready && d.kind == fs::DriveKind::Cd)
                    .find_map(|d| fs::read(&fs::join(&fs::join(&d.path, "Programs"), file)).ok());
                on_disc.ok_or(e)
            }
        };
        self.status = match data.map(|d| fs::write(&to, &d).map_err(|e| String::from(e.message()))) {
            Ok(Ok(())) => {
                web::add_shortcut(&name, &to);
                crate::serial::write_str("\nstore: installed ");
                crate::serial::write_str(file);
                crate::serial::write_str("\n");
                format!("{} is installed. Its shortcut is on the desktop and in the launcher.", name)
            }
            Ok(Err(e)) | Err(e) => format!("Could not install {}: {}", name, e),
        };
        self.read_installed();
    }

    fn name_of(&self, file: &str) -> String {
        self.entries
            .iter()
            .find(|e| e.file.eq_ignore_ascii_case(file))
            .map(|e| e.name.clone())
            .unwrap_or_else(|| String::from(file.trim_end_matches(PROGRAM_EXT)))
    }

    fn install(&mut self, file: &str) {
        if self.installing(file) {
            return;
        }
        let mut job = download(format!("{}{}", web::CATALOG_BASE, file));
        job.file = Some(String::from(file));
        self.jobs.push(job);
        self.status = format!("Downloading {}...", self.name_of(file));
    }

    fn remove(&mut self, file: &str) {
        let path = fs::join(&web::programs_folder(), file);
        web::remove_shortcuts(&path);
        self.status = match fs::remove(&path) {
            Ok(()) => format!("{} was removed.", self.name_of(file)),
            Err(e) => String::from(e.message()),
        };
        self.read_installed();
    }

    /// The cards shown on the current tab: installed programs that are
    /// not in the catalog show up under Installed too.
    fn shown(&self) -> Vec<CatalogEntry> {
        let mut out: Vec<CatalogEntry> = self
            .entries
            .iter()
            .filter(|e| match self.tab {
                Tab::All => true,
                Tab::Games => e.category.eq_ignore_ascii_case("games"),
                Tab::Tools => !e.category.eq_ignore_ascii_case("games"),
                Tab::Installed => self.is_installed(&e.file),
            })
            .cloned()
            .collect();
        if self.tab == Tab::Installed {
            for n in &self.installed {
                if !out.iter().any(|e| e.file.eq_ignore_ascii_case(n)) {
                    out.push(CatalogEntry {
                        file: n.clone(),
                        name: String::from(n.trim_end_matches(PROGRAM_EXT)),
                        category: String::from("Installed"),
                        about: String::from("Installed from a file, not from the catalog."),
                    });
                }
            }
        }
        out
    }

    fn card_rect(&self, i: usize) -> Rect {
        let list = list_rect();
        let n = columns();
        let (col, row) = ((i % n) as i32, (i / n) as i32);
        let left = (cw() - n as i32 * (CARD_W + GAP) + GAP) / 2;
        Rect::new(
            left + col * (CARD_W + GAP),
            list.y + 8 + row * (CARD_H + GAP) - self.scroll,
            CARD_W,
            CARD_H,
        )
    }

    /// The buttons on card `i`, right to left from the bottom corner.
    fn buttons(&self, i: usize, e: &CatalogEntry) -> Vec<(Act, Rect)> {
        let r = self.card_rect(i);
        let y = r.bottom() - 44;
        if self.is_installed(&e.file) {
            vec![
                (Act::Open, Rect::new(r.right() - 110, y, 94, 32)),
                (Act::Remove, Rect::new(r.right() - 214, y, 94, 32)),
            ]
        } else {
            vec![(Act::Install, Rect::new(r.right() - 110, y, 94, 32))]
        }
    }

    /// The height of all the cards.
    fn total(&self) -> i32 {
        let rows = self.shown().len().div_ceil(columns()) as i32;
        rows * (CARD_H + GAP) + 16
    }

    fn max_scroll(&self) -> i32 {
        (self.total() - list_rect().h).max(0)
    }

    fn scroll_to(&mut self, to: i32) -> bool {
        let old = self.scroll;
        self.scroll = to.clamp(0, self.max_scroll());
        old != self.scroll
    }

    fn hit(&self, x: i32, y: i32) -> Option<(usize, Act)> {
        if !list_rect().contains(x, y) {
            return None;
        }
        for (i, e) in self.shown().iter().enumerate() {
            for (act, r) in self.buttons(i, e) {
                if r.contains(x, y) {
                    return Some((i, act));
                }
            }
        }
        None
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        let view = list_rect().h;
        match ev.kind {
            MouseKind::Move => {
                let Some(grab) = self.thumb_grab else {
                    return false;
                };
                let to = widgets::thumb_drag(track(), true, self.total(), view, ev.y, grab);
                return self.scroll_to(to);
            }
            MouseKind::Up => {
                self.thumb_grab = None;
                return false;
            }
            MouseKind::Down { right: true } => return false,
            MouseKind::Down { right: false } => {}
        }
        if self.max_scroll() > 0 && track().inset(-4).contains(ev.x, ev.y) {
            let t = widgets::thumb(track(), true, self.total(), view, self.scroll);
            if t.contains(ev.x, ev.y) {
                self.thumb_grab = Some(ev.y - t.y);
                return false;
            }
            // a click above or below the thumb turns a page
            let page = view - CARD_H / 2;
            let to = if ev.y < t.y { self.scroll - page } else { self.scroll + page };
            return self.scroll_to(to);
        }
        for (i, (tab, _)) in TABS.iter().enumerate() {
            if tab_rect(i).contains(ev.x, ev.y) {
                self.tab = *tab;
                self.scroll = 0;
                return true;
            }
        }
        if refresh_rect().contains(ev.x, ev.y) {
            self.read_installed();
            self.fetch_catalog();
            return true;
        }
        if let Some((i, act)) = self.hit(ev.x, ev.y) {
            let Some(e) = self.shown().get(i).cloned() else {
                return false;
            };
            match act {
                Act::Install => self.install(&e.file),
                Act::Open => self.open_request = Some(fs::join(&web::programs_folder(), &e.file)),
                Act::Remove => self.remove(&e.file),
            }
            return true;
        }
        false
    }

    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let h = self.hit(x, y);
        core::mem::replace(&mut self.hover, h) != h
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        self.scroll_to(self.scroll + clicks * 60)
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        match key {
            Key::Down => self.on_wheel(1),
            Key::PageDown => self.on_wheel(5),
            Key::Up => self.on_wheel(-1),
            Key::PageUp => self.on_wheel(-5),
            Key::Home => self.scroll_to(0),
            Key::End => self.scroll_to(self.max_scroll()),
            _ => false,
        }
    }

    pub fn draw(&mut self, c: &mut Canvas) {
        c.fill(client(), theme::light());
        // the header
        let head = Rect::new(0, 0, cw(), HEAD_H);
        c.vertical_gradient(head, rgb(0x6c, 0x3f, 0xd1), rgb(0x1a, 0x73, 0xe8));
        bag_icon(c, 28, 24);
        c.draw_text_in(&HEADING, 104, 22, "App Store", 0xffffff);
        c.draw_text(106, 62, "Games and tools for RyzikOS, downloaded from the internet", rgb(0xe4, 0xe8, 0xff));
        let (label, color) = match self.source {
            Source::Loading => ("Updating the catalog...", rgb(0xff, 0xf0, 0xb0)),
            Source::Web => ("Catalog from the internet", rgb(0xc8, 0xff, 0xd8)),
            Source::BuiltIn => ("Offline catalog", rgb(0xff, 0xe0, 0xc0)),
        };
        let w = UI.width(label);
        c.draw_text(cw() - 28 - w, 40, label, color);
        // tabs and refresh
        for (i, (tab, label)) in TABS.iter().enumerate() {
            let r = tab_rect(i);
            if *tab == self.tab {
                c.fill_round(r, 17, theme::accent());
                c.text_centered(r, label, 0xffffff);
            } else {
                c.fill_round(r, 17, mix(theme::stroke(), theme::light(), 120));
                c.text_centered(r, label, theme::text());
            }
        }
        theme::button(c, refresh_rect(), "Check for new", false);
        // the cards
        let list = list_rect();
        let shown = self.shown();
        {
            let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
            s.clip_to(list);
            if shown.is_empty() {
                let msg = if self.tab == Tab::Installed {
                    "Nothing installed yet. Pick a program on the All tab and press Install."
                } else {
                    "The catalog is empty."
                };
                s.text_centered(Rect::new(0, list.y + 40, cw(), 24), msg, theme::text_dim());
            }
            for (i, e) in shown.iter().enumerate() {
                let r = self.card_rect(i);
                if r.bottom() < list.y || r.y > list.bottom() {
                    continue;
                }
                self.draw_card(&mut s, i, e, r);
            }
        }
        if self.max_scroll() > 0 {
            let (t, total, view) = (track(), self.total(), list.h);
            c.fill_round(t, 6, mix(theme::stroke(), theme::light(), 140));
            let thumb = widgets::thumb(t, true, total, view, self.scroll).inset(2);
            let color = if self.thumb_grab.is_some() { theme::accent() } else { theme::thumb() };
            c.fill_round(thumb, 4, color);
        }
        // the status line
        let st = Rect::new(0, ch() - STATUS_H, cw(), STATUS_H);
        c.fill(st, theme::face());
        c.fill_rect(0, st.y, cw(), 1, theme::stroke());
        let n = self.installed.len();
        let left = if self.status.is_empty() {
            format!("{} program{} installed", n, if n == 1 { "" } else { "s" })
        } else {
            self.status.clone()
        };
        c.draw_text(14, st.y + 6, &fit(&left, cw() - 28), theme::text());
    }

    fn draw_card(&self, c: &mut Canvas, i: usize, e: &CatalogEntry, r: Rect) {
        c.fill_round(r, 12, theme::raised());
        c.outline_round(r, 12, theme::stroke());
        // the program's tile: its first letter on a color of its own
        let tile = Rect::new(r.x + 18, r.y + 18, 64, 64);
        let color = tile_color(&e.name);
        {
            let mut t = c.sub(Rect::new(0, 0, c.width, c.height));
            t.clip_round(tile, 14);
            t.vertical_gradient(tile, mix(color, 0xffffff, 40), color);
        }
        let letter: String = e.name.chars().take(1).collect();
        c.text_centered_in(&HEADING, tile, &letter, 0xffffff);
        let tx = r.x + 100;
        let tw = r.w - 118;
        c.draw_text_in(&UI_BOLD, tx, r.y + 16, &fit(&e.name, tw - 90), theme::text());
        let cat_w = UI.width(&e.category) + 18;
        let chip = Rect::new(r.right() - 18 - cat_w, r.y + 16, cat_w, 22);
        c.fill_round(chip, 11, mix(color, theme::light(), 215));
        c.text_centered(chip, &e.category, color);
        for (k, line) in wrap(&e.about, tw, 2).iter().enumerate() {
            c.draw_text(tx, r.y + 44 + k as i32 * 20, line, theme::text_dim());
        }
        if self.installing(&e.file) {
            let y = r.bottom() - 38;
            c.draw_text(r.right() - 18 - UI.width("Installing..."), y, "Installing...", theme::accent());
            return;
        }
        for (act, b) in self.buttons(i, e) {
            let hot = self.hover == Some((i, act));
            match act {
                Act::Install | Act::Open => {
                    let face = if hot { mix(theme::accent(), 0x000000, 30) } else { theme::accent() };
                    c.fill_round(b, 16, face);
                    c.text_centered(b, if act == Act::Install { "Install" } else { "Open" }, 0xffffff);
                }
                Act::Remove => {
                    let face = if hot { theme::hover() } else { theme::light() };
                    c.fill_round(b, 16, face);
                    c.outline_round(b, 16, theme::stroke());
                    c.text_centered(b, "Remove", theme::error());
                }
            }
        }
        if self.is_installed(&e.file) {
            c.draw_text(tx, r.bottom() - 38, "Installed", rgb(0x2e, 0xa0, 0x56));
        }
    }
}

/// The App Store's picture: a shopping bag, 56 pixels.
pub fn bag_icon(c: &mut Canvas, x: i32, y: i32) {
    let body = Rect::new(x + 6, y + 16, 44, 38);
    c.fill_round(body, 8, 0xffffff);
    c.outline_round(Rect::new(x + 16, y + 4, 24, 26), 12, 0xffffff);
    c.outline_round(Rect::new(x + 17, y + 5, 22, 24), 11, 0xffffff);
    c.fill_round(Rect::new(x + 20, y + 26, 16, 16), 8, rgb(0x6c, 0x3f, 0xd1));
    c.fill_round(Rect::new(x + 25, y + 31, 6, 6), 3, 0xffffff);
}
