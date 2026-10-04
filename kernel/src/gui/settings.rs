//! Settings, like the Windows 11 app: pages in a list on the left and
//! cards of settings on the right. Most rows show how the system is set
//! up; the keyboard layout can be switched here, the About page opens
//! "About RyzikOS", and Personalization changes the look: light or dark
//! mode, the accent colour and the desktop background. Update finds a
//! newer RyzikOS on GitHub and gets it ready for the next start.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::filedialog::{self, FileDialog, Mode};
use super::icons::{self, Pic, SMALL};
use super::personalize::{self, Background, Prefs, COLORS, FITS};
use super::text::{TITLE, UI, UI_BOLD};
use super::tray::{self, Net};
use super::widgets::{FieldEvent, TextField};
use super::{picture, theme, wallpaper, App, MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::keyboard::{Key, Layout};
use crate::update::{self, State, Updater};
use crate::{clock, fs, interrupts};

pub const CLIENT_W: i32 = 940;
pub const CLIENT_H: i32 = 620;

/// The window's size now; it opens at CLIENT_W x CLIENT_H.
fn cw() -> i32 {
    super::client_w(super::App::Settings)
}
fn ch() -> i32 {
    super::client_h(super::App::Settings)
}

const NAV_W: i32 = 260;
const NAV_TOP: i32 = 112;
const NAV_ROW: i32 = 40;
const PAGE_X: i32 = NAV_W + 24;
const ROW_H: i32 = 56;

/// What the pages show, collected by the desktop.
pub struct Info<'a> {
    pub screen: (i32, i32),
    pub memory_mib: u32,
    pub bootloader: &'a str,
    pub net: Net,
    pub address: &'a str,
    pub layout: Layout,
    pub clock: &'a str,
    pub date: &'a str,
    pub uptime_minutes: u64,
    /// The text caret is showing (it blinks).
    pub caret: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    System,
    Personalization,
    Network,
    Time,
    Accounts,
    Update,
    About,
}

const PAGES: [(Page, &str); 7] = [
    (Page::System, "System"),
    (Page::Personalization, "Personalization"),
    (Page::Network, "Network & internet"),
    (Page::Time, "Time & language"),
    (Page::Accounts, "Accounts"),
    (Page::Update, "Update"),
    (Page::About, "About"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    SwitchLayout,
    OpenAbout,
    Update,
    /// Time & language: type a new date and time.
    ChangeTime,
    AutoTime,
    ZoneWest,
    ZoneEast,
    SaveTime,
    CancelTime,
}

/// Typing a new date and time on the Time & language page.
struct TimeEdit {
    field: TextField,
    error: bool,
}

pub struct Settings {
    page: Page,
    pressed: Option<Button>,
    /// Asks the desktop to switch the keyboard layout.
    pub switch_layout: bool,
    /// Choosing a picture for the background.
    dialog: RefCell<Option<FileDialog>>,
    /// Why the chosen file can't be the background.
    error: Option<&'static str>,
    /// Small pictures of the backgrounds, made when first shown.
    thumbs: RefCell<Thumbs>,
    pub updater: Updater,
    time_edit: RefCell<Option<TimeEdit>>,
    /// Reading the time from the internet, and when that was last tried.
    clock_sync: Option<Fiber>,
    clock_tried: Option<u64>,
}

#[derive(Default)]
struct Thumbs {
    builtin: Vec<Vec<u32>>,
    /// The picture from the disk, and its path.
    picture: Option<(String, Vec<u32>)>,
    /// The whole look, and the settings it shows.
    preview: Option<(Prefs, Vec<u32>)>,
}

/// Something on the Personalization page that can be clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// Light (false) or dark (true).
    Mode(bool),
    Accent(usize),
    Slot(usize),
    Browse,
    Fit(usize),
    Color(usize),
}

// the Personalization page
fn page_w() -> i32 {
    cw() - PAGE_X - 28
}
const PREVIEW: Rect = Rect::new(PAGE_X + 16, 84, 248, 140);
const THUMB_W: i32 = 120;
const THUMB_H: i32 = 68;
const SWATCH: i32 = 26;

fn mode_card() -> Rect {
    Rect::new(PAGE_X, 72, page_w(), 164)
}

fn accent_card() -> Rect {
    Rect::new(PAGE_X, 244, page_w(), 64)
}

fn background_card() -> Rect {
    Rect::new(PAGE_X, 316, page_w(), 176)
}

fn fit_card() -> Rect {
    Rect::new(PAGE_X, 500, page_w(), 52)
}

fn color_card() -> Rect {
    Rect::new(PAGE_X, 560, page_w(), 52)
}

fn mode_tile(dark: bool) -> Rect {
    let x = PAGE_X + 280 + if dark { 162 } else { 0 };
    Rect::new(x, 116, 150, 100)
}

/// Round colour swatch `i` of `n`, at the right end of card `r`.
fn swatch(r: Rect, i: usize, n: usize) -> Rect {
    let total = n as i32 * (SWATCH + 8) - 8;
    let x = r.right() - 20 - total + i as i32 * (SWATCH + 8);
    Rect::new(x, r.y + (r.h - SWATCH) / 2, SWATCH, SWATCH)
}

fn fit_button(i: usize) -> Rect {
    let r = fit_card();
    let total = FITS.len() as i32 * 78 - 6;
    Rect::new(r.right() - 16 - total + i as i32 * 78, r.y + 11, 72, 30)
}

fn browse_button() -> Rect {
    let r = background_card();
    Rect::new(r.right() - 16 - 130, r.y + 14, 130, 32)
}

fn slot_rect(i: usize) -> Rect {
    let r = background_card();
    Rect::new(
        r.x + 20 + i as i32 * (THUMB_W + 14),
        r.y + 64,
        THUMB_W,
        THUMB_H,
    )
}

/// The backgrounds to pick from: the built-in pictures, the picture
/// from the disk if one is used, and a plain colour.
fn slots(prefs: &Prefs) -> Vec<Background> {
    let mut out: Vec<Background> = (0..wallpaper::BUILTIN.len())
        .map(Background::Builtin)
        .collect();
    if let Background::Picture(_) = prefs.background {
        out.push(prefs.background.clone());
    }
    out.push(Background::Solid);
    out
}

fn targets(prefs: &Prefs) -> Vec<(Target, Rect)> {
    let mut out = Vec::new();
    out.push((Target::Mode(false), mode_tile(false)));
    out.push((Target::Mode(true), mode_tile(true)));
    for i in 0..theme::ACCENTS.len() {
        out.push((
            Target::Accent(i),
            swatch(accent_card(), i, theme::ACCENTS.len()),
        ));
    }
    for i in 0..slots(prefs).len() {
        out.push((Target::Slot(i), slot_rect(i)));
    }
    out.push((Target::Browse, browse_button()));
    for i in 0..FITS.len() {
        out.push((Target::Fit(i), fit_button(i)));
    }
    for i in 0..COLORS.len() {
        out.push((Target::Color(i), swatch(color_card(), i, COLORS.len())));
    }
    out
}

fn dialog_area() -> Rect {
    Rect::new(0, 0, cw(), ch())
}

fn nav_rect(i: usize) -> Rect {
    Rect::new(12, NAV_TOP + i as i32 * NAV_ROW, NAV_W - 24, NAV_ROW - 4)
}

/// Row `i` of the card on the current page.
fn row_rect(i: usize) -> Rect {
    Rect::new(
        PAGE_X,
        72 + i as i32 * (ROW_H + 4),
        cw() - PAGE_X - 28,
        ROW_H,
    )
}

/// The button at the right end of row `i`.
fn row_button(i: usize) -> Rect {
    let r = row_rect(i);
    Rect::new(r.right() - 16 - 170, r.y + (ROW_H - 32) / 2, 170, 32)
}

/// The time zone row's two buttons, west then east.
fn zone_buttons() -> (Rect, Rect) {
    let b = row_button(2);
    (
        Rect::new(b.x, b.y, 81, b.h),
        Rect::new(b.right() - 81, b.y, 81, b.h),
    )
}

/// The card for typing a new date and time, under the rows.
fn time_card() -> Rect {
    let r = row_rect(4);
    Rect::new(r.x, r.y, r.w, 96)
}

fn time_field() -> Rect {
    let r = time_card();
    Rect::new(r.x + 20, r.y + 40, 220, 32)
}

fn time_save() -> Rect {
    let f = time_field();
    Rect::new(f.right() + 12, f.y, 100, 32)
}

fn time_cancel() -> Rect {
    let s = time_save();
    Rect::new(s.right() + 8, s.y, 100, 32)
}

/// Try the internet time again after this long, if it failed.
const CLOCK_RETRY_TICKS: u64 = 120 * interrupts::TIMER_HZ;

impl Settings {
    pub fn new() -> Self {
        Self {
            page: Page::System,
            pressed: None,
            switch_layout: false,
            dialog: RefCell::new(None),
            error: None,
            thumbs: RefCell::new(Thumbs::default()),
            updater: Updater::new(),
            time_edit: RefCell::new(None),
            clock_sync: None,
            clock_tried: None,
        }
    }

    /// Run the updater and the clock check a little. Returns true if a
    /// page changed.
    pub fn tick(&mut self) -> bool {
        let mut changed = self.updater.tick();
        if let Some(f) = &mut self.clock_sync {
            if f.resume() {
                self.clock_sync = None;
                changed = true;
            }
        }
        changed
    }

    pub fn busy(&self) -> bool {
        self.updater.busy() || self.clock_sync.is_some()
    }

    /// With the network up, read the time from the internet if it is
    /// set automatically and was not read yet.
    pub fn check_clock(&mut self) {
        if !clock::auto() || clock::synced() || self.clock_sync.is_some() {
            return;
        }
        let now = interrupts::ticks();
        if self
            .clock_tried
            .is_some_and(|t| now - t < CLOCK_RETRY_TICKS)
        {
            return;
        }
        self.clock_tried = Some(now);
        self.clock_sync = Some(Fiber::new(|| match clock::fetch_utc() {
            Some(utc) => clock::set_utc(utc),
            None => crate::serial::write_str("clock: could not read the time from the internet\n"),
        }));
    }

    /// Typing a new time: the caret blinks.
    pub fn typing(&self) -> bool {
        self.page == Page::Time && self.time_edit.borrow().is_some()
    }

    fn save_time(&mut self) {
        let edit = self.time_edit.get_mut();
        let Some(e) = edit else {
            return;
        };
        match clock::parse(&e.field.string()) {
            Some(t) => {
                clock::set_local(t);
                *edit = None;
            }
            None => e.error = true,
        }
    }

    /// The Update page's button, for the updater's state.
    fn update_label(&self) -> Option<&'static str> {
        if update::BUILD == 0 || crate::multiboot::live() {
            return None;
        }
        match self.updater.state() {
            State::Idle | State::UpToDate => Some("Check for updates"),
            State::Failed(_) => Some("Try again"),
            State::Ready(_) => Some("Restart now"),
            State::Checking | State::Downloading(_) => None,
        }
    }

    /// Show a page, as "Personalize" on the desktop's menu asks.
    pub fn show_page(&mut self, page: Page) {
        self.page = page;
        self.pressed = None;
        self.page_shown();
    }

    /// Opening the Update page looks for updates, if not done yet.
    fn page_shown(&mut self) {
        if self.page == Page::Update && self.updater.state() == State::Idle {
            self.updater.start();
        }
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        if self.page == Page::Time {
            if let Some(e) = self.time_edit.get_mut() {
                match e.field.on_key(key) {
                    FieldEvent::Enter => self.save_time(),
                    FieldEvent::Escape => *self.time_edit.get_mut() = None,
                    FieldEvent::Changed => e.error = false,
                    FieldEvent::None => {}
                }
                return true;
            }
        }
        let event = match self.dialog.get_mut() {
            Some(d) => d.on_key(key, dialog_area()),
            None => return false,
        };
        self.dialog_event(event)
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        match self.dialog.get_mut() {
            Some(d) => d.on_wheel(clicks, dialog_area()),
            None => false,
        }
    }

    fn dialog_event(&mut self, event: filedialog::Event) -> bool {
        match event {
            filedialog::Event::None => false,
            filedialog::Event::Redraw => true,
            filedialog::Event::Cancel => {
                *self.dialog.get_mut() = None;
                true
            }
            filedialog::Event::Chosen(path) => {
                *self.dialog.get_mut() = None;
                let readable = fs::read(&path)
                    .ok()
                    .is_some_and(|data| picture::decode(&data).is_some());
                if readable {
                    self.error = None;
                    personalize::set_wallpaper(&path);
                } else {
                    self.error =
                        Some("RyzikOS can't show that file. Pick a PNG, JPEG or BMP picture.");
                }
                true
            }
        }
    }

    /// A click on the Personalization page.
    fn personalize_click(&mut self, x: i32, y: i32) -> bool {
        let prefs = personalize::get();
        let Some((t, _)) = targets(&prefs).into_iter().find(|(_, r)| r.contains(x, y)) else {
            return false;
        };
        self.error = None;
        match t {
            Target::Mode(dark) => personalize::update(|p| p.dark = dark),
            Target::Accent(i) => personalize::update(|p| p.accent = theme::ACCENTS[i].1),
            Target::Slot(i) => {
                let bg = slots(&prefs)[i].clone();
                personalize::update(|p| p.background = bg);
            }
            Target::Browse => {
                let user = crate::users::current_name().unwrap_or_default();
                let dir = fs::join(&fs::home(user.as_str()), "Pictures");
                *self.dialog.get_mut() = Some(FileDialog::new(Mode::Open, &dir, ""));
            }
            Target::Fit(i) => personalize::update(|p| p.fit = FITS[i].0),
            Target::Color(i) => personalize::update(|p| p.color = COLORS[i]),
        }
        true
    }

    /// The page's buttons and where they are.
    fn buttons(&self) -> Vec<(Button, Rect)> {
        let mut out = Vec::new();
        match self.page {
            Page::Time => {
                out.push((Button::ChangeTime, row_button(0)));
                out.push((Button::AutoTime, row_button(1)));
                let (west, east) = zone_buttons();
                out.push((Button::ZoneWest, west));
                out.push((Button::ZoneEast, east));
                out.push((Button::SwitchLayout, row_button(3)));
                if self.time_edit.borrow().is_some() {
                    out.push((Button::SaveTime, time_save()));
                    out.push((Button::CancelTime, time_cancel()));
                }
            }
            Page::About => out.push((Button::OpenAbout, row_button(4))),
            Page::Update if self.update_label().is_some() => {
                out.push((Button::Update, row_button(0)))
            }
            _ => {}
        }
        out
    }

    fn button_at(&self, x: i32, y: i32) -> Option<Button> {
        self.buttons()
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(b, _)| b)
    }

    fn button_rect(&self, b: Button) -> Option<Rect> {
        self.buttons()
            .into_iter()
            .find(|&(k, _)| k == b)
            .map(|(_, r)| r)
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        if self.dialog.get_mut().is_some() {
            if let MouseKind::Down { right: false } = ev.kind {
                let event = match self.dialog.get_mut() {
                    Some(d) => d.on_click(dialog_area(), ev.x, ev.y),
                    None => filedialog::Event::None,
                };
                return self.dialog_event(event);
            }
            return false;
        }
        match ev.kind {
            MouseKind::Down { right: false } => {
                if let Some(i) = (0..PAGES.len()).find(|&i| nav_rect(i).contains(ev.x, ev.y)) {
                    let changed = self.page != PAGES[i].0;
                    self.page = PAGES[i].0;
                    self.page_shown();
                    return changed;
                }
                if self.page == Page::Personalization {
                    return self.personalize_click(ev.x, ev.y);
                }
                if self.page == Page::Time && time_field().contains(ev.x, ev.y) {
                    if let Some(e) = self.time_edit.get_mut() {
                        e.field.click(time_field(), ev.x);
                        return true;
                    }
                }
                match self.button_at(ev.x, ev.y) {
                    Some(b) => {
                        self.pressed = Some(b);
                        true
                    }
                    None => false,
                }
            }
            MouseKind::Up => {
                let Some(b) = self.pressed.take() else {
                    return false;
                };
                if self.button_at(ev.x, ev.y) == Some(b) {
                    match b {
                        Button::ChangeTime => {
                            let ((y, mo, d), (h, mi, _)) = clock::now();
                            let mut field = TextField::new(&format!(
                                "{:02}.{:02}.{} {:02}:{:02}",
                                d, mo, y, h, mi
                            ));
                            field.select_all();
                            *self.time_edit.get_mut() = Some(TimeEdit {
                                field,
                                error: false,
                            });
                        }
                        Button::SaveTime => self.save_time(),
                        Button::CancelTime => *self.time_edit.get_mut() = None,
                        Button::AutoTime => {
                            clock::set_auto(!clock::auto());
                            self.clock_tried = None;
                            if clock::auto() && crate::net::configured() {
                                self.check_clock();
                            }
                        }
                        Button::ZoneWest => clock::step_zone(-1),
                        Button::ZoneEast => clock::step_zone(1),
                        Button::SwitchLayout => self.switch_layout = true,
                        Button::OpenAbout => {
                            super::request_open(App::About);
                        }
                        Button::Update => match self.updater.state() {
                            State::Ready(_) => {
                                super::request_power(true);
                            }
                            _ => self.updater.start(),
                        },
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub fn draw(&self, c: &mut Canvas, info: &Info) {
        c.fill_rect(0, 0, cw(), ch(), theme::face());
        self.draw_nav(c, info);
        let title = PAGES.iter().find(|p| p.0 == self.page).map_or("", |p| p.1);
        c.draw_text_in(&TITLE, PAGE_X, 26, title, theme::text());

        let user = crate::users::current_name();
        let user = user.as_ref().map_or("nobody", |n| n.as_str());
        match self.page {
            Page::Personalization => self.draw_personalization(c),
            Page::System => {
                let storage = match fs::storage() {
                    fs::Storage::Disk => format!(
                        "System Disk, FAT32, {} MB",
                        fs::capacity() / (1024 * 1024)
                    ),
                    _ => String::from("No disk: files are kept in memory"),
                };
                let uptime = format!(
                    "{} h {} min",
                    info.uptime_minutes / 60,
                    info.uptime_minutes % 60
                );
                self.rows(
                    c,
                    &[
                        (
                            "Display",
                            "Resolution",
                            &format!("{} x {}", info.screen.0, info.screen.1),
                        ),
                        (
                            "Memory",
                            "Installed RAM",
                            &format!("{} MB", info.memory_mib),
                        ),
                        ("Storage", "Where your files are saved", &storage),
                        ("Uptime", "Time since RyzikOS started", &uptime),
                    ],
                );
            }
            Page::Network => {
                let address = if info.address.is_empty() {
                    "-"
                } else {
                    info.address
                };
                let adapter = crate::net::card_name().unwrap_or("None found");
                self.rows(
                    c,
                    &[
                        ("Ethernet", "Status", info.net.label()),
                        ("IPv4 address", "Given by DHCP", address),
                        ("Network adapter", "The card RyzikOS talks to", adapter),
                    ],
                );
            }
            Page::Time => {
                let layout = match info.layout {
                    Layout::Us => "English (ENG)",
                    Layout::Ru => "Russian (РУС)",
                };
                let now = format!("{}   {}", info.clock, info.date);
                let (auto_note, auto_value) = if !clock::auto() {
                    ("Off: the time you set is used", "Off")
                } else if clock::synced() {
                    ("Read from the internet", "On")
                } else if self.clock_sync.is_some() {
                    ("Asking the internet what time it is...", "On")
                } else {
                    ("From the internet, once it is connected", "On")
                };
                let zone = clock::zone_name(clock::zone());
                self.rows(
                    c,
                    &[
                        ("Date and time", "Shown in the menu bar", &now),
                        ("Set time automatically", auto_note, auto_value),
                        ("Time zone", "", &zone),
                        ("Keyboard layout", "Alt+Shift also switches it", layout),
                    ],
                );
                self.draw_button(c, Button::ChangeTime, "Change");
                let auto_label = if clock::auto() { "Turn off" } else { "Turn on" };
                self.draw_button(c, Button::AutoTime, auto_label);
                self.draw_button(c, Button::ZoneWest, "<  West");
                self.draw_button(c, Button::ZoneEast, "East  >");
                let label = match info.layout {
                    Layout::Us => "Switch to Russian",
                    Layout::Ru => "Switch to English",
                };
                self.draw_button(c, Button::SwitchLayout, label);
                self.draw_time_edit(c, info.caret);
            }
            Page::Accounts => {
                let mut rows: [(String, &str); 8] = Default::default();
                let n = crate::users::count().min(rows.len());
                for (i, row) in rows.iter_mut().enumerate().take(n) {
                    let name = crate::users::name(i);
                    row.0 = String::from(name.as_ref().map_or("", |n| n.as_str()));
                    row.1 = if crate::users::has_password(i) {
                        "Password set"
                    } else {
                        "No password"
                    };
                }
                for (i, (name, password)) in rows[..n].iter().enumerate() {
                    let r = row_rect(i);
                    card(c, r);
                    avatar(c, r.x + 16, r.y + 12, 32, name);
                    c.draw_text(r.x + 60, r.y + 10, name, theme::text());
                    let kind = if name == user {
                        "Signed in, local account"
                    } else {
                        "Local account"
                    };
                    c.draw_text(r.x + 60, r.y + 29, kind, theme::text_dim());
                    let w = UI.width(password);
                    c.draw_text(r.right() - 20 - w, r.y + 19, password, theme::text_dim());
                }
            }
            Page::Update => self.draw_update(c),
            Page::About => {
                self.rows(
                    c,
                    &[
                        ("Device name", "", "RYZIKOS-PC"),
                        (
                            "Operating system",
                            "",
                            &format!("RyzikOS {}", update::version()),
                        ),
                        ("Processor", "", "x86_64, long mode"),
                        ("Bootloader", "", info.bootloader),
                        ("About RyzikOS", "Version, license and this computer", ""),
                    ],
                );
                self.draw_button(c, Button::OpenAbout, "Open");
            }
        }
    }

    fn draw_nav(&self, c: &mut Canvas, info: &Info) {
        // the signed-in user at the top, like Windows
        let user = crate::users::current_name();
        let user = user.as_ref().map_or("nobody", |n| n.as_str());
        avatar(c, 20, 24, 56, user);
        c.draw_text_in(&UI_BOLD, 88, 34, user, theme::text());
        c.draw_text(88, 54, "Local account", theme::text_dim());

        for (i, &(page, label)) in PAGES.iter().enumerate() {
            let r = nav_rect(i);
            if page == self.page {
                c.fill_round(r, 5, theme::hover());
                c.fill_round(Rect::new(r.x, r.y + 10, 3, r.h - 20), 1, theme::accent());
            }
            let (x, y) = (r.x + 14, r.y + (r.h - 16) / 2);
            let pics = icons::get();
            match page {
                Page::System => pics.draw_pic(c, Pic::Computer, SMALL, x, y),
                Page::Network => {
                    let bg = if page == self.page {
                        theme::hover()
                    } else {
                        theme::face()
                    };
                    tray::network_icon(c, x, y + 1, info.net, theme::text(), bg);
                }
                Page::Time => {
                    c.outline_round(Rect::new(x, y, 16, 16), 8, theme::text());
                    c.fill_rect(x + 8, y + 3, 1, 6, theme::text());
                    c.fill_rect(x + 8, y + 8, 4, 1, theme::text());
                }
                Page::Accounts => {
                    c.fill_round(Rect::new(x + 4, y, 8, 8), 4, theme::accent());
                    c.fill_round(Rect::new(x + 1, y + 9, 14, 7), 3, theme::accent());
                }
                Page::About => pics.draw_small(c, App::About, x, y),
                Page::Update => {
                    let bg = if page == self.page {
                        theme::hover()
                    } else {
                        theme::face()
                    };
                    super::start::restart_symbol(c, x + 8, y + 8, theme::accent(), bg);
                }
                Page::Personalization => brush_icon(c, x, y),
            }
            c.draw_text(
                r.x + 42,
                r.y + (r.h - UI.line_height) / 2,
                label,
                theme::text(),
            );
        }
    }

    /// One card per row: a title, a line under it and a value on the right.
    fn rows(&self, c: &mut Canvas, rows: &[(&str, &str, &str)]) {
        for (i, (title, note, value)) in rows.iter().enumerate() {
            let r = row_rect(i);
            card(c, r);
            if note.is_empty() {
                c.draw_text(
                    r.x + 20,
                    r.y + (ROW_H - UI.line_height) / 2,
                    title,
                    theme::text(),
                );
            } else {
                c.draw_text(r.x + 20, r.y + 10, title, theme::text());
                c.draw_text(r.x + 20, r.y + 29, note, theme::text_dim());
            }
            // values go left of a button in the same row
            let right = self
                .buttons()
                .iter()
                .filter(|(_, b)| r.contains(b.x, b.y))
                .map(|(_, b)| b.x - 16)
                .min()
                .unwrap_or(r.right() - 20);
            let w = UI.width(value);
            let y = r.y + (ROW_H - UI.line_height) / 2;
            c.draw_text(right - w, y, value, theme::text_dim());
        }
    }

    fn draw_update(&self, c: &mut Canvas) {
        let state = self.updater.state();
        let (title, note) = match &state {
            State::Idle | State::Checking => (
                String::from("Checking for updates..."),
                String::from("Looking for a newer RyzikOS on GitHub"),
            ),
            State::UpToDate => (
                String::from("You're up to date"),
                String::from("This is the newest RyzikOS"),
            ),
            State::Downloading(b) => (
                format!("Downloading RyzikOS {}...", update::version_of(*b)),
                String::from("You can keep working meanwhile"),
            ),
            State::Ready(b) => (
                format!("RyzikOS {} is ready", update::version_of(*b)),
                String::from("Restart to finish installing it"),
            ),
            State::Failed(e) => (String::from("Couldn't update"), e.clone()),
        };
        let (title, note) = if crate::multiboot::live() {
            (
                String::from("Install RyzikOS to get updates"),
                String::from("The live CD can't be changed; an installed RyzikOS updates itself"),
            )
        } else if update::BUILD == 0 {
            (
                String::from("Updates are off"),
                String::from("This RyzikOS was built from source, not a GitHub release"),
            )
        } else {
            (title, note)
        };
        let r = row_rect(0);
        card(c, r);
        c.draw_text_in(&UI_BOLD, r.x + 20, r.y + 10, &title, theme::text());
        let color = if matches!(state, State::Failed(_)) {
            theme::error()
        } else {
            theme::text_dim()
        };
        c.draw_text(r.x + 20, r.y + 29, &note, color);
        if let Some(label) = self.update_label() {
            if let Some(b) = self.button_rect(Button::Update) {
                let pressed = self.pressed == Some(Button::Update);
                if matches!(state, State::Ready(_)) {
                    theme::accent_button(c, b, label, pressed);
                } else {
                    theme::button(c, b, label, pressed);
                }
            }
        }
        let rows = [
            ("Current version", update::version()),
            ("Updates come from", String::from("GitHub releases")),
            (
                "How it installs",
                String::from("Saved on the system disk, started on restart"),
            ),
        ];
        for (i, (label, value)) in rows.iter().enumerate() {
            let r = row_rect(i + 1);
            card(c, r);
            let y = r.y + (ROW_H - UI.line_height) / 2;
            c.draw_text(r.x + 20, y, label, theme::text());
            let w = UI.width(value);
            c.draw_text(r.right() - 20 - w, y, value, theme::text_dim());
        }
    }

    fn draw_button(&self, c: &mut Canvas, b: Button, label: &str) {
        if let Some(r) = self.button_rect(b) {
            theme::button(c, r, label, self.pressed == Some(b));
        }
    }

    /// The card for typing a new date and time, when it is open.
    fn draw_time_edit(&self, c: &mut Canvas, caret: bool) {
        let mut edit = self.time_edit.borrow_mut();
        let Some(e) = edit.as_mut() else {
            return;
        };
        let r = time_card();
        card(c, r);
        let (note, color) = if e.error {
            (
                "That isn't a date and time. Type it like 04.10.2026 15:30",
                theme::error(),
            )
        } else {
            (
                "Type the date and time (day.month.year hours:minutes), then Save",
                theme::text_dim(),
            )
        };
        c.draw_text(r.x + 20, r.y + 12, note, color);
        let f = time_field();
        c.fill_round(f, 4, theme::control());
        c.outline_round(f, 4, theme::accent());
        e.field.draw(c, f.inset(6), true, caret);
        // not through buttons(): it borrows time_edit too
        let save = self.pressed == Some(Button::SaveTime);
        theme::accent_button(c, time_save(), "Save", save);
        let cancel = self.pressed == Some(Button::CancelTime);
        theme::button(c, time_cancel(), "Cancel", cancel);
    }
}

impl Settings {
    fn draw_personalization(&self, c: &mut Canvas) {
        let prefs = personalize::get();
        let mut thumbs = self.thumbs.borrow_mut();

        // the look now, with a little window and taskbar on it
        let r = mode_card();
        card(c, r);
        let fresh = matches!(&thumbs.preview, Some((p, _)) if p.background == prefs.background
            && p.fit == prefs.fit && p.color == prefs.color);
        if !fresh {
            let pixels = wallpaper::thumbnail(&prefs.background, &prefs, PREVIEW.w, PREVIEW.h);
            thumbs.preview = Some((prefs.clone(), pixels));
        }
        if let Some((_, pixels)) = &thumbs.preview {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(PREVIEW, 6);
            m.blit(
                PREVIEW.x,
                PREVIEW.y,
                PREVIEW.w,
                PREVIEW.h,
                pixels,
                PREVIEW.w as usize,
            );
            let bar = Rect::new(PREVIEW.x, PREVIEW.bottom() - 12, PREVIEW.w, 12);
            m.fill_round_alpha(bar, 0, theme::taskbar(), 220);
            for k in 0..4 {
                let dot = Rect::new(PREVIEW.x + PREVIEW.w / 2 - 22 + k * 12, bar.y + 3, 7, 6);
                m.fill_round(
                    dot,
                    1,
                    if k == 0 {
                        theme::accent()
                    } else {
                        theme::thumb()
                    },
                );
            }
            let win = Rect::new(PREVIEW.x + 60, PREVIEW.y + 26, 128, 76);
            mini_window(&mut m, win, prefs.dark);
        }
        c.outline_round(PREVIEW, 6, theme::stroke());

        let x0 = mode_tile(false).x;
        c.draw_text_in(&UI_BOLD, x0, 84, "Choose your mode", theme::text());
        for dark in [false, true] {
            let t = mode_tile(dark);
            let on = prefs.dark == dark;
            c.fill_round(t, 6, theme::control());
            if on {
                c.outline_round(t, 6, theme::accent());
                c.outline_round(t.inset(1), 5, theme::accent());
            } else {
                c.outline_round(t, 6, theme::stroke());
            }
            let pic = Rect::new(t.x + 12, t.y + 10, t.w - 24, 56);
            mini_window(c, pic, dark);
            let label = if dark { "Dark" } else { "Light" };
            c.text_centered(
                Rect::new(t.x, t.bottom() - 30, t.w, 24),
                label,
                theme::text(),
            );
        }

        // accent colours
        let r = accent_card();
        card(c, r);
        c.draw_text(r.x + 20, r.y + 12, "Accent color", theme::text());
        c.draw_text(
            r.x + 20,
            r.y + 31,
            "Buttons, highlights and selections",
            theme::text_dim(),
        );
        for (i, &(_, color)) in theme::ACCENTS.iter().enumerate() {
            let s = swatch(r, i, theme::ACCENTS.len());
            color_dot(c, s, color, color == prefs.accent);
        }

        // backgrounds
        let r = background_card();
        card(c, r);
        c.draw_text(r.x + 20, r.y + 12, "Background", theme::text());
        let note = self
            .error
            .unwrap_or("Pictures are scaled to fit any screen size");
        let note_color = if self.error.is_some() {
            theme::error()
        } else {
            theme::text_dim()
        };
        c.draw_text(r.x + 20, r.y + 31, note, note_color);
        theme::button(c, browse_button(), "Browse photos", false);
        if thumbs.builtin.len() != wallpaper::BUILTIN.len() {
            let fill = Prefs {
                fit: personalize::Fit::Fill,
                ..prefs.clone()
            };
            thumbs.builtin = (0..wallpaper::BUILTIN.len())
                .map(|i| wallpaper::thumbnail(&Background::Builtin(i), &fill, THUMB_W, THUMB_H))
                .collect();
        }
        if let Background::Picture(path) = &prefs.background {
            if thumbs.picture.as_ref().is_none_or(|(p, _)| p != path) {
                let fill = Prefs {
                    fit: personalize::Fit::Fill,
                    ..prefs.clone()
                };
                let pixels = wallpaper::thumbnail(&prefs.background, &fill, THUMB_W, THUMB_H);
                thumbs.picture = Some((path.clone(), pixels));
            }
        }
        for (i, bg) in slots(&prefs).iter().enumerate() {
            let s = slot_rect(i);
            let label = match bg {
                Background::Builtin(k) => {
                    c.blit(s.x, s.y, s.w, s.h, &thumbs.builtin[*k], THUMB_W as usize);
                    String::from(wallpaper::BUILTIN[*k].0)
                }
                Background::Picture(path) => {
                    if let Some((_, pixels)) = &thumbs.picture {
                        c.blit(s.x, s.y, s.w, s.h, pixels, THUMB_W as usize);
                    }
                    String::from(fs::file_name(path))
                }
                Background::Solid => {
                    c.fill(s, prefs.color);
                    String::from("Solid color")
                }
            };
            if *bg == prefs.background {
                c.outline_round(s.inset(-3), 6, theme::accent());
                c.outline_round(s.inset(-2), 5, theme::accent());
            } else {
                c.outline_round(s, 2, theme::stroke());
            }
            let mut label = label;
            while UI.width(&label) > THUMB_W && label.pop().is_some() {}
            c.text_centered(
                Rect::new(s.x, s.bottom() + 6, s.w, 20),
                &label,
                theme::text(),
            );
        }

        // how pictures fit, and the colour behind them
        let r = fit_card();
        card(c, r);
        c.draw_text(
            r.x + 20,
            r.y + (r.h - UI.line_height) / 2,
            "Fit to screen",
            theme::text(),
        );
        for (i, &(fit, name)) in FITS.iter().enumerate() {
            theme::toggle_button(c, fit_button(i), name, prefs.fit == fit);
        }
        let r = color_card();
        card(c, r);
        c.draw_text(
            r.x + 20,
            r.y + (r.h - UI.line_height) / 2,
            "Background color",
            theme::text(),
        );
        for (i, &color) in COLORS.iter().enumerate() {
            color_dot(c, swatch(r, i, COLORS.len()), color, color == prefs.color);
        }

        if let Some(d) = self.dialog.borrow_mut().as_mut() {
            d.draw(c, dialog_area(), true);
        }
    }
}

/// A round colour to pick, with a ring when it is the chosen one.
fn color_dot(c: &mut Canvas, r: Rect, color: u32, on: bool) {
    if on {
        c.fill_round(r.inset(-3), (r.w + 6) / 2, theme::text());
        c.fill_round(r.inset(-1), (r.w + 2) / 2, theme::light());
    }
    c.fill_round(r, r.w / 2, color);
    c.outline_round(r, r.w / 2, mix(color, theme::text(), 50));
}

/// A tiny window in the light or dark look, for the previews.
fn mini_window(c: &mut Canvas, r: Rect, dark: bool) {
    let (face, bar, ink) = if dark {
        (
            rgb(0x2b, 0x2b, 0x2b),
            rgb(0x1c, 0x1c, 0x1c),
            rgb(0xe0, 0xe0, 0xe0),
        )
    } else {
        (
            rgb(0xff, 0xff, 0xff),
            rgb(0xee, 0xf1, 0xf8),
            rgb(0x30, 0x30, 0x30),
        )
    };
    c.fill_round(r, 5, face);
    {
        let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
        m.clip_round(r, 5);
        m.fill(Rect::new(r.x, r.y, r.w, 12), bar);
    }
    c.outline_round(
        r,
        5,
        if dark {
            rgb(0x50, 0x50, 0x54)
        } else {
            rgb(0xc0, 0xc2, 0xc8)
        },
    );
    for k in 0..3 {
        let w = if k == 2 { r.w / 3 } else { r.w - 24 };
        c.fill_round(
            Rect::new(r.x + 10, r.y + 20 + k * 9, w, 4),
            2,
            mix(face, ink, 90),
        );
    }
    let b = Rect::new(r.right() - 38, r.bottom() - 16, 28, 9);
    c.fill_round(b, 3, theme::accent_base());
}

/// A paint brush, for the Personalization page.
fn brush_icon(c: &mut Canvas, x: i32, y: i32) {
    c.fill_polygon(
        &[
            (x + 9, y + 9),
            (x + 14, y + 1),
            (x + 16, y + 3),
            (x + 11, y + 11),
        ],
        theme::text(),
    );
    c.fill_round(Rect::new(x + 2, y + 9, 8, 7), 3, theme::accent());
}

fn card(c: &mut Canvas, r: Rect) {
    c.fill_round(r, 6, theme::light());
    c.outline_round(r, 6, theme::stroke());
}

/// A round picture with the first letter of the name.
fn avatar(c: &mut Canvas, x: i32, y: i32, size: i32, name: &str) {
    let r = Rect::new(x, y, size, size);
    c.fill_round(r, size / 2, rgb(0x3a, 0x7c, 0xd0));
    let mut first = String::new();
    first.extend(name.chars().next().map(|ch| ch.to_ascii_uppercase()));
    let font = if size >= 48 { &TITLE } else { &UI_BOLD };
    c.text_centered_in(font, r, &first, 0xffffff);
}
