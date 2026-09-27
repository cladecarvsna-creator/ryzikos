//! File Explorer, like the one in Windows 11: Back, Forward and Up, an
//! address bar with the path's parts, search, a command bar, the
//! navigation pane with the home folders, and the files as a details
//! list or large icons. Double-clicking a file opens it in Notepad.
//! Deleted items go to the Recycle Bin, which it shows like a folder
//! with Restore and Empty Recycle Bin.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::icons::{self, Pic, SMALL};
use super::text::UI;
use super::theme;
use super::widgets::{self, FieldEvent, Item, TextField};
use super::{MouseEvent, MouseKind};
use crate::fs::{self, recycle, Info};
use crate::keyboard::{self, Key};
use crate::{interrupts, users};

pub const CLIENT_W: i32 = 1120;
/// Where every disk and drive is listed.
pub const COMPUTER: &str = "computer:";
/// A drive in the Computer view.
const TILE_W: i32 = 400;
const TILE_H: i32 = 92;
pub const CLIENT_H: i32 = 680;

const NAV_H: i32 = 48;
const CMD_H: i32 = 44;
const TOP: i32 = NAV_H + CMD_H;
const STATUS_H: i32 = 26;
const SIDE_W: i32 = 210;
const SB: i32 = 14;
const HEADER_H: i32 = 30;
const ROW: i32 = 28;
const SIDE_ROW: i32 = 32;
const CELL_W: i32 = 120;
const CELL_H: i32 = 116;
const DOUBLE_CLICK: u64 = interrupts::TIMER_HZ / 2;

/// Details columns: where they start (from the content's left) and
/// their titles.
const COLUMNS: [(i32, &str); 4] = [
    (12, "Name"),
    (420, "Date modified"),
    (600, "Type"),
    (760, "Size"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Details,
    Icons,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Open,
    Rename,
    Delete,
    NewFolder,
    NewFile,
    Refresh,
    Details,
    Icons,
    Restore,
    Empty,
    SelectAll,
    /// Make the chosen picture the desktop background.
    SetBackground,
}

/// A menu entry: label, shortcut, and what it does (None is a separator
/// or, with a label, an item that can't be used now).
type Entry = (&'static str, &'static str, Option<Cmd>);

enum Dialog {
    /// Delete these items for good?
    Delete(Vec<usize>),
    /// Delete everything in the Recycle Bin for good?
    Empty,
    Message(&'static str),
}

/// Dragging a rectangle over the files to choose them, like Windows.
struct Band {
    /// Where the drag started and where the mouse is now, in content
    /// coordinates (scrolling included).
    from: (i32, i32),
    to: (i32, i32),
    /// Chosen before the drag (kept with Ctrl held).
    before: Vec<usize>,
}

impl Band {
    fn rect(&self) -> Rect {
        let (x0, x1) = (self.from.0.min(self.to.0), self.from.0.max(self.to.0));
        let (y0, y1) = (self.from.1.min(self.to.1), self.from.1.max(self.to.1));
        Rect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1)
    }
}

/// What has the keyboard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Address,
    Search,
    Rename,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceKind {
    Home,
    Library,
    Computer,
    Drive,
    Disc,
    Bin,
}

/// A place in the navigation pane.
struct Place {
    label: String,
    path: String,
    kind: PlaceKind,
}

pub struct Explorer {
    path: String,
    /// Everything in the folder, and what the search leaves of it.
    all: Vec<Info>,
    items: Vec<Info>,
    /// The item with the focus: clicked last, moved to with the keys.
    selected: Option<usize>,
    /// Other chosen items, picked with Ctrl, Shift or the rectangle.
    marked: Vec<usize>,
    /// Where Shift+click selects from.
    anchor: Option<usize>,
    /// The selection rectangle being dragged.
    band: Option<Band>,
    /// How far the files are scrolled, in pixels.
    scroll: i32,
    view: View,
    back: Vec<String>,
    forward: Vec<String>,
    focus: Focus,
    address: TextField,
    search: TextField,
    rename: TextField,
    /// The item being renamed.
    renaming: Option<usize>,
    menu: Option<(bool, Rect)>,
    menu_hover: Option<usize>,
    dialog: Option<Dialog>,
    hover: Option<usize>,
    side_hover: Option<usize>,
    last_click: (u64, usize),
    thumb_grab: Option<i32>,
    started: bool,
    /// `fs::changes()` when the folder was last read.
    seen: u32,
    /// A file to open in Notepad, for the desktop to pick up.
    pub open_request: Option<String>,
    /// The disks and drives, when Computer is shown.
    drives: Vec<fs::DriveInfo>,
}

fn content() -> Rect {
    Rect::new(
        SIDE_W + 1,
        TOP,
        CLIENT_W - SIDE_W - 1 - SB,
        CLIENT_H - TOP - STATUS_H,
    )
}

fn track() -> Rect {
    let c = content();
    Rect::new(c.right(), c.y, SB, c.h)
}

fn client() -> Rect {
    Rect::new(0, 0, CLIENT_W, CLIENT_H)
}

fn nav_button(i: i32) -> Rect {
    Rect::new(10 + i * 40, 8, 36, 32)
}

fn address_rect() -> Rect {
    Rect::new(176, 8, CLIENT_W - 176 - 262, 32)
}

fn search_rect() -> Rect {
    Rect::new(CLIENT_W - 250, 8, 238, 32)
}

/// Command bar buttons: what they do, their label and where they are.
/// The Recycle Bin has its own.
fn commands(bin: bool) -> [(Cmd, &'static str, Rect); 6] {
    let y = NAV_H + 6;
    let mut x = 12;
    let mut out = [(Cmd::Open, "", Rect::default()); 6];
    let buttons = if bin {
        [
            (Cmd::Empty, "Empty Trash"),
            (Cmd::Restore, "Restore"),
            (Cmd::Delete, "Delete"),
            (Cmd::Refresh, "Refresh"),
        ]
    } else {
        [
            (Cmd::NewFolder, "New folder"),
            (Cmd::NewFile, "New text document"),
            (Cmd::Rename, "Rename"),
            (Cmd::Delete, "Delete"),
        ]
    };
    for (i, (cmd, label)) in buttons.into_iter().enumerate() {
        let w = UI.width(label) + 44;
        out[i] = (cmd, label, Rect::new(x, y, w, 32));
        x += w + if i == 1 { 20 } else { 6 };
    }
    let right = CLIENT_W - 12;
    out[4] = (Cmd::Details, "Details", Rect::new(right - 224, y, 104, 32));
    out[5] = (
        Cmd::Icons,
        "Large icons",
        Rect::new(right - 116, y, 116, 32),
    );
    out
}

fn user() -> String {
    String::from(users::current_name().unwrap_or_default().as_str())
}

fn home() -> String {
    fs::home(&user())
}

/// The current user's Recycle Bin folder.
pub fn bin_folder() -> String {
    recycle::folder(&user())
}

/// Drives the navigation pane has room for.
const SIDE_DRIVES: usize = 6;

fn places() -> Vec<Place> {
    let home = home();
    let mut out = vec![Place {
        label: String::from("Home"),
        path: home.clone(),
        kind: PlaceKind::Home,
    }];
    for lib in fs::LIBRARIES {
        out.push(Place {
            label: String::from(lib),
            path: fs::join(&home, lib),
            kind: PlaceKind::Library,
        });
    }
    out.push(Place {
        label: String::from("Computer"),
        path: String::from(COMPUTER),
        kind: PlaceKind::Computer,
    });
    for d in fs::drives().into_iter().take(SIDE_DRIVES) {
        out.push(Place {
            label: d.name,
            path: d.path,
            kind: if d.kind == fs::DriveKind::Cd {
                PlaceKind::Disc
            } else {
                PlaceKind::Drive
            },
        });
    }
    out.push(Place {
        label: String::from("Trash"),
        path: bin_folder(),
        kind: PlaceKind::Bin,
    });
    out
}

/// Where place `i` is in the navigation pane: a gap before the libraries
/// and another before the devices (Computer, the disks, the drives).
fn place_rect(i: usize, n: usize) -> Rect {
    let gap = if i == 0 {
        0
    } else if i + 1 == n {
        68
    } else if i > fs::LIBRARIES.len() {
        56
    } else {
        12
    };
    Rect::new(
        6,
        TOP + 10 + i as i32 * SIDE_ROW + gap,
        SIDE_W - 12,
        SIDE_ROW - 2,
    )
}

/// "1.5 GB", "300 MB" for disk sizes.
fn big_size(bytes: u64) -> String {
    let mb = bytes / (1024 * 1024);
    if mb >= 1024 {
        format!("{}.{} GB", mb / 1024, mb % 1024 * 10 / 1024)
    } else {
        format!("{} MB", mb)
    }
}

fn type_name(item: &Info) -> String {
    if item.dir {
        return String::from("File folder");
    }
    match item.name.rfind('.') {
        Some(i) if i > 0 => {
            let ext = &item.name[i + 1..];
            let lower = ext.to_ascii_lowercase();
            if fs::same_name(ext, "txt") {
                String::from("Text Document")
            } else if ["png", "jpg", "jpeg", "bmp"].contains(&lower.as_str()) {
                String::from("Picture")
            } else if ["avi", "mjpg", "mjpeg"].contains(&lower.as_str()) {
                String::from("Video")
            } else if lower == "rzapp" {
                String::from("RyzikOS Program")
            } else if lower == "rzlink" {
                String::from("Shortcut")
            } else {
                let mut s: String = ext.chars().flat_map(char::to_uppercase).collect();
                s.push_str(" File");
                s
            }
        }
        _ => String::from("File"),
    }
}

/// Sizes as Windows shows them: whole kilobytes, rounded up.
fn size_text(size: u32) -> String {
    let mut s = String::new();
    let kb = (size as u64).div_ceil(1024);
    if kb < 1024 * 10 {
        let _ = write!(s, "{} KB", kb);
    } else {
        let _ = write!(s, "{} MB", kb.div_ceil(1024));
    }
    s
}

/// Cut text to fit `w` pixels, ending with "…".
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

impl Explorer {
    pub fn new() -> Self {
        Self {
            path: String::from("/"),
            all: Vec::new(),
            items: Vec::new(),
            selected: None,
            marked: Vec::new(),
            anchor: None,
            band: None,
            scroll: 0,
            view: View::Details,
            back: Vec::new(),
            forward: Vec::new(),
            focus: Focus::List,
            address: TextField::default(),
            search: TextField::default(),
            rename: TextField::default(),
            renaming: None,
            menu: None,
            menu_hover: None,
            dialog: None,
            hover: None,
            side_hover: None,
            last_click: (0, usize::MAX),
            thumb_grab: None,
            started: false,
            seen: 0,
            open_request: None,
            drives: Vec::new(),
        }
    }

    /// The window opened: show the home folder the first time.
    pub fn start(&mut self) {
        if !self.started {
            self.started = true;
            let home = home();
            let path = if fs::is_dir(&home) {
                home
            } else {
                String::from("/")
            };
            self.load(&path);
        } else {
            self.refresh();
        }
    }

    /// Show a folder, from the shell's `explorer` command.
    pub fn show(&mut self, path: &str) {
        self.started = true;
        self.navigate(path);
    }

    /// Whether the Recycle Bin (or a folder in it) is shown.
    fn in_bin(&self) -> bool {
        recycle::contains(&user(), &self.path)
    }

    /// Whether the Recycle Bin itself is shown, where items can be
    /// restored.
    fn at_bin(&self) -> bool {
        fs::same_name(&self.path, &bin_folder())
    }

    /// Show the folder a file or folder is in, with it selected.
    pub fn reveal(&mut self, path: &str) {
        self.started = true;
        self.navigate(&fs::parent(path));
        self.select_name(fs::file_name(path));
    }

    /// The window title: the folder's name.
    pub fn title(&self) -> String {
        if self.at_bin() {
            return String::from("Trash - Files");
        }
        let mut t = self.place_name();
        t.push_str(" - Files");
        t
    }

    /// What the shown folder is called: its name, or the drive's.
    fn place_name(&self) -> String {
        if self.at_computer() {
            return String::from("Computer");
        }
        if let Some(d) = fs::drives().into_iter().find(|d| fs::same_name(&d.path, &self.path)) {
            return d.name;
        }
        String::from(fs::file_name(&self.path))
    }

    fn at_computer(&self) -> bool {
        self.path == COMPUTER
    }

    // ---- folders ---------------------------------------------------------------

    /// Read the folder again if files changed elsewhere, as in Notepad.
    /// Returns whether it did.
    pub fn check_changes(&mut self) -> bool {
        if !self.started || self.seen == fs::changes() || self.renaming.is_some() {
            return false;
        }
        self.refresh();
        true
    }

    fn load(&mut self, path: &str) -> bool {
        if path == COMPUTER {
            fs::refresh_disc();
        }
        self.seen = fs::changes();
        let listed = if path == COMPUTER {
            self.drives = fs::drives();
            Ok(self
                .drives
                .iter()
                .map(|d| Info {
                    name: d.name.clone(),
                    dir: true,
                    size: 0,
                    modified: (0, 0, 0, 0, 0),
                })
                .collect())
        } else {
            if fs::is_on_disc(path) {
                // a disc may have gone in since
                fs::refresh_disc();
            }
            fs::list(path)
        };
        match listed {
            Ok(items) => {
                if !fs::same_name(path, &self.path) {
                    self.search = TextField::default();
                }
                self.path = String::from(path);
                self.all = items;
                self.filter();
                self.select_one(None);
                self.scroll = 0;
                self.renaming = None;
                self.focus = Focus::List;
                true
            }
            Err(e) => {
                self.dialog = Some(Dialog::Message(e.message()));
                false
            }
        }
    }

    fn filter(&mut self) {
        let q: String = self.search.string().to_lowercase();
        self.items = self
            .all
            .iter()
            .filter(|i| q.is_empty() || i.name.to_lowercase().contains(&q))
            .cloned()
            .collect();
    }

    fn navigate(&mut self, path: &str) {
        // typed as an address or given to the shell's explorer command
        let bare = path.trim_matches('/');
        let path = if bare.eq_ignore_ascii_case(COMPUTER) || bare.eq_ignore_ascii_case("computer") {
            COMPUTER
        } else {
            path
        };
        let old = self.path.clone();
        if self.load(path) && !fs::same_name(&old, path) {
            self.back.push(old);
            self.forward.clear();
        }
    }

    fn go_back(&mut self) {
        if let Some(p) = self.back.pop() {
            let old = self.path.clone();
            if self.load(&p) {
                self.forward.push(old);
            }
        }
    }

    fn go_forward(&mut self) {
        if let Some(p) = self.forward.pop() {
            let old = self.path.clone();
            if self.load(&p) {
                self.back.push(old);
            }
        }
    }

    fn go_up(&mut self) {
        let drive_root = fs::drives().iter().any(|d| fs::same_name(&d.path, &self.path));
        if self.at_bin() || drive_root {
            let from = self.place_name();
            self.navigate(COMPUTER);
            self.select_name(&from);
        } else if !self.at_computer() {
            let up = fs::parent(&self.path);
            let from = String::from(fs::file_name(&self.path));
            self.navigate(&up);
            // select the folder we came from
            self.select_name(&from);
        }
    }

    /// Read the folder again, keeping the selection.
    fn refresh(&mut self) {
        let name = self.selected.map(|i| self.items[i].name.clone());
        let path = self.path.clone();
        if !self.load(&path) {
            // the folder is gone: go to the top
            self.load("/");
        }
        if let Some(n) = name {
            self.select_name(&n);
        }
    }

    fn select_name(&mut self, name: &str) {
        let i = self.items.iter().position(|i| i.name == name);
        self.select_one(i);
        self.scroll_to_selected();
    }

    /// Choose one item, or none.
    fn select_one(&mut self, i: Option<usize>) {
        self.selected = i;
        self.anchor = i;
        self.marked.clear();
    }

    /// Everything chosen, in order.
    fn chosen(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .marked
            .iter()
            .copied()
            .chain(self.selected)
            .filter(|&i| i < self.items.len())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn is_chosen(&self, i: usize) -> bool {
        self.selected == Some(i) || self.marked.contains(&i)
    }

    /// Choose the items from the anchor to `i`, as Shift does.
    fn select_range(&mut self, i: usize) {
        let a = self.anchor.unwrap_or(i);
        self.marked = (a.min(i)..=a.max(i)).collect();
        self.selected = Some(i);
    }

    fn select_all(&mut self) {
        self.marked = (0..self.items.len()).collect();
        if self.selected.is_none() && !self.items.is_empty() {
            self.selected = Some(0);
        }
    }

    /// The single chosen item, when exactly one is.
    fn single(&self) -> Option<usize> {
        match self.chosen()[..] {
            [i] => Some(i),
            _ => None,
        }
    }

    fn selected_path(&self) -> Option<String> {
        self.single()
            .map(|i| fs::join(&self.path, &self.items[i].name))
    }

    /// The chosen picture file, for "Set as desktop background".
    fn chosen_picture(&self) -> Option<String> {
        let i = self.single()?;
        let item = &self.items[i];
        (!item.dir && super::picture::is_picture(&item.name) && !self.in_bin())
            .then(|| fs::join(&self.path, &item.name))
    }

    // ---- commands --------------------------------------------------------------

    fn open(&mut self, i: usize) {
        if self.at_computer() {
            let Some(d) = self.drives.iter().find(|d| d.name == self.items[i].name).cloned() else {
                return;
            };
            if d.kind == fs::DriveKind::Cd && !d.ready {
                // look again: a disc may have gone in
                fs::refresh_disc();
                if let Some(now) = fs::drives().into_iter().find(|n| n.path == d.path && n.ready) {
                    self.navigate(&now.path);
                } else {
                    self.dialog = Some(Dialog::Message("There is no disc in the drive."));
                    self.refresh();
                }
            } else if d.ready {
                self.navigate(&d.path);
            } else {
                self.dialog = Some(Dialog::Message(
                    "This disk has a format RyzikOS can't read. Only FAT32 disks can be opened.",
                ));
            }
            return;
        }
        let item = &self.items[i];
        let path = fs::join(&self.path, &item.name);
        if item.dir {
            self.navigate(&path);
        } else {
            self.open_request = Some(path);
        }
    }

    fn run(&mut self, cmd: Cmd) {
        self.menu = None;
        self.commit_rename();
        match cmd {
            Cmd::Open => {
                // files open one by one; a folder is gone into
                let chosen = self.chosen();
                if let Some(&i) = chosen.iter().find(|&&i| self.items[i].dir) {
                    self.open(i);
                } else if let Some(&i) = chosen.first() {
                    self.open(i);
                }
            }
            Cmd::Rename => {
                if let Some(i) = self.single() {
                    self.start_rename(i);
                }
            }
            Cmd::Delete => {
                let chosen = self.chosen();
                if !chosen.is_empty() {
                    if self.in_bin() {
                        self.dialog = Some(Dialog::Delete(chosen));
                    } else {
                        // to the Recycle Bin, without asking, like Windows
                        self.recycle(&chosen);
                    }
                }
            }
            Cmd::Restore => {
                let chosen = self.chosen();
                if self.at_bin() && !chosen.is_empty() {
                    for &i in &chosen {
                        let name = self.items[i].name.clone();
                        if let Err(e) = recycle::restore(&user(), &name) {
                            self.dialog = Some(Dialog::Message(e.message()));
                        }
                    }
                    self.reload_near(chosen[0]);
                }
            }
            Cmd::SelectAll => self.select_all(),
            Cmd::SetBackground => {
                if let Some(path) = self.chosen_picture() {
                    super::personalize::set_wallpaper(&path);
                }
            }
            Cmd::Empty => {
                if !self.items.is_empty() {
                    self.dialog = Some(Dialog::Empty);
                }
            }
            Cmd::NewFolder => self.create(true),
            Cmd::NewFile => self.create(false),
            Cmd::Refresh => self.refresh(),
            Cmd::Details => self.view = View::Details,
            Cmd::Icons => self.view = View::Icons,
        }
        self.clamp_scroll();
    }

    fn create(&mut self, folder: bool) {
        let name = if folder {
            fs::unique_name(&self.path, "New folder", "")
        } else {
            fs::unique_name(&self.path, "New Text Document", ".txt")
        };
        let path = fs::join(&self.path, &name);
        let result = if folder {
            fs::create_dir(&path)
        } else {
            fs::write(&path, &[])
        };
        match result {
            Ok(()) => {
                self.search = TextField::default();
                self.refresh();
                self.select_name(&name);
                if let Some(i) = self.selected {
                    self.start_rename(i);
                }
            }
            Err(e) => self.dialog = Some(Dialog::Message(e.message())),
        }
    }

    fn start_rename(&mut self, i: usize) {
        let name = self.items[i].name.clone();
        self.rename = TextField::new(&name);
        // the name without its extension is selected, as in Windows
        let base = match name.rfind('.') {
            Some(p) if p > 0 && !self.items[i].dir => name[..p].chars().count(),
            _ => name.chars().count(),
        };
        self.rename.select(0, base);
        self.renaming = Some(i);
        self.focus = Focus::Rename;
        self.scroll_to_selected();
    }

    fn commit_rename(&mut self) {
        let Some(i) = self.renaming.take() else {
            return;
        };
        self.focus = Focus::List;
        let new = self.rename.string();
        let new = new.trim().trim_end_matches('.');
        let old = self.items[i].name.clone();
        if new.is_empty() || new == old {
            return;
        }
        match fs::rename(&fs::join(&self.path, &old), new) {
            Ok(()) => {
                let new = String::from(new);
                self.refresh();
                self.select_name(&new);
            }
            Err(e) => self.dialog = Some(Dialog::Message(e.message())),
        }
    }

    /// Delete items for good (in the Recycle Bin).
    fn delete(&mut self, items: &[usize]) {
        let names: Vec<String> = items
            .iter()
            .filter_map(|&i| self.items.get(i).map(|it| it.name.clone()))
            .collect();
        for name in &names {
            let result = if self.at_bin() {
                recycle::purge(&user(), name)
            } else {
                fs::remove(&fs::join(&self.path, name))
            };
            if let Err(e) = result {
                self.dialog = Some(Dialog::Message(e.message()));
            }
        }
        self.reload_near(items.first().copied().unwrap_or(0));
    }

    /// Move items to the Recycle Bin.
    fn recycle(&mut self, items: &[usize]) {
        let paths: Vec<String> = items
            .iter()
            .filter_map(|&i| self.items.get(i).map(|it| fs::join(&self.path, &it.name)))
            .collect();
        for path in &paths {
            if let Err(e) = recycle::recycle(&user(), path) {
                self.dialog = Some(Dialog::Message(e.message()));
            }
        }
        self.reload_near(items.first().copied().unwrap_or(0));
    }

    fn empty_bin(&mut self) {
        if let Err(e) = recycle::empty(&user()) {
            self.dialog = Some(Dialog::Message(e.message()));
        }
        self.reload_near(0);
    }

    /// Read the folder again after an item went, selecting the one that
    /// took its place.
    fn reload_near(&mut self, i: usize) {
        let path = self.path.clone();
        self.load(&path);
        if !self.items.is_empty() {
            self.select_one(Some(i.min(self.items.len() - 1)));
        }
    }

    // ---- layout ----------------------------------------------------------------

    fn columns(&self) -> i32 {
        ((content().w - 16) / CELL_W).max(1)
    }

    fn tile_columns(&self) -> i32 {
        ((content().w - 24) / TILE_W).max(1)
    }

    fn content_height(&self) -> i32 {
        let n = self.items.len() as i32;
        if self.at_computer() {
            let cols = self.tile_columns();
            return 44 + (n + cols - 1) / cols * (TILE_H + 12) + 16;
        }
        match self.view {
            View::Details => HEADER_H + n * ROW + 8,
            View::Icons => (n + self.columns() - 1) / self.columns() * CELL_H + 16,
        }
    }

    /// Where item `i` is drawn, before scrolling is taken into account.
    fn item_rect(&self, i: usize) -> Rect {
        let c = content();
        if self.at_computer() {
            let cols = self.tile_columns();
            let (col, row) = (i as i32 % cols, i as i32 / cols);
            return Rect::new(
                c.x + 16 + col * (TILE_W + 12),
                c.y + 44 + row * (TILE_H + 12),
                TILE_W,
                TILE_H,
            );
        }
        match self.view {
            View::Details => Rect::new(c.x + 6, c.y + HEADER_H + i as i32 * ROW, c.w - 12, ROW),
            View::Icons => {
                let cols = self.columns();
                let (col, row) = (i as i32 % cols, i as i32 / cols);
                Rect::new(
                    c.x + 8 + col * CELL_W,
                    c.y + 8 + row * CELL_H,
                    CELL_W - 6,
                    CELL_H - 6,
                )
            }
        }
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        let c = content();
        let header = self.view == View::Details && !self.at_computer();
        if !c.contains(x, y) || (header && y < c.y + HEADER_H) {
            return None;
        }
        (0..self.items.len()).find(|&i| self.item_rect(i).offset(0, -self.scroll).contains(x, y))
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_height() - content().h).max(0);
        self.scroll = self.scroll.clamp(0, max);
    }

    fn scroll_to_selected(&mut self) {
        let Some(i) = self.selected else {
            return;
        };
        let c = content();
        let r = self.item_rect(i);
        let top = if self.view == View::Details && !self.at_computer() {
            c.y + HEADER_H
        } else {
            c.y
        };
        if r.y - self.scroll < top {
            self.scroll = r.y - top;
        } else if r.bottom() - self.scroll > c.bottom() {
            self.scroll = r.bottom() - c.bottom() + 4;
        }
        self.clamp_scroll();
    }

    /// The parts of the address: label, folder and where each is drawn.
    fn crumbs(&self) -> Vec<(String, String, Rect)> {
        let a = address_rect();
        let computer = (String::from("Computer"), String::from(COMPUTER));
        let mut parts = vec![computer.clone()];
        if !self.at_computer() {
            let drives = fs::drives();
            parts.push((String::from("System Disk"), String::from("/")));
            let mut acc = String::new();
            let bin = bin_folder();
            for p in self.path.split('/').filter(|p| !p.is_empty()) {
                acc.push('/');
                acc.push_str(p);
                if fs::same_name(&acc, &bin) {
                    // the Trash is a place of its own
                    parts.clear();
                    parts.push((String::from("Trash"), acc.clone()));
                } else if let Some(d) = drives.iter().find(|d| fs::same_name(&d.path, &acc)) {
                    parts.clear();
                    parts.push(computer.clone());
                    parts.push((d.name.clone(), acc.clone()));
                } else if !fs::same_name(&acc, recycle::ROOT) {
                    parts.push((String::from(p), acc.clone()));
                }
            }
        }
        // drop parts from the front until they fit
        let sep = 22;
        let width = |ps: &[(String, String)]| -> i32 {
            ps.iter().map(|(l, _)| UI.width(l) + 12 + sep).sum::<i32>() + 32
        };
        let mut start = 0;
        while start + 1 < parts.len() && width(&parts[start..]) > a.w {
            start += 1;
        }
        let mut x = a.x + 32;
        let mut out = Vec::new();
        for (label, path) in parts.into_iter().skip(start) {
            let w = UI.width(&label) + 12;
            out.push((label, path, Rect::new(x, a.y + 3, w, a.h - 6)));
            x += w + sep;
        }
        out
    }

    // ---- input -----------------------------------------------------------------

    pub fn on_key(&mut self, key: Key) -> bool {
        if let Some(d) = &self.dialog {
            match key {
                Key::Enter => match d {
                    Dialog::Delete(items) => {
                        let items = items.clone();
                        self.dialog = None;
                        self.delete(&items);
                    }
                    Dialog::Empty => {
                        self.dialog = None;
                        self.empty_bin();
                    }
                    Dialog::Message(_) => self.dialog = None,
                },
                Key::Escape => self.dialog = None,
                _ => return false,
            }
            return true;
        }
        if self.menu.is_some() {
            if let Key::Escape = key {
                self.menu = None;
                return true;
            }
        }
        match self.focus {
            Focus::Address => {
                match self.address.on_key(key) {
                    FieldEvent::Enter => {
                        let path = fs::parse(&self.address.string());
                        self.focus = Focus::List;
                        if fs::is_dir(&path) {
                            self.navigate(&path);
                        } else if fs::exists(&path) {
                            self.open_request = Some(path);
                        } else {
                            self.dialog = Some(Dialog::Message(fs::Error::NotFound.message()));
                        }
                    }
                    FieldEvent::Escape => self.focus = Focus::List,
                    FieldEvent::Changed => {}
                    FieldEvent::None => return false,
                }
                return true;
            }
            Focus::Search => {
                match self.search.on_key(key) {
                    FieldEvent::Changed => {
                        self.filter();
                        self.select_one(None);
                        self.scroll = 0;
                    }
                    FieldEvent::Escape => {
                        self.search = TextField::default();
                        self.filter();
                        self.focus = Focus::List;
                    }
                    FieldEvent::Enter => {
                        self.focus = Focus::List;
                        if !self.items.is_empty() {
                            self.select_one(Some(0));
                        }
                    }
                    FieldEvent::None => return false,
                }
                return true;
            }
            Focus::Rename => {
                match self.rename.on_key(key) {
                    FieldEvent::Enter => self.commit_rename(),
                    FieldEvent::Escape => {
                        self.renaming = None;
                        self.focus = Focus::List;
                    }
                    FieldEvent::Changed => {}
                    FieldEvent::None => return false,
                }
                return true;
            }
            Focus::List => {}
        }
        let n = self.items.len();
        let step = match self.view {
            View::Details => 1,
            View::Icons => self.columns() as usize,
        };
        // Shift with the arrows chooses everything on the way
        let move_to = |s: &mut Self, i: usize| {
            if n > 0 {
                let i = i.min(n - 1);
                if keyboard::shift_held() {
                    s.select_range(i);
                } else {
                    s.select_one(Some(i));
                }
                s.scroll_to_selected();
            }
        };
        match key {
            Key::Down => {
                let i = self.selected.map_or(0, |i| i + step);
                move_to(
                    self,
                    if i >= n {
                        self.selected.unwrap_or(0)
                    } else {
                        i
                    },
                );
            }
            Key::Up => {
                let i = self.selected.map_or(0, |i| i.saturating_sub(step));
                move_to(self, i);
            }
            Key::Right if self.view == View::Icons => {
                move_to(self, self.selected.map_or(0, |i| i + 1));
            }
            Key::Left if self.view == View::Icons => {
                move_to(self, self.selected.map_or(0, |i| i.saturating_sub(1)));
            }
            Key::Home => move_to(self, 0),
            Key::End => move_to(self, n.saturating_sub(1)),
            Key::Enter => self.run(Cmd::Open),
            Key::Delete => self.run(Cmd::Delete),
            Key::Function(2) => self.run(Cmd::Rename),
            Key::Function(5) => self.run(Cmd::Refresh),
            Key::Backspace => {
                if self.back.is_empty() {
                    self.go_up();
                } else {
                    self.go_back();
                }
            }
            Key::Escape => self.select_one(None),
            Key::Ctrl('a') => self.select_all(),
            Key::Ctrl('n') if keyboard::shift_held() => self.run(Cmd::NewFolder),
            Key::Ctrl('f') | Key::Ctrl('e') => self.focus = Focus::Search,
            Key::Ctrl('l') => self.edit_address(),
            Key::Char(c) if !c.is_control() && c != ' ' => {
                // jump to the next name starting with that letter
                let start = self.selected.map_or(0, |i| i + 1);
                let lower: String = c.to_lowercase().collect();
                let found = (0..n)
                    .map(|k| (start + k) % n)
                    .find(|&k| self.items[k].name.to_lowercase().starts_with(&lower));
                match found {
                    Some(k) => move_to(self, k),
                    None => return false,
                }
            }
            _ => return false,
        }
        true
    }

    fn edit_address(&mut self) {
        self.commit_rename();
        self.address = TextField::new(&fs::display(&self.path));
        self.address.select_all();
        self.focus = Focus::Address;
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        let old = self.scroll;
        self.scroll += clicks * 3 * ROW;
        self.clamp_scroll();
        old != self.scroll
    }

    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let old = (self.hover, self.side_hover, self.menu_hover);
        if let Some((on_item, r)) = self.menu {
            let items = self.menu_items(on_item);
            self.menu_hover = widgets::menu_item_at(r, &items, x, y);
        }
        let covered = self.menu.is_some_and(|(_, r)| r.contains(x, y)) || self.dialog.is_some();
        self.hover = if covered { None } else { self.item_at(x, y) };
        let n = places().len();
        self.side_hover = if covered {
            None
        } else {
            (0..n).find(|&i| place_rect(i, n).contains(x, y))
        };
        old != (self.hover, self.side_hover, self.menu_hover)
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Down { right } => self.press(ev.x, ev.y, right),
            MouseKind::Move => match self.thumb_grab {
                Some(grab) => {
                    let total = self.content_height();
                    self.scroll =
                        widgets::thumb_drag(track(), true, total, content().h, ev.y, grab);
                    self.clamp_scroll();
                    true
                }
                None => self.drag_band(ev.x, ev.y),
            },
            MouseKind::Up => {
                self.thumb_grab = None;
                self.band.take().is_some()
            }
        }
    }

    /// The right-click menu, on the chosen items or on the empty space.
    fn menu_entries(&self, on_item: bool) -> Vec<Entry> {
        const SEP: Entry = ("", "", None);
        let any = !self.items.is_empty();
        let maybe = |on: bool, cmd: Cmd| on.then_some(cmd);
        if self.at_bin() {
            return if on_item {
                vec![
                    ("Restore", "", Some(Cmd::Restore)),
                    SEP,
                    ("Delete", "Del", Some(Cmd::Delete)),
                ]
            } else {
                vec![
                    ("Empty Trash", "", maybe(any, Cmd::Empty)),
                    SEP,
                    ("Select all", "Ctrl+A", maybe(any, Cmd::SelectAll)),
                    ("Details", "", Some(Cmd::Details)),
                    ("Large icons", "", Some(Cmd::Icons)),
                    ("Refresh", "F5", Some(Cmd::Refresh)),
                ]
            };
        }
        if self.at_computer() {
            return if on_item {
                vec![("Open", "Enter", Some(Cmd::Open))]
            } else {
                vec![("Refresh", "F5", Some(Cmd::Refresh))]
            };
        }
        if on_item {
            let mut v = vec![("Open", "Enter", Some(Cmd::Open))];
            if self.chosen_picture().is_some() {
                v.push(("Set as desktop background", "", Some(Cmd::SetBackground)));
            }
            v.push(SEP);
            v.push(("Rename", "F2", maybe(self.single().is_some(), Cmd::Rename)));
            v.push(("Delete", "Del", Some(Cmd::Delete)));
            v
        } else {
            vec![
                ("New folder", "Ctrl+Shift+N", Some(Cmd::NewFolder)),
                ("New text document", "", Some(Cmd::NewFile)),
                SEP,
                ("Select all", "Ctrl+A", maybe(any, Cmd::SelectAll)),
                ("Details", "", Some(Cmd::Details)),
                ("Large icons", "", Some(Cmd::Icons)),
                ("Refresh", "F5", Some(Cmd::Refresh)),
            ]
        }
    }

    fn menu_items(&self, on_item: bool) -> Vec<Item<'static>> {
        self.menu_entries(on_item)
            .into_iter()
            .map(|(label, key, cmd)| (label, key, cmd.is_some()))
            .collect()
    }

    fn press(&mut self, x: i32, y: i32, right: bool) -> bool {
        if let Some(d) = &self.dialog {
            if right {
                return false;
            }
            let (lines, buttons) = self.dialog_text(d);
            let lines: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
            let r = widgets::message_rect(client(), &lines, buttons);
            let rects = widgets::message_buttons(r, buttons.len());
            match rects.iter().position(|b| b.contains(x, y)) {
                Some(0) => match self.dialog.take() {
                    Some(Dialog::Delete(items)) => self.delete(&items),
                    Some(Dialog::Empty) => self.empty_bin(),
                    _ => {}
                },
                Some(_) => self.dialog = None,
                None => return false,
            }
            return true;
        }
        if let Some((on_item, r)) = self.menu.take() {
            let items = self.menu_items(on_item);
            if let Some(i) = widgets::menu_item_at(r, &items, x, y) {
                if let Some(cmd) = self.menu_entries(on_item)[i].2 {
                    self.run(cmd);
                }
                return true;
            }
            if r.contains(x, y) {
                return true;
            }
        }
        // a click anywhere but in the rename box finishes renaming
        if let Some(i) = self.renaming {
            if !self.rename_rect(i).contains(x, y) {
                self.commit_rename();
            } else {
                self.rename.click(self.rename_rect(i), x);
                return true;
            }
        }
        if self.focus != Focus::List
            && !address_rect().contains(x, y)
            && !search_rect().contains(x, y)
        {
            self.focus = Focus::List;
        }
        if y < NAV_H {
            if right {
                return true;
            }
            match (0..4).find(|&i| nav_button(i).contains(x, y)) {
                Some(0) => self.go_back(),
                Some(1) => self.go_forward(),
                Some(2) => self.go_up(),
                Some(_) => self.refresh(),
                None => {}
            }
            if address_rect().contains(x, y) {
                if self.focus == Focus::Address {
                    self.address.click(address_rect(), x);
                } else if let Some((_, path, _)) =
                    self.crumbs().into_iter().find(|(_, _, r)| r.contains(x, y))
                {
                    self.navigate(&path);
                } else {
                    self.edit_address();
                }
            }
            if search_rect().contains(x, y) {
                self.focus = Focus::Search;
                self.search.click(search_rect(), x);
            }
            return true;
        }
        if y < TOP {
            if !right {
                if let Some((cmd, _, _)) = commands(self.at_bin())
                    .into_iter()
                    .find(|(_, _, r)| r.contains(x, y))
                {
                    self.run(cmd);
                }
            }
            return true;
        }
        if x < SIDE_W {
            let places = places();
            let n = places.len();
            if let Some(i) = (0..n).find(|&i| place_rect(i, n).contains(x, y)) {
                if !right {
                    let path = places[i].path.clone();
                    self.navigate(&path);
                }
            }
            return true;
        }
        if track().contains(x, y) && !right {
            let total = self.content_height();
            let t = widgets::thumb(track(), true, total, content().h, self.scroll);
            if t.contains(x, y) {
                self.thumb_grab = Some(y - t.y);
            } else {
                let page = content().h - ROW;
                self.scroll += if y < t.y { -page } else { page };
                self.clamp_scroll();
            }
            return true;
        }
        if !content().contains(x, y) {
            return false;
        }
        let hit = self.item_at(x, y);
        if right {
            // a right-click on a chosen item keeps the whole selection
            match hit {
                Some(i) if self.is_chosen(i) => self.selected = Some(i),
                _ => self.select_one(hit),
            }
            let items = self.menu_items(hit.is_some());
            let mut r = widgets::menu_rect(x, y, &items);
            r.x = r.x.min(CLIENT_W - r.w - 4);
            if r.bottom() > CLIENT_H - 4 {
                r.y = y - r.h;
            }
            self.menu = Some((hit.is_some(), r));
            self.menu_hover = None;
            return true;
        }
        let (ctrl, shift) = (keyboard::ctrl_held(), keyboard::shift_held());
        let Some(i) = hit else {
            // empty space: start the selection rectangle
            let at = (x, y + self.scroll);
            let before = if ctrl { self.chosen() } else { Vec::new() };
            if !ctrl {
                self.select_one(None);
            }
            self.band = Some(Band {
                from: at,
                to: at,
                before,
            });
            return true;
        };
        if ctrl {
            if self.is_chosen(i) {
                self.marked = self.chosen();
                self.marked.retain(|&k| k != i);
                self.selected = self.marked.last().copied();
            } else {
                self.marked = self.chosen();
                self.selected = Some(i);
            }
            self.anchor = Some(i);
            return true;
        }
        if shift {
            self.select_range(i);
            return true;
        }
        // a plain click on one of several chosen items keeps them for a
        // double-click; otherwise it chooses just this one
        if !self.is_chosen(i) || self.chosen().len() == 1 {
            self.select_one(Some(i));
        } else {
            self.selected = Some(i);
            self.anchor = Some(i);
        }
        {
            let now = interrupts::ticks();
            if self.last_click.1 == i && now - self.last_click.0 <= DOUBLE_CLICK {
                self.last_click = (0, usize::MAX);
                self.open(i);
            } else {
                self.last_click = (now, i);
            }
        }
        true
    }

    /// Stretch the selection rectangle to the mouse and choose what it
    /// touches, scrolling when the mouse goes past the top or bottom.
    fn drag_band(&mut self, x: i32, y: i32) -> bool {
        if self.band.is_none() {
            return false;
        }
        let area = content();
        let top = if self.view == View::Details {
            area.y + HEADER_H
        } else {
            area.y
        };
        if y < top {
            self.scroll -= ROW / 2;
        } else if y > area.bottom() {
            self.scroll += ROW / 2;
        }
        self.clamp_scroll();
        let x = x.clamp(area.x, area.right() - 1);
        let y = y.clamp(top, area.bottom() - 1) + self.scroll;
        let Some(band) = &mut self.band else {
            return false;
        };
        band.to = (x, y);
        let r = band.rect();
        let mut chosen = band.before.clone();
        for i in 0..self.items.len() {
            if !self.item_rect(i).intersect(&r).is_empty() && !chosen.contains(&i) {
                chosen.push(i);
            }
        }
        self.selected = chosen.last().copied();
        self.anchor = self.selected;
        self.marked = chosen;
        true
    }

    fn dialog_text(&self, d: &Dialog) -> (Vec<String>, &'static [&'static str]) {
        match d {
            Dialog::Delete(items) if items.len() > 1 => (
                vec![format!(
                    "Are you sure you want to permanently delete these {} items?",
                    items.len()
                )],
                &["Yes", "No"],
            ),
            Dialog::Delete(items) => {
                let item = &self.items[items[0]];
                let first = if item.dir {
                    "Are you sure you want to permanently delete this folder"
                } else {
                    "Are you sure you want to permanently delete this file?"
                };
                let mut name = String::from("\"");
                name.push_str(&item.name);
                name.push('"');
                let mut lines = vec![String::from(first)];
                if item.dir {
                    lines.push(String::from("and everything in it?"));
                }
                lines.push(name);
                (lines, &["Yes", "No"])
            }
            Dialog::Empty => {
                let n = self.items.len();
                let mut first = String::new();
                if n == 1 {
                    first.push_str("Are you sure you want to permanently delete this item?");
                } else {
                    let _ = write!(
                        first,
                        "Are you sure you want to permanently delete these {} items?",
                        n
                    );
                }
                (vec![first], &["Yes", "No"])
            }
            Dialog::Message(m) => (vec![String::from(*m)], &["OK"]),
        }
    }

    fn rename_rect(&self, i: usize) -> Rect {
        let r = self.item_rect(i).offset(0, -self.scroll);
        match self.view {
            View::Details => Rect::new(r.x + 30, r.y + 2, 360, r.h - 4),
            View::Icons => Rect::new(r.x - 2, r.y + 64, r.w + 4, 28),
        }
    }

    // ---- drawing ---------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, caret: bool) {
        c.fill(client(), theme::light());
        self.draw_nav(c, caret);
        self.draw_commands(c);
        self.draw_side(c);
        self.draw_files(c, caret);
        self.draw_status(c);
        if let Some((on_item, r)) = self.menu {
            let items = self.menu_items(on_item);
            widgets::draw_menu(c, r, &items, self.menu_hover);
        }
        if let Some(d) = &self.dialog {
            let (lines, buttons) = self.dialog_text(d);
            let lines: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
            let title = match d {
                Dialog::Delete(_) => "Delete",
                Dialog::Empty => "Empty Trash",
                Dialog::Message(_) => "Files",
            };
            widgets::draw_message(c, client(), title, &lines, buttons, None);
        }
    }

    fn draw_nav(&mut self, c: &mut Canvas, caret: bool) {
        c.fill(Rect::new(0, 0, CLIENT_W, NAV_H), theme::face());
        let enabled = [
            !self.back.is_empty(),
            !self.forward.is_empty(),
            !self.at_computer(),
            true,
        ];
        for (i, on) in enabled.into_iter().enumerate() {
            let r = nav_button(i as i32);
            let color = if on {
                theme::text()
            } else {
                mix(theme::text_dim(), theme::face(), 120)
            };
            let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
            match i {
                0 | 1 => {
                    let d = if i == 0 { -1 } else { 1 };
                    c.line(cx - 7, cy, cx + 7, cy, color);
                    c.line(cx + 7 * d, cy, cx + 2 * d, cy - 5, color);
                    c.line(cx + 7 * d, cy, cx + 2 * d, cy + 5, color);
                }
                2 => {
                    c.line(cx, cy - 7, cx, cy + 7, color);
                    c.line(cx, cy - 7, cx - 5, cy - 2, color);
                    c.line(cx, cy - 7, cx + 5, cy - 2, color);
                }
                _ => {
                    // a circle with an arrowhead
                    c.outline_round(Rect::new(cx - 7, cy - 7, 15, 15), 7, color);
                    c.fill(Rect::new(cx + 2, cy - 9, 7, 7), theme::face());
                    c.line(cx + 1, cy - 7, cx + 6, cy - 7, color);
                    c.line(cx + 6, cy - 7, cx + 6, cy - 2, color);
                }
            }
        }
        let a = address_rect();
        if self.focus == Focus::Address {
            self.address.draw(c, a, true, caret);
        } else {
            c.fill_round(a, 4, theme::light());
            c.outline_round(a, 4, theme::stroke());
            let crumbs = self.crumbs();
            let icon_y = a.y + 8;
            if self.at_bin() {
                icons::get().draw_pic(c, Pic::BinEmpty, SMALL, a.x + 8, icon_y);
            } else if self.at_computer() {
                icons::get().draw_pic(c, Pic::Computer, SMALL, a.x + 8, icon_y);
            } else if crumbs.len() > 2 {
                widgets::folder_icon(c, a.x + 8, icon_y, 16);
            } else {
                drive_icon(c, a.x + 8, icon_y);
            }
            for (k, (label, _, r)) in crumbs.iter().enumerate() {
                c.draw_text(r.x + 6, a.y + 7, label, theme::text());
                if k + 1 < crumbs.len() {
                    let (sx, sy) = (r.right() + 8, a.y + 12);
                    c.line(sx, sy, sx + 4, sy + 4, theme::text_dim());
                    c.line(sx + 4, sy + 4, sx, sy + 8, theme::text_dim());
                }
            }
        }
        let s = search_rect();
        self.search.draw(c, s, self.focus == Focus::Search, caret);
        if self.search.text.is_empty() && self.focus != Focus::Search {
            let mut label = String::from("Search ");
            label.push_str(&self.place_name());
            let label = fit(&label, s.w - 40);
            c.draw_text(s.x + 10, s.y + 7, &label, theme::text_dim());
        }
        // magnifying glass
        let (gx, gy) = (s.right() - 26, s.y + 9);
        c.outline_round(Rect::new(gx, gy, 11, 11), 5, theme::text_dim());
        c.line(gx + 9, gy + 9, gx + 13, gy + 13, theme::text_dim());
    }

    fn draw_commands(&self, c: &mut Canvas) {
        let bar = Rect::new(0, NAV_H, CLIENT_W, CMD_H);
        c.fill(bar, theme::face());
        c.fill_rect(0, TOP - 1, CLIENT_W, 1, theme::stroke());
        let has_sel = !self.chosen().is_empty();
        let bin = self.at_bin();
        // discs can only be read, and Computer only lists drives
        let disc = self.at_computer() || fs::is_read_only(&self.path);
        for (cmd, label, r) in commands(bin) {
            let enabled = match cmd {
                Cmd::NewFolder | Cmd::NewFile => !disc,
                Cmd::Rename => self.single().is_some() && !disc,
                Cmd::Delete => has_sel && !disc,
                Cmd::Details | Cmd::Icons => !self.at_computer(),
                Cmd::Restore => has_sel && bin,
                Cmd::Empty => !self.items.is_empty(),
                _ => true,
            };
            let on = (cmd == Cmd::Details && self.view == View::Details)
                || (cmd == Cmd::Icons && self.view == View::Icons);
            if on {
                c.fill_round(r, 4, theme::accent_light());
            }
            let color = if enabled {
                theme::text()
            } else {
                mix(theme::text_dim(), theme::face(), 100)
            };
            let (ix, iy) = (r.x + 10, r.y + 8);
            match cmd {
                Cmd::NewFolder => widgets::folder_icon(c, ix, iy, 16),
                Cmd::NewFile => widgets::file_icon(c, ix, iy, 16),
                Cmd::Rename => {
                    // a text cursor in a box
                    c.outline_round(Rect::new(ix, iy + 3, 16, 10), 2, color);
                    c.fill_rect(ix + 11, iy, 1, 16, color);
                }
                Cmd::Delete => {
                    let red = if enabled { theme::error() } else { color };
                    c.fill_rect(ix + 1, iy + 2, 14, 2, red);
                    c.fill_rect(ix + 5, iy, 6, 2, red);
                    c.outline_round(Rect::new(ix + 3, iy + 4, 10, 12), 2, red);
                }
                Cmd::Details => {
                    for k in 0..4 {
                        c.fill_rect(ix, iy + 2 + k * 4, 16, 2, color);
                    }
                }
                Cmd::Empty => icons::get().draw_pic(c, Pic::BinEmpty, SMALL, ix, iy),
                Cmd::Restore | Cmd::Refresh => {
                    // an arrow going back up
                    let color = if cmd == Cmd::Restore && enabled {
                        theme::accent()
                    } else {
                        color
                    };
                    c.line(ix + 3, iy + 6, ix + 13, iy + 6, color);
                    c.line(ix + 3, iy + 6, ix + 7, iy + 2, color);
                    c.line(ix + 3, iy + 6, ix + 7, iy + 10, color);
                    c.line(ix + 13, iy + 6, ix + 13, iy + 14, color);
                }
                _ => {
                    for k in 0..4 {
                        let (dx, dy) = (k % 2 * 9, k / 2 * 9);
                        c.fill_round(Rect::new(ix + dx, iy + dy, 7, 7), 1, color);
                    }
                }
            }
            c.draw_text(r.x + 34, r.y + 7, label, color);
        }
        // a separator between creating and changing
        let r = commands(bin)[1].2;
        c.fill_rect(r.right() + 10, bar.y + 12, 1, 20, theme::stroke());
    }

    fn draw_side(&self, c: &mut Canvas) {
        let side = Rect::new(0, TOP, SIDE_W, CLIENT_H - TOP - STATUS_H);
        c.fill(side, theme::raised());
        c.fill_rect(SIDE_W, TOP, 1, side.h, theme::stroke());
        let places = places();
        let n = places.len();
        for (i, p) in places.iter().enumerate() {
            let r = place_rect(i, n);
            if fs::same_name(&p.path, &self.path) {
                c.fill_round(r, 4, theme::accent_light());
                c.fill_round(Rect::new(r.x, r.y + 8, 3, r.h - 16), 1, theme::accent());
            } else if self.side_hover == Some(i) {
                c.fill_round(r, 4, theme::hover());
            }
            match p.kind {
                PlaceKind::Computer => icons::get().draw_pic(c, Pic::Computer, SMALL, r.x + 12, r.y + 7),
                PlaceKind::Drive => drive_icon(c, r.x + 12, r.y + 8),
                PlaceKind::Disc => disc_icon(c, r.x + 12, r.y + 7),
                PlaceKind::Home => home_icon(c, r.x + 12, r.y + 7),
                PlaceKind::Library => widgets::folder_icon(c, r.x + 12, r.y + 7, 16),
                PlaceKind::Bin => {
                    let pic = if recycle::is_empty(&user()) {
                        Pic::BinEmpty
                    } else {
                        Pic::BinFull
                    };
                    icons::get().draw_pic(c, pic, SMALL, r.x + 12, r.y + 7);
                }
            }
            let label = fit(&p.label, r.w - 44);
            c.draw_text(r.x + 38, r.y + 7, &label, theme::text());
        }
        // "Devices" above Computer and the drives
        let first = place_rect(fs::LIBRARIES.len() + 1, n);
        c.fill_rect(12, first.y - 42, SIDE_W - 24, 1, theme::stroke());
        c.draw_text(14, first.y - 30, "Devices", theme::text_dim());
        let bin = place_rect(n - 1, n);
        c.fill_rect(12, bin.y - 8, SIDE_W - 24, 1, theme::stroke());
    }

    fn draw_files(&mut self, c: &mut Canvas, caret: bool) {
        if self.at_computer() {
            self.draw_drives(c);
            return;
        }
        let area = content();
        let mut f = c.sub(Rect::new(0, 0, c.width, c.height));
        f.clip_to(area);
        let scroll = self.scroll;
        if self.view == View::Details {
            // the column titles stay put
            let h = Rect::new(area.x, area.y, area.w, HEADER_H);
            for (k, (x, title)) in COLUMNS.into_iter().enumerate() {
                if k > 0 {
                    f.fill_rect(area.x + x - 8, h.y + 6, 1, HEADER_H - 12, theme::stroke());
                }
                f.draw_text(area.x + x + 6, h.y + 7, title, theme::text_dim());
            }
            f.fill_rect(area.x + 6, h.bottom() - 1, area.w - 12, 1, theme::stroke());
            f.clip_to(Rect::new(
                area.x,
                area.y + HEADER_H,
                area.w,
                area.h - HEADER_H,
            ));
        }
        for i in 0..self.items.len() {
            let r = self.item_rect(i).offset(0, -scroll);
            if r.bottom() < area.y || r.y > area.bottom() {
                continue;
            }
            let item = &self.items[i];
            if self.is_chosen(i) {
                f.fill_round(r, 4, theme::selection());
                if self.selected == Some(i) && self.chosen().len() > 1 {
                    f.outline_round(r, 4, theme::selection_edge());
                }
            } else if self.hover == Some(i) {
                f.fill_round(r, 4, theme::row_hover());
            }
            let renaming = self.renaming == Some(i);
            match self.view {
                View::Details => {
                    if item.dir {
                        widgets::folder_icon(&mut f, r.x + 8, r.y + 6, 16);
                    } else {
                        widgets::file_icon(&mut f, r.x + 8, r.y + 6, 16);
                    }
                    let ty = r.y + (ROW - UI.line_height) / 2;
                    let base = area.x;
                    if !renaming {
                        let name = fit(&item.name, COLUMNS[1].0 - COLUMNS[0].0 - 40);
                        f.draw_text(r.x + 36, ty, &name, theme::text());
                    }
                    let (y, mo, d, h, mi) = item.modified;
                    let mut s = String::new();
                    let _ = write!(s, "{:02}.{:02}.{} {:02}:{:02}", d, mo, y, h, mi);
                    let dim = mix(theme::text_dim(), theme::text(), 80);
                    f.draw_text(base + COLUMNS[1].0 + 6, ty, &s, dim);
                    f.draw_text(base + COLUMNS[2].0 + 6, ty, &type_name(item), dim);
                    if !item.dir {
                        let s = size_text(item.size);
                        let right = base + COLUMNS[3].0 + 100;
                        f.draw_text(right - UI.width(&s), ty, &s, dim);
                    }
                }
                View::Icons => {
                    let ix = r.x + (r.w - 48) / 2;
                    if item.dir {
                        widgets::folder_icon(&mut f, ix, r.y + 10, 48);
                    } else {
                        widgets::file_icon(&mut f, ix, r.y + 8, 48);
                    }
                    if !renaming {
                        let name = fit(&item.name, r.w - 8);
                        f.text_centered(Rect::new(r.x, r.y + 68, r.w, 20), &name, theme::text());
                    }
                }
            }
        }
        if self.items.is_empty() {
            let msg = if self.search.text.is_empty() {
                "This folder is empty."
            } else {
                "No items match your search."
            };
            let top = if self.view == View::Details {
                HEADER_H
            } else {
                0
            };
            let r = Rect::new(area.x, area.y + top + 30, area.w, 20);
            f.text_centered(r, msg, theme::text_dim());
        }
        if let Some(band) = &self.band {
            let r = band.rect().offset(0, -scroll);
            f.fill_round_alpha(r, 0, theme::accent_base(), 60);
            f.outline_round(r, 0, theme::selection_edge());
        }
        if let Some(i) = self.renaming {
            let r = self.rename_rect(i);
            let mut rc = c.sub(Rect::new(0, 0, c.width, c.height));
            rc.clip_to(area);
            self.rename.draw(&mut rc, r, true, caret);
        }
        let total = self.content_height();
        widgets::draw_scrollbar(c, track(), true, total, area.h, self.scroll);
    }

    /// Computer: a tile for every disk and drive, with how full it is.
    fn draw_drives(&self, c: &mut Canvas) {
        let area = content();
        let mut f = c.sub(Rect::new(0, 0, c.width, c.height));
        f.clip_to(area);
        let scroll = self.scroll;
        f.draw_text(area.x + 18, area.y + 14 - scroll, "Disks and drives", theme::text_dim());
        for (i, d) in self.drives.iter().enumerate() {
            let r = self.item_rect(i).offset(0, -scroll);
            if r.bottom() < area.y || r.y > area.bottom() {
                continue;
            }
            if self.is_chosen(i) {
                f.fill_round(r, 6, theme::selection());
            } else if self.hover == Some(i) {
                f.fill_round(r, 6, theme::row_hover());
            }
            f.outline_round(r, 6, theme::stroke());
            // a big icon
            let (ix, iy) = (r.x + 14, r.y + 20);
            if d.kind == fs::DriveKind::Cd {
                big_disc_icon(&mut f, ix, iy, d.ready);
            } else {
                big_disk_icon(&mut f, ix, iy, d.kind == fs::DriveKind::System);
            }
            let tx = r.x + 84;
            let tw = r.w - 96;
            f.draw_text(tx, r.y + 8, &fit(&d.name, tw), theme::text());
            let dim = mix(theme::text_dim(), theme::text(), 60);
            match d.free {
                Some(free) if d.bytes > 0 => {
                    let bar = Rect::new(tx, r.y + 34, tw, 10);
                    f.fill_round(bar, 3, mix(theme::stroke(), theme::light(), 140));
                    let used = d.bytes.saturating_sub(free);
                    let w = (bar.w as u64 * used / d.bytes) as i32;
                    let full = used * 10 > d.bytes * 9;
                    let color = if full { theme::error() } else { theme::accent() };
                    f.fill_round(Rect::new(bar.x, bar.y, w.max(3), bar.h), 3, color);
                    let s = format!("{} free of {}", big_size(free), big_size(d.bytes));
                    f.draw_text(tx, r.y + 48, &s, dim);
                }
                _ => {
                    let mut s = d.status.clone();
                    if d.bytes > 0 {
                        let _ = write!(s, ", {}", big_size(d.bytes));
                    }
                    let color = if d.ready || d.kind == fs::DriveKind::Cd {
                        dim
                    } else {
                        theme::warning()
                    };
                    f.draw_text(tx, r.y + 36, &fit(&s, tw), color);
                }
            }
            let detail = fit(&d.detail, tw);
            f.draw_text(tx, r.y + 68, &detail, theme::text_dim());
        }
        let total = self.content_height();
        widgets::draw_scrollbar(c, track(), true, total, area.h, self.scroll);
    }

    fn draw_status(&self, c: &mut Canvas) {
        let r = Rect::new(0, CLIENT_H - STATUS_H, CLIENT_W, STATUS_H);
        c.fill(r, theme::face());
        c.fill_rect(0, r.y, CLIENT_W, 1, theme::stroke());
        let ty = r.y + (STATUS_H - UI.line_height) / 2;
        let mut s = String::new();
        let n = self.items.len();
        if self.at_computer() {
            let _ = write!(s, "{} disk{} and drive{}", n, if n == 1 { "" } else { "s" }, if n == 1 { "" } else { "s" });
            c.draw_text(12, ty, &s, theme::text());
            return;
        }
        let _ = write!(s, "{} item{}", n, if n == 1 { "" } else { "s" });
        let chosen = self.chosen();
        if chosen.len() > 1 {
            let _ = write!(s, "      {} items selected", chosen.len());
            let bytes: u64 = chosen
                .iter()
                .filter(|&&i| !self.items[i].dir)
                .map(|&i| self.items[i].size as u64)
                .sum();
            if bytes > 0 {
                let _ = write!(s, "  {}", size_text(bytes.min(u32::MAX as u64) as u32));
            }
        } else if let Some(i) = self.single() {
            let _ = write!(s, "      1 item selected");
            if !self.items[i].dir {
                let _ = write!(s, "  {}", size_text(self.items[i].size));
            }
            if self.at_bin() {
                if let Some(from) = recycle::original(&user(), &self.items[i].name) {
                    let _ = write!(s, "      deleted from {}", fs::display(&fs::parent(&from)));
                }
            }
        }
        c.draw_text(12, ty, &s, theme::text());
        s.clear();
        // on another disk or a disc: say which
        let other = fs::drives().into_iter().find(|d| {
            d.kind != fs::DriveKind::System
                && (fs::same_name(&d.path, &self.path)
                    || self.path.len() > d.path.len()
                        && fs::same_name(&self.path[..d.path.len()], &d.path)
                        && self.path.as_bytes()[d.path.len()] == b'/')
        });
        if let Some(d) = other {
            let _ = write!(s, "{}  {}  {}", d.name, d.status, big_size(d.bytes));
            let w = UI.width(&s);
            c.draw_text(r.right() - 14 - w, ty, &s, theme::text_dim());
            return;
        }
        match fs::storage() {
            fs::Storage::Disk => {
                let _ = write!(
                    s,
                    "System Disk  FAT32  {} MB, saved on the disk",
                    fs::capacity() / (1024 * 1024)
                );
                let w = UI.width(&s);
                c.draw_text(r.right() - 14 - w, ty, &s, theme::text_dim());
            }
            _ => {
                let s = "No disk: files are kept in memory until restart";
                let w = UI.width(s);
                c.draw_text(r.right() - 14 - w, ty, s, theme::warning());
            }
        }
        let _ = self.selected_path();
    }
}

/// The disk, 16 pixels square.
/// A CD: a silver ring with a hole, 16 pixels.
fn disc_icon(c: &mut Canvas, x: i32, y: i32) {
    c.fill_round(Rect::new(x, y, 16, 16), 8, rgb(0xb8, 0xc4, 0xd4));
    c.fill_round(Rect::new(x + 3, y + 3, 7, 7), 3, rgb(0xe8, 0xf0, 0xff));
    c.outline_round(Rect::new(x, y, 16, 16), 8, rgb(0x70, 0x7c, 0x90));
    c.fill_round(Rect::new(x + 6, y + 6, 4, 4), 2, theme::raised());
}

/// A hard disk, 48 by 40 pixels: a case with a light. The system disk
/// has the logo color.
fn big_disk_icon(c: &mut Canvas, x: i32, y: i32, system: bool) {
    let body = if system { theme::accent() } else { rgb(0x6b, 0x77, 0x8a) };
    c.fill_round(Rect::new(x, y + 8, 52, 30), 6, body);
    c.fill_round(Rect::new(x + 2, y + 10, 48, 16), 5, mix(body, rgb(0xff, 0xff, 0xff), 60));
    c.fill_round(Rect::new(x + 40, y + 29, 6, 5), 2, rgb(0x5c, 0xe0, 0x8a));
    for k in 0..4 {
        c.fill_rect(x + 8 + k * 6, y + 30, 3, 4, mix(body, rgb(0, 0, 0), 60));
    }
}

/// A CD, 48 pixels: silver when a disc is in, a gray outline when not.
fn big_disc_icon(c: &mut Canvas, x: i32, y: i32, disc: bool) {
    let r = Rect::new(x + 2, y, 46, 46);
    if disc {
        c.fill_round(r, 23, rgb(0xc4, 0xcf, 0xdd));
        c.fill_round(Rect::new(x + 10, y + 8, 18, 18), 9, rgb(0xee, 0xf4, 0xff));
        c.outline_round(r, 23, rgb(0x70, 0x7c, 0x90));
    } else {
        c.outline_round(r, 23, rgb(0x9a, 0xa4, 0xb4));
        c.outline_round(Rect::new(x + 6, y + 4, 38, 38), 19, rgb(0xc8, 0xcf, 0xda));
    }
    c.fill_round(Rect::new(x + 19, y + 17, 12, 12), 6, theme::light());
    c.outline_round(Rect::new(x + 19, y + 17, 12, 12), 6, rgb(0x90, 0x9a, 0xaa));
}

fn drive_icon(c: &mut Canvas, x: i32, y: i32) {
    icons::get().draw_pic(c, Pic::Drives, SMALL, x, y);
}

/// A small house for Home.
fn home_icon(c: &mut Canvas, x: i32, y: i32) {
    let color = theme::accent();
    c.fill_polygon(&[(x, y + 8), (x + 8, y), (x + 16, y + 8)], color);
    c.fill_round(Rect::new(x + 2, y + 7, 12, 9), 1, color);
    c.fill(Rect::new(x + 6, y + 10, 4, 6), theme::raised());
}
