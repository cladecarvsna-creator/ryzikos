//! "Install RyzikOS", on the live CD's desktop: pick a hard disk, erase
//! it or keep its files, and copy RyzikOS onto it (see install.rs). The
//! steps are listed on the left, like most installers.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{Canvas, Rect};
use super::icons::{self, Pic, LARGE};
use super::text::{HEADING, UI, UI_BOLD};
use super::{theme, App, MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::fs::{self, InstallDisk};
use crate::install::{self, Step};
use crate::keyboard::Key;

pub const CLIENT_W: i32 = 780;
pub const CLIENT_H: i32 = 540;

const SIDE_W: i32 = 220;
const X: i32 = SIDE_W + 32;
const W: i32 = CLIENT_W - X - 32;
const DISK_H: i32 = 64;
const CHOICE_H: i32 = 58;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    Disk,
    /// "Erase Disk N?" before erasing.
    Confirm,
    Installing,
    Finished,
}

/// The installation running in a fiber: the step it is on, then the
/// outcome.
struct Run {
    fiber: Fiber,
    step: Rc<RefCell<Step>>,
    result: Rc<RefCell<Option<Result<(), String>>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Back,
    Next,
    Cancel,
    Restart,
    Disk(usize),
    Erase(bool),
}

pub struct Installer {
    page: Page,
    disks: Vec<InstallDisk>,
    chosen: Option<usize>,
    erase: bool,
    pressed: Option<Button>,
    run: Option<Run>,
    step: Step,
    error: Option<String>,
}

fn next_rect() -> Rect {
    Rect::new(CLIENT_W - 32 - 150, CLIENT_H - 28 - 34, 150, 34)
}

fn back_rect() -> Rect {
    Rect::new(CLIENT_W - 32 - 150 - 12 - 110, CLIENT_H - 28 - 34, 110, 34)
}

fn disk_rect(i: usize) -> Rect {
    Rect::new(X, 128 + i as i32 * (DISK_H + 8), W, DISK_H)
}

fn size_text(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= 10 * GIB {
        format!("{} GB", bytes / GIB)
    } else if bytes >= GIB {
        format!("{}.{} GB", bytes / GIB, bytes % GIB * 10 / GIB)
    } else {
        format!("{} MB", bytes / (1024 * 1024))
    }
}

impl Installer {
    pub fn new() -> Self {
        Self {
            page: Page::Welcome,
            disks: Vec::new(),
            chosen: None,
            erase: true,
            pressed: None,
            run: None,
            step: Step::Formatting,
            error: None,
        }
    }

    /// Start over when the window opens again, unless it is installing.
    pub fn reset(&mut self) {
        if self.run.is_none() && self.page != Page::Finished {
            *self = Self::new();
        }
    }

    fn load_disks(&mut self) {
        let room = install::boot_sectors().unwrap_or(u64::MAX);
        self.disks = fs::install_disks(room);
        self.disks.truncate(4);
        self.chosen = match self.disks.len() {
            1 => Some(0),
            _ => self.chosen.filter(|&i| i < self.disks.len()),
        };
        self.fix_choice();
    }

    /// Keeping files only works on a disk that has room for GRUB.
    fn fix_choice(&mut self) {
        let keep = self.chosen.is_some_and(|i| self.disks[i].keep);
        if !keep {
            self.erase = true;
        }
    }

    fn choice_rect(&self, erase: bool) -> Rect {
        let y = disk_rect(self.disks.len().max(1)).y + 18;
        let k = if erase { 0 } else { 1 };
        Rect::new(X, y + k * (CHOICE_H + 8), W, CHOICE_H)
    }

    /// What can be clicked on this page.
    fn buttons(&self) -> Vec<(Button, Rect)> {
        let mut out = Vec::new();
        match self.page {
            Page::Welcome => {
                out.push((Button::Cancel, back_rect()));
                out.push((Button::Next, next_rect()));
            }
            Page::Disk => {
                for i in 0..self.disks.len() {
                    out.push((Button::Disk(i), disk_rect(i)));
                }
                if let Some(i) = self.chosen {
                    out.push((Button::Erase(true), self.choice_rect(true)));
                    if self.disks[i].keep {
                        out.push((Button::Erase(false), self.choice_rect(false)));
                    }
                }
                out.push((Button::Back, back_rect()));
                out.push((Button::Next, next_rect()));
            }
            Page::Confirm => {
                out.push((Button::Back, back_rect()));
                out.push((Button::Next, next_rect()));
            }
            Page::Installing => {}
            Page::Finished => match self.error {
                Some(_) => out.push((Button::Back, next_rect())),
                None => out.push((Button::Restart, next_rect())),
            },
        }
        out
    }

    pub fn busy(&self) -> bool {
        self.run.is_some()
    }

    /// Run the installation a little. Returns true if something changed.
    pub fn tick(&mut self) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        let ended = run.fiber.resume();
        let step = *run.step.borrow();
        let changed = step != self.step;
        self.step = step;
        if !ended {
            return changed;
        }
        let result = run.result.borrow_mut().take();
        self.run = None;
        self.error = match result {
            Some(Ok(())) => None,
            Some(Err(e)) => Some(e),
            None => Some(String::from("The installation stopped.")),
        };
        self.page = Page::Finished;
        true
    }

    fn start(&mut self) {
        let Some(i) = self.chosen else {
            return;
        };
        let index = self.disks[i].index;
        let erase = self.erase;
        let step = Rc::new(RefCell::new(Step::Formatting));
        let result = Rc::new(RefCell::new(None));
        let (s, r) = (step.clone(), result.clone());
        let fiber = Fiber::new(move || {
            let outcome = install::install(index, erase, |now| {
                *s.borrow_mut() = now;
                // let the desktop draw the progress
                if crate::fiber::inside() {
                    crate::fiber::pause();
                }
            });
            *r.borrow_mut() = Some(outcome);
        });
        self.step = if erase { Step::Formatting } else { Step::Copying };
        self.error = None;
        self.page = Page::Installing;
        self.run = Some(Run { fiber, step, result });
    }

    fn press(&mut self, b: Button) {
        match (self.page, b) {
            (_, Button::Cancel) => {
                super::request_close(App::Installer);
            }
            (Page::Welcome, Button::Next) => {
                self.load_disks();
                self.page = Page::Disk;
            }
            (Page::Disk, Button::Back) => self.page = Page::Welcome,
            (Page::Disk, Button::Disk(i)) => {
                self.chosen = Some(i);
                self.fix_choice();
            }
            (Page::Disk, Button::Erase(erase)) => self.erase = erase,
            (Page::Disk, Button::Next) if self.chosen.is_some() => {
                if self.erase {
                    self.page = Page::Confirm;
                } else {
                    self.start();
                }
            }
            (Page::Confirm, Button::Back) => self.page = Page::Disk,
            (Page::Confirm, Button::Next) => self.start(),
            (Page::Finished, Button::Back) => {
                self.load_disks();
                self.page = Page::Disk;
            }
            (Page::Finished, Button::Restart) => {
                super::request_power(true);
            }
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        let hit = self.buttons().into_iter().find(|(_, r)| r.contains(ev.x, ev.y));
        match ev.kind {
            MouseKind::Down { right: false } => {
                self.pressed = hit.map(|(b, _)| b);
                self.pressed.is_some()
            }
            MouseKind::Up => {
                let Some(b) = self.pressed.take() else {
                    return false;
                };
                if hit.is_some_and(|(h, _)| h == b) {
                    self.press(b);
                }
                true
            }
            _ => false,
        }
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        let has = |b: Button| self.buttons().iter().any(|(x, _)| *x == b);
        match key {
            Key::Enter if has(Button::Next) => self.press(Button::Next),
            Key::Enter if has(Button::Restart) => self.press(Button::Restart),
            Key::Escape if has(Button::Back) => self.press(Button::Back),
            Key::Up | Key::Down if self.page == Page::Disk && !self.disks.is_empty() => {
                let n = self.disks.len();
                let i = self.chosen.map_or(0, |i| {
                    if matches!(key, Key::Up) {
                        (i + n - 1) % n
                    } else {
                        (i + 1) % n
                    }
                });
                self.press(Button::Disk(i));
            }
            _ => return false,
        }
        true
    }

    // ---- drawing --------------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, theme::light());
        self.draw_side(c);
        match self.page {
            Page::Welcome => self.draw_welcome(c),
            Page::Disk => self.draw_disks(c),
            Page::Confirm => self.draw_confirm(c),
            Page::Installing => self.draw_progress(c),
            Page::Finished => self.draw_finished(c),
        }
        for (b, r) in self.buttons() {
            let pressed = self.pressed == Some(b);
            let label = match b {
                Button::Back => "Back",
                Button::Cancel => "Cancel",
                Button::Next => match self.page {
                    Page::Welcome => "Next",
                    Page::Confirm => "Erase and install",
                    _ if self.erase => "Next",
                    _ => "Install",
                },
                Button::Restart => "Restart now",
                _ => continue,
            };
            let usable = b != Button::Next || self.page != Page::Disk || self.chosen.is_some();
            if b == Button::Next && self.page == Page::Confirm {
                theme::colored_button(c, r, label, theme::error(), pressed);
            } else if matches!(b, Button::Next | Button::Restart) && usable {
                theme::accent_button(c, r, label, pressed);
            } else {
                theme::button(c, r, label, pressed);
            }
        }
    }

    fn draw_side(&self, c: &mut Canvas) {
        c.fill_rect(0, 0, SIDE_W, CLIENT_H, theme::face());
        c.fill_rect(SIDE_W, 0, 1, CLIENT_H, theme::stroke());
        icons::get().draw_logo(c, 64, 28, 28);
        c.draw_text_in(&UI_BOLD, 28, 104, "RyzikOS", theme::text());
        c.draw_text(28, 124, &crate::update::version(), theme::text_dim());
        let steps = [
            ("Welcome", Page::Welcome),
            ("Choose a disk", Page::Disk),
            ("Install", Page::Installing),
            ("Finish", Page::Finished),
        ];
        let now = match self.page {
            Page::Confirm => Page::Disk,
            p => p,
        };
        let at = steps.iter().position(|s| s.1 == now).unwrap_or(0);
        for (i, (label, _)) in steps.iter().enumerate() {
            let y = 176 + i as i32 * 40;
            let dot = Rect::new(28, y, 22, 22);
            if i < at {
                c.fill_round(dot, 11, theme::accent());
                check(c, dot.x + 5, dot.y + 6, theme::on_accent());
            } else if i == at {
                c.fill_round(dot, 11, theme::accent());
                c.text_centered_in(&UI_BOLD, dot, &format!("{}", i + 1), theme::on_accent());
            } else {
                c.outline_round(dot, 11, theme::stroke());
                c.text_centered(dot, &format!("{}", i + 1), theme::text_dim());
            }
            let (font, color) = if i == at {
                (&UI_BOLD, theme::text())
            } else {
                (&UI, theme::text_dim())
            };
            c.draw_text_in(font, 62, y + 2, label, color);
        }
    }

    fn heading(&self, c: &mut Canvas, title: &str) {
        c.draw_text_in(&HEADING, X, 34, title, theme::text());
    }

    fn paragraph(&self, c: &mut Canvas, y: i32, text: &str, color: u32) -> i32 {
        wrap(c, X, y, W, text, color)
    }

    fn draw_welcome(&self, c: &mut Canvas) {
        self.heading(c, "Install RyzikOS");
        let mut y = 96;
        y = self.paragraph(
            c,
            y,
            "You're using RyzikOS from the live CD. Everything works, but nothing is kept: your files stay in memory until you restart.",
            theme::text(),
        ) + 14;
        y = self.paragraph(
            c,
            y,
            "The installer copies RyzikOS onto a hard disk. Then it starts without the CD, keeps your files and settings, and updates itself from GitHub.",
            theme::text(),
        ) + 26;
        let facts = [
            "IDE and SATA hard disks, up to 64 GB of it for RyzikOS",
            "Starts like any PC system: BIOS, or UEFI with CSM turned on",
            "Takes about 5 MB of the disk",
        ];
        for f in facts {
            check(c, X + 2, y + 5, theme::accent());
            c.draw_text(X + 24, y, f, theme::text());
            y += 26;
        }
    }

    fn draw_disks(&self, c: &mut Canvas) {
        self.heading(c, "Where should RyzikOS go?");
        c.draw_text(X, 90, "Pick a hard disk.", theme::text_dim());
        if self.disks.is_empty() {
            let r = disk_rect(0);
            card(c, r, false);
            c.draw_text_in(&UI_BOLD, r.x + 20, r.y + 12, "No hard disk found", theme::text());
            c.draw_text(
                r.x + 20,
                r.y + 34,
                "RyzikOS installs on IDE and SATA disks. NVMe and USB disks don't work yet.",
                theme::text_dim(),
            );
            return;
        }
        let pics = icons::get();
        for (i, d) in self.disks.iter().enumerate() {
            let r = disk_rect(i);
            let on = self.chosen == Some(i);
            card(c, r, on);
            pics.draw_pic(c, Pic::Drives, LARGE, r.x + 10, r.y + 8);
            c.draw_text_in(&UI_BOLD, r.x + 70, r.y + 12, &format!("{}  ·  {}", d.name, size_text(d.bytes)), theme::text());
            let status = if d.status == "FAT32" {
                format!("{}, FAT32, {} used", d.detail, size_text(d.used))
            } else {
                format!("{}, {}", d.detail, d.status.to_ascii_lowercase())
            };
            c.draw_text(r.x + 70, r.y + 34, &fit(&status, r.w - 90), theme::text_dim());
        }
        let Some(i) = self.chosen else {
            return;
        };
        let d = &self.disks[i];
        let erase_note = format!("Everything on {} is deleted", d.name);
        let options = [
            (true, "Erase the disk and install", erase_note),
            (
                false,
                "Install next to the files on it",
                String::from(if d.keep {
                    "Nothing is deleted; RyzikOS goes into a boot folder"
                } else {
                    "Only for a FAT32 disk that RyzikOS formatted"
                }),
            ),
        ];
        for (erase, title, note) in options {
            let r = self.choice_rect(erase);
            let usable = erase || d.keep;
            let on = self.erase == erase;
            card(c, r, on && usable);
            let dot = Rect::new(r.x + 18, r.y + (r.h - 18) / 2, 18, 18);
            c.outline_round(dot, 9, if usable { theme::text_dim() } else { theme::stroke() });
            if on {
                c.fill_round(dot.inset(4), 5, theme::accent());
            }
            let ink = if usable { theme::text() } else { theme::text_dim() };
            c.draw_text(r.x + 50, r.y + 10, title, ink);
            let note_ink = if erase && on { theme::error() } else { theme::text_dim() };
            c.draw_text(r.x + 50, r.y + 30, &note, note_ink);
        }
    }

    fn draw_confirm(&self, c: &mut Canvas) {
        let Some(d) = self.chosen.map(|i| &self.disks[i]) else {
            return;
        };
        self.heading(c, &format!("Erase {}?", d.name));
        let mut y = 96;
        y = self.paragraph(
            c,
            y,
            &format!(
                "{} ({}, {}) is formatted as FAT32. All files and systems on it are deleted and can't be brought back.",
                d.name,
                d.detail,
                size_text(d.bytes)
            ),
            theme::text(),
        ) + 14;
        self.paragraph(
            c,
            y,
            "If another system like Windows is on this disk, go back and pick another one.",
            theme::error(),
        );
    }

    fn draw_progress(&self, c: &mut Canvas) {
        self.heading(c, "Installing RyzikOS");
        c.draw_text(X, 90, "This takes a few seconds. Don't turn the computer off.", theme::text_dim());
        let steps = [
            (Step::Formatting, "Formatting the disk"),
            (Step::Copying, "Copying RyzikOS"),
            (Step::BootLoader, "Setting up the boot loader"),
        ];
        let order = |s: Step| steps.iter().position(|x| x.0 == s).unwrap_or(steps.len());
        let now = order(self.step);
        let mut y = 136;
        for (i, (step, label)) in steps.iter().enumerate() {
            if *step == Step::Formatting && !self.erase {
                continue;
            }
            if i < now {
                check(c, X + 2, y + 5, theme::accent());
                c.draw_text(X + 26, y, label, theme::text());
            } else if i == now {
                c.fill_round(Rect::new(X + 3, y + 5, 10, 10), 5, theme::accent());
                c.draw_text_in(&UI_BOLD, X + 26, y, label, theme::text());
            } else {
                c.outline_round(Rect::new(X + 3, y + 5, 10, 10), 5, theme::stroke());
                c.draw_text(X + 26, y, label, theme::text_dim());
            }
            y += 30;
        }
        let bar = Rect::new(X, y + 20, W, 8);
        c.fill_round(bar, 4, theme::track());
        let total = steps.len() as i32;
        let done = (now as i32).min(total);
        let w = (bar.w * (done * 2 + 1) / (total * 2)).clamp(8, bar.w);
        c.fill_round(Rect::new(bar.x, bar.y, w, bar.h), 4, theme::accent());
    }

    fn draw_finished(&self, c: &mut Canvas) {
        match &self.error {
            Some(e) => {
                self.heading(c, "RyzikOS wasn't installed");
                let y = self.paragraph(c, 96, e, theme::error());
                self.paragraph(
                    c,
                    y + 14,
                    "Go back to try again or pick another disk.",
                    theme::text(),
                );
            }
            None => {
                self.heading(c, "RyzikOS is installed");
                icons::get().draw(c, App::Installer, LARGE, CLIENT_W - 32 - 48, 30);
                let name = self.chosen.map_or("the disk", |i| self.disks[i].name.as_str());
                let mut y = self.paragraph(
                    c,
                    96,
                    &format!("RyzikOS is on {} now. Restart the computer to start it from there.", name),
                    theme::text(),
                ) + 14;
                y = self.paragraph(
                    c,
                    y,
                    "Take out the CD or USB stick first, or leave it in: its menu starts the hard disk by itself.",
                    theme::text(),
                ) + 14;
                self.paragraph(
                    c,
                    y,
                    "After signing in, Welcome shows what to do first.",
                    theme::text_dim(),
                );
            }
        }
    }
}

fn card(c: &mut Canvas, r: Rect, on: bool) {
    c.fill_round(r, 6, if on { theme::accent_light() } else { theme::light() });
    if on {
        c.outline_round(r, 6, theme::accent());
        c.outline_round(r.inset(1), 5, theme::accent());
    } else {
        c.outline_round(r, 6, theme::stroke());
    }
}

/// A small tick mark.
pub fn check(c: &mut Canvas, x: i32, y: i32, color: u32) {
    for i in 0..4 {
        c.fill_rect(x + i, y + 4 + i, 2, 2, color);
    }
    for i in 0..7 {
        c.fill_rect(x + 4 + i, y + 6 - i, 2, 2, color);
    }
}

/// Cut `text` to fit `w` pixels, with an ellipsis.
fn fit(text: &str, w: i32) -> String {
    if UI.width(text) <= w {
        return String::from(text);
    }
    let mut out = String::from(text);
    while !out.is_empty() && UI.width(&out) + UI.width("...") > w {
        out.pop();
    }
    out.push_str("...");
    out
}

/// Draw `text` wrapped at word breaks to `w` pixels. Returns the y under
/// the last line.
pub fn wrap(c: &mut Canvas, x: i32, mut y: i32, w: i32, text: &str, color: u32) -> i32 {
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() {
            String::from(word)
        } else {
            format!("{} {}", line, word)
        };
        if UI.width(&candidate) > w && !line.is_empty() {
            c.draw_text(x, y, &line, color);
            y += UI.line_height + 4;
            line = String::from(word);
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        c.draw_text(x, y, &line, color);
        y += UI.line_height + 4;
    }
    y
}
