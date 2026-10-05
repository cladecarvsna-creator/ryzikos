//! Archiver: makes, opens, changes and unpacks ZIP files, and opens and
//! unpacks .tar, .tar.gz and .gz (see crate::archive).
//!
//! The window has two sides: your files on the left and the archive on
//! the right, with Add and Extract between them. Files and folders go
//! from the left into the archive's folder that is shown; what is chosen
//! in the archive comes out into the folder shown on the left. Rows have
//! a tick box, so several can be chosen with the mouse alone; Ctrl and
//! Shift work too. The right side shows how much each file was packed.
//!
//! Packing and unpacking run in a fiber, with a progress card and Stop.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::filedialog::{self, FileDialog, Mode};
use super::text::{TITLE, UI, UI_BOLD};
use super::{theme, widgets, App, MouseEvent, MouseKind};
use crate::archive::{self, Archive, Level, Source, Stamp};
use crate::fiber::Fiber;
use crate::fs::{self, Info};
use crate::keyboard::{self, Key};
use crate::{interrupts, serial, users};

pub const CLIENT_W: i32 = 1040;
pub const CLIENT_H: i32 = 640;

fn cw() -> i32 {
    super::client_w(App::Archiver)
}
fn ch() -> i32 {
    super::client_h(App::Archiver)
}
fn client() -> Rect {
    Rect::new(0, 0, cw(), ch())
}

const HEAD_H: i32 = 64;
const FOOT_H: i32 = 52;
const MARGIN: i32 = 12;
const GUTTER: i32 = 76;
const PANE_HEAD: i32 = 44;
const COLS_H: i32 = 26;
const ROW: i32 = 30;
const DOUBLE_CLICK: u64 = interrupts::TIMER_HZ / 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Files,
    Archive,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    New,
    Open,
    ExtractAll,
    Add,
    Extract,
    Up(Side),
    Level(usize),
    Delete,
    Stop,
    Hint(usize),
}

/// A row on the archive side: a file, or a folder with what is in it.
struct Row {
    name: String,
    /// Its whole path in the archive.
    full: String,
    dir: bool,
    size: u64,
    packed: u64,
    files: usize,
    modified: Stamp,
}

enum Dialog {
    /// Make a new ZIP file: where and under what name.
    New(FileDialog),
    Open(FileDialog),
    /// Remove these from the archive?
    Delete(Vec<String>),
    Message(&'static str, Vec<String>),
}

enum Work {
    /// Writing the ZIP file at this path.
    Pack(String),
    /// Unpacking into this folder; go there on the left afterwards.
    Unpack(String, bool),
}

#[derive(Default)]
struct Progress {
    done: u64,
    total: u64,
    name: String,
    result: Option<Result<usize, String>>,
}

struct Job {
    fiber: Fiber,
    work: Work,
    state: Rc<RefCell<Progress>>,
    stopping: bool,
    /// What the progress card showed last.
    shown: (i32, u64),
}

/// One side's list: which rows are chosen and how far it is scrolled.
#[derive(Default)]
struct List {
    chosen: Vec<bool>,
    /// Where Shift+click chooses from.
    anchor: usize,
    scroll: i32,
}

impl List {
    fn reset(&mut self, n: usize) {
        self.chosen = vec![false; n];
        self.anchor = 0;
        self.scroll = 0;
    }

    fn picked(&self) -> Vec<usize> {
        (0..self.chosen.len()).filter(|&i| self.chosen[i]).collect()
    }

    fn click(&mut self, i: usize, tick_box: bool) {
        if keyboard::shift_held() {
            let (a, b) = (self.anchor.min(i), self.anchor.max(i));
            for k in 0..self.chosen.len() {
                self.chosen[k] = (a..=b).contains(&k);
            }
            return;
        }
        if tick_box || keyboard::ctrl_held() {
            self.chosen[i] = !self.chosen[i];
        } else {
            for c in self.chosen.iter_mut() {
                *c = false;
            }
            self.chosen[i] = true;
        }
        self.anchor = i;
    }
}

pub struct Archiver {
    /// The folder on the left and what is in it.
    dir: String,
    files: Vec<Info>,
    left: List,
    /// The open archive, its file and the folder in it on the right.
    path: Option<String>,
    archive: Option<Rc<Archive>>,
    inside: String,
    rows: Vec<Row>,
    right: List,
    side: Side,
    level: Level,
    job: Option<Job>,
    dialog: Option<Dialog>,
    pressed: Option<Button>,
    hover: Option<(Side, usize)>,
    /// Where the mouse is, so the wheel scrolls the side under it.
    mouse_x: i32,
    last_click: (u64, Side, usize),
    /// The last thing done, for the bottom bar.
    status: String,
    changes: u32,
    /// A file taken out of the archive for the desktop to open.
    pub open_request: Option<String>,
}

fn home() -> String {
    fs::home(users::current_name().unwrap_or_default().as_str())
}

// ---- layout ------------------------------------------------------------------

fn pane_w() -> i32 {
    (cw() - 2 * MARGIN - GUTTER) / 2
}

fn pane(side: Side) -> Rect {
    let x = match side {
        Side::Files => MARGIN,
        Side::Archive => cw() - MARGIN - pane_w(),
    };
    Rect::new(x, HEAD_H, pane_w(), ch() - HEAD_H - FOOT_H - 4)
}

/// Where a side's rows are drawn.
fn list_rect(side: Side) -> Rect {
    let p = pane(side);
    Rect::new(
        p.x + 1,
        p.y + PANE_HEAD + COLS_H,
        p.w - 2,
        p.h - PANE_HEAD - COLS_H - 1,
    )
}

fn row_rect(side: Side, i: usize, scroll: i32) -> Rect {
    let l = list_rect(side);
    Rect::new(l.x + 4, l.y + 2 + i as i32 * ROW - scroll, l.w - 8, ROW)
}

fn button_rect(b: Button) -> Rect {
    let mid = MARGIN + pane_w() + GUTTER / 2;
    let body = pane(Side::Files);
    let cy = body.y + body.h / 2;
    match b {
        Button::ExtractAll => Rect::new(cw() - 16 - 128, 16, 128, 32),
        Button::Open => Rect::new(cw() - 16 - 128 - 8 - 96, 16, 96, 32),
        Button::New => Rect::new(cw() - 16 - 128 - 8 - 96 - 8 - 104, 16, 104, 32),
        Button::Add => Rect::new(mid - 30, cy - 64, 60, 56),
        Button::Extract => Rect::new(mid - 30, cy + 8, 60, 56),
        Button::Up(side) => {
            let p = pane(side);
            Rect::new(p.x + 8, p.y + 8, 30, 28)
        }
        Button::Level(i) => Rect::new(MARGIN + 74 + i as i32 * 76, ch() - FOOT_H + 10, 72, 30),
        Button::Delete => Rect::new(cw() - MARGIN - 100, ch() - FOOT_H + 10, 100, 30),
        Button::Stop => {
            let card = progress_card();
            Rect::new(card.right() - 20 - 100, card.bottom() - 48, 100, 32)
        }
        Button::Hint(i) => {
            let p = pane(Side::Archive);
            let cy = p.y + p.h / 2 + 44;
            Rect::new(p.x + p.w / 2 - 128 + i as i32 * 136, cy, 120, 32)
        }
    }
}

fn progress_card() -> Rect {
    let (w, h) = (460, 170);
    Rect::new((cw() - w) / 2, (ch() - h) / 2, w, h)
}

// ---- text --------------------------------------------------------------------

pub fn size_text(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{}.{} KB", bytes / 1024, bytes % 1024 * 10 / 1024)
    } else {
        format!("{}.{} MB", bytes >> 20, (bytes % (1 << 20)) * 10 >> 20)
    }
}

fn percent(done: u64, total: u64) -> i32 {
    if total == 0 {
        0
    } else {
        (done * 100 / total).min(100) as i32
    }
}

/// How much smaller packing made it, in percent.
fn saved(size: u64, packed: u64) -> u32 {
    if size == 0 || packed >= size {
        0
    } else {
        ((size - packed) * 100 / size) as u32
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", n, if n == 1 { one } else { many })
}

/// Break text into lines that fit `w` pixels.
fn wrap(text: &str, w: i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if !line.is_empty() && UI.width(&line) + UI.width(" ") + UI.width(word) > w {
            lines.push(core::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// A file's name without an archive's extension: "photos" for
/// "photos.tar.gz".
fn base_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for ext in [".tar.gz", ".tgz", ".zip", ".tar", ".gz"] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return String::from(&name[..name.len() - ext.len()]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => String::from(&name[..i]),
        _ => String::from(name),
    }
}

/// A folder's contents; the top also has the discs and other disks.
fn list(dir: &str) -> Result<Vec<Info>, fs::Error> {
    let mut items = fs::list(dir)?;
    if dir == "/" {
        for d in fs::drives().into_iter().filter(|d| d.ready) {
            let name = d.path.trim_start_matches('/');
            if !name.is_empty() && !name.contains('/') && !items.iter().any(|i| fs::same_name(&i.name, name)) {
                items.push(Info { name: String::from(name), dir: true, size: 0, modified: (1980, 1, 1, 0, 0) });
            }
        }
    }
    Ok(items)
}

pub fn is_archive(path: &str) -> bool {
    Archive::kind_of(path).is_some()
}

// ---- the app -----------------------------------------------------------------

impl Archiver {
    pub fn new() -> Self {
        Self {
            dir: String::new(),
            files: Vec::new(),
            left: List::default(),
            path: None,
            archive: None,
            inside: String::new(),
            rows: Vec::new(),
            right: List::default(),
            side: Side::Files,
            level: Level::Normal,
            job: None,
            dialog: None,
            pressed: None,
            hover: None,
            mouse_x: 0,
            last_click: (0, Side::Files, usize::MAX),
            status: String::new(),
            changes: 0,
            open_request: None,
        }
    }

    /// The window opened: show the home folder the first time.
    pub fn start(&mut self) {
        if self.dir.is_empty() {
            let dir = home();
            self.go(&dir);
        } else {
            self.reload_files();
        }
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => format!("{} - Archiver", fs::file_name(p)),
            None => String::from("Archiver"),
        }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// A name is being typed (the New ZIP and Open boxes), so the caret
    /// blinks.
    pub fn typing(&self) -> bool {
        matches!(self.dialog, Some(Dialog::New(_) | Dialog::Open(_)))
    }

    pub fn resized(&mut self) {
        self.clamp_scroll(Side::Files);
        self.clamp_scroll(Side::Archive);
    }

    /// Read the left folder again when files changed.
    pub fn check_changes(&mut self) -> bool {
        let now = fs::changes();
        if now == self.changes || self.job.is_some() {
            return false;
        }
        self.reload_files();
        true
    }

    // ---- the left side: files ------------------------------------------------

    fn go(&mut self, dir: &str) {
        match list(dir) {
            Ok(items) => {
                self.status.clear();
                self.dir = String::from(dir);
                self.files = items;
                self.left.reset(self.files.len());
                self.changes = fs::changes();
            }
            Err(e) => self.message("Can't open the folder", e.message()),
        }
    }

    /// Read the folder again, keeping what was chosen.
    fn reload_files(&mut self) {
        let chosen: Vec<String> = self
            .left
            .picked()
            .into_iter()
            .map(|i| self.files[i].name.clone())
            .collect();
        let scroll = self.left.scroll;
        let dir = self.dir.clone();
        self.changes = fs::changes();
        match list(&dir) {
            Ok(items) => {
                self.files = items;
                self.left.reset(self.files.len());
                self.left.scroll = scroll;
                for (i, f) in self.files.iter().enumerate() {
                    self.left.chosen[i] = chosen.iter().any(|n| fs::same_name(n, &f.name));
                }
            }
            // the folder is gone: go home
            Err(_) => {
                let h = home();
                self.go(&h);
            }
        }
        self.clamp_scroll(Side::Files);
    }

    fn up(&mut self, side: Side) {
        match side {
            Side::Files => {
                if self.dir != "/" {
                    let parent = fs::parent(&self.dir);
                    let from = String::from(fs::file_name(&self.dir));
                    self.go(&parent);
                    self.pick_name(Side::Files, &from);
                }
            }
            Side::Archive => {
                if !self.inside.is_empty() {
                    let from = match self.inside.rfind('/') {
                        Some(i) => String::from(&self.inside[i + 1..]),
                        None => self.inside.clone(),
                    };
                    self.inside = match self.inside.rfind('/') {
                        Some(i) => String::from(&self.inside[..i]),
                        None => String::new(),
                    };
                    self.build_rows();
                    self.pick_name(Side::Archive, &from);
                }
            }
        }
    }

    /// Choose the row with this name and scroll to it.
    fn pick_name(&mut self, side: Side, name: &str) {
        let i = match side {
            Side::Files => self.files.iter().position(|f| fs::same_name(&f.name, name)),
            Side::Archive => self.rows.iter().position(|r| fs::same_name(&r.name, name)),
        };
        if let Some(i) = i {
            let list = self.list_mut(side);
            list.chosen[i] = true;
            list.anchor = i;
            self.scroll_to(side, i);
        }
    }

    fn scroll_to(&mut self, side: Side, i: usize) {
        let h = list_rect(side).h;
        let list = self.list_mut(side);
        let top = i as i32 * ROW;
        if top < list.scroll {
            list.scroll = top;
        } else if top + ROW + 4 > list.scroll + h {
            list.scroll = top + ROW + 4 - h;
        }
    }

    // ---- the right side: the archive -------------------------------------------

    /// Open an archive file on the right, and its folder on the left.
    pub fn open_file(&mut self, path: &str) {
        if self.job.is_some() {
            return;
        }
        match fs::read(path)
            .map_err(|e| String::from(e.message()))
            .and_then(|d| Archive::parse(d, path))
        {
            Ok(a) => {
                serial::write_str(&format!(
                    "\narchiver: opened {}, {} {}\n",
                    fs::file_name(path),
                    a.entries.len(),
                    if a.entries.len() == 1 {
                        "entry"
                    } else {
                        "entries"
                    }
                ));
                self.show(path, a);
                let dir = fs::parent(path);
                if !fs::same_name(&dir, &self.dir) {
                    self.go(&dir);
                }
                self.pick_name(Side::Files, fs::file_name(path));
                self.side = Side::Archive;
            }
            Err(e) => self.message("Can't open the archive", &e),
        }
    }

    fn show(&mut self, path: &str, a: Archive) {
        let same = self.path.as_deref().is_some_and(|p| fs::same_name(p, path));
        self.path = Some(String::from(path));
        self.archive = Some(Rc::new(a));
        if !same {
            self.inside.clear();
        }
        let scroll = self.right.scroll;
        self.build_rows();
        if same {
            self.right.scroll = scroll;
            self.clamp_scroll(Side::Archive);
        }
    }

    /// The rows of the archive folder being shown: files, and folders with
    /// the sizes of everything in them. Folders an archive only has
    /// implied by its paths are shown too.
    fn build_rows(&mut self) {
        self.rows.clear();
        let Some(a) = &self.archive else {
            self.right.reset(0);
            return;
        };
        if !self.inside.is_empty()
            && !a
                .entries
                .iter()
                .any(|e| archive::within(&e.name, &self.inside) && e.name.len() > self.inside.len())
        {
            self.inside.clear();
        }
        for e in &a.entries {
            let rel = if self.inside.is_empty() {
                e.name.as_str()
            } else if archive::within(&e.name, &self.inside) && e.name.len() > self.inside.len() {
                &e.name[self.inside.len() + 1..]
            } else {
                continue;
            };
            let (first, deeper) = match rel.split_once('/') {
                Some((f, _)) => (f, true),
                None => (rel, false),
            };
            let dir = deeper || e.dir;
            let at = self
                .rows
                .iter()
                .position(|r| r.dir == dir && fs::same_name(&r.name, first));
            let i = match at {
                Some(i) => i,
                None => {
                    let full = if self.inside.is_empty() {
                        String::from(first)
                    } else {
                        format!("{}/{}", self.inside, first)
                    };
                    self.rows.push(Row {
                        name: String::from(first),
                        full,
                        dir,
                        size: 0,
                        packed: 0,
                        files: 0,
                        modified: e.modified,
                    });
                    self.rows.len() - 1
                }
            };
            let r = &mut self.rows[i];
            if !e.dir {
                r.size += e.size;
                r.packed += e.packed;
                r.files += 1;
            }
            if !deeper {
                r.modified = e.modified;
            }
        }
        self.rows.sort_by(|a, b| {
            b.dir.cmp(&a.dir).then_with(|| {
                a.name
                    .chars()
                    .flat_map(char::to_lowercase)
                    .cmp(b.name.chars().flat_map(char::to_lowercase))
            })
        });
        self.right.reset(self.rows.len());
    }

    fn close_archive(&mut self) {
        self.path = None;
        self.archive = None;
        self.inside.clear();
        self.build_rows();
    }

    /// Read the archive file again after it was written.
    fn reload_archive(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        match fs::read(&path)
            .map_err(|e| String::from(e.message()))
            .and_then(|d| Archive::parse(d, &path))
        {
            Ok(a) => self.show(&path, a),
            Err(e) => {
                self.close_archive();
                self.message("Can't open the archive", &e);
            }
        }
    }

    // ---- actions ---------------------------------------------------------------

    fn message(&mut self, title: &'static str, text: &str) {
        let w = (cw() - 120).min(520);
        self.dialog = Some(Dialog::Message(title, wrap(text, w)));
    }

    fn press(&mut self, b: Button) {
        match b {
            Button::New => {
                let name = fs::unique_name(&self.dir, "Archive", ".zip");
                let d = FileDialog::new(Mode::Save, &self.dir, &name).with_extension(".zip");
                self.dialog = Some(Dialog::New(d));
            }
            Button::Open => {
                self.dialog = Some(Dialog::Open(FileDialog::new(Mode::Open, &self.dir, "")));
            }
            Button::ExtractAll => self.extract_all(),
            Button::Add => self.add_chosen(),
            Button::Extract => self.extract_chosen(),
            Button::Up(side) => self.up(side),
            Button::Level(i) => self.level = Level::ALL[i],
            Button::Delete => self.ask_delete(),
            Button::Stop => {
                if let Some(job) = &mut self.job {
                    job.fiber.cancel();
                    job.stopping = true;
                }
            }
            Button::Hint(0) => self.press(Button::New),
            Button::Hint(_) => self.press(Button::Open),
        }
    }

    /// Make an empty ZIP file and open it.
    fn create(&mut self, path: &str) {
        let empty = archive::ZipWriter::new().finish();
        match fs::write(path, &empty) {
            Ok(()) => {
                self.open_file(path);
                self.status = format!(
                    "Made {}. Choose files on the left and press Add.",
                    fs::file_name(path)
                );
            }
            Err(e) => self.message("Can't make the archive", e.message()),
        }
    }

    /// Put what is chosen on the left into the archive's folder shown on
    /// the right. With no archive open, a new ZIP file is made next to
    /// them, named after them.
    fn add_chosen(&mut self) {
        let picked = self.left.picked();
        if picked.is_empty() {
            self.status = String::from("Choose files or folders on the left to add.");
            return;
        }
        let paths: Vec<String> = picked
            .iter()
            .map(|&i| fs::join(&self.dir, &self.files[i].name))
            .collect();
        if let Some(a) = &self.archive {
            if !a.kind.editable() {
                self.message(
                    "Can't change this archive",
                    &format!("Files can only be added to ZIP archives, and this is {}. Extract it and make a ZIP instead.", a.kind.name()),
                );
                return;
            }
            let target = self.path.clone().unwrap_or_default();
            if paths.iter().any(|p| fs::same_name(p, &target)) {
                self.message(
                    "Can't add the archive to itself",
                    "Choose other files, or make a new archive.",
                );
                return;
            }
            let base = a.clone();
            let inside = self.inside.clone();
            self.pack(base, &paths, &inside, target);
        } else {
            self.pack_new(&paths);
        }
    }

    /// Pack files into a new ZIP file in their folder (Files' "Add to ZIP
    /// archive" and Add with no archive open).
    pub fn pack_new(&mut self, paths: &[String]) {
        if self.job.is_some() || paths.is_empty() {
            return;
        }
        // next to them, or in Downloads when they are on a disc
        let mut dir = fs::parent(&paths[0]);
        if fs::is_read_only(&dir) {
            dir = fs::join(&home(), "Downloads");
        }
        let base = if paths.len() == 1 {
            let name = fs::file_name(&paths[0]);
            if fs::is_dir(&paths[0]) {
                String::from(name)
            } else {
                base_name(name)
            }
        } else {
            match fs::parent(&paths[0]).as_str() {
                "/" => String::from("Archive"),
                from => String::from(fs::file_name(from)),
            }
        };
        let name = fs::unique_name(&dir, &base, ".zip");
        let target = fs::join(&dir, &name);
        if !fs::same_name(&dir, &self.dir) {
            self.go(&dir);
        }
        self.close_archive();
        self.path = Some(target.clone());
        self.pack(Rc::new(Archive::new_zip()), paths, "", target);
    }

    fn pack(&mut self, base: Rc<Archive>, paths: &[String], inside: &str, target: String) {
        let sources: Vec<Source> = paths
            .iter()
            .map(|p| {
                let name = fs::file_name(p);
                Source {
                    path: p.clone(),
                    name: if inside.is_empty() {
                        String::from(name)
                    } else {
                        format!("{}/{}", inside, name)
                    },
                }
            })
            .collect();
        let level = self.level;
        let count = sources.len();
        let dest = target.clone();
        self.run(Work::Pack(target), move |progress| {
            let data = archive::add(&base, &sources, level, progress)?;
            fs::write(&dest, &data).map_err(|e| String::from(e.message()))?;
            Ok(count)
        });
    }

    /// Start work in a fiber that reports progress.
    fn run(
        &mut self,
        work: Work,
        f: impl FnOnce(archive::Progress) -> Result<usize, String> + 'static,
    ) {
        let state = Rc::new(RefCell::new(Progress::default()));
        let st = state.clone();
        let fiber = Fiber::new(move || {
            let st2 = st.clone();
            let mut progress = move |done: u64, total: u64, name: &str| -> bool {
                {
                    let mut s = st2.borrow_mut();
                    s.done = done;
                    s.total = total;
                    if s.name != name {
                        s.name = String::from(name);
                    }
                }
                crate::fiber::pause_if_slice_used();
                !crate::fiber::cancelled()
            };
            let result = f(&mut progress);
            st.borrow_mut().result = Some(result);
        });
        self.status.clear();
        self.job = Some(Job {
            fiber,
            work,
            state,
            stopping: false,
            shown: (-1, 0),
        });
    }

    /// Run the work a little. Returns true when there is something new to
    /// show.
    pub fn tick(&mut self) -> bool {
        let Some(job) = &mut self.job else {
            return false;
        };
        if !job.fiber.resume() {
            // redraw when the card would show something new
            let s = job.state.borrow();
            let now = (percent(s.done, s.total), s.name.len() as u64 ^ s.done >> 20);
            drop(s);
            let changed = now != job.shown;
            job.shown = now;
            return changed;
        }
        let job = self.job.take().unwrap();
        let result = job.state.borrow_mut().result.take();
        let result = result.unwrap_or_else(|| Err(String::from("Stopped.")));
        match (job.work, result) {
            (Work::Pack(path), Ok(n)) => {
                serial::write_str(&format!(
                    "\narchiver: packed {} into {}\n",
                    plural(n, "item", "items"),
                    fs::file_name(&path)
                ));
                self.path = Some(path.clone());
                self.reload_archive();
                self.reload_files();
                self.pick_name(Side::Files, fs::file_name(&path));
                self.status = format!(
                    "Added {} to {}.",
                    plural(n, "item", "items"),
                    fs::file_name(&path)
                );
            }
            (Work::Unpack(dest, go), Ok(n)) => {
                serial::write_str(&format!(
                    "\narchiver: unpacked {} to {}\n",
                    plural(n, "file", "files"),
                    fs::display(&dest)
                ));
                if go {
                    let parent = fs::parent(&dest);
                    self.go(&parent);
                    self.pick_name(Side::Files, fs::file_name(&dest));
                } else {
                    self.reload_files();
                }
                self.status = format!(
                    "Extracted {} to {}.",
                    plural(n, "file", "files"),
                    fs::display(&dest)
                );
            }
            (work, Err(e)) => {
                if let Work::Pack(path) = &work {
                    // a new archive that was never written is not kept open
                    if !fs::exists(path) {
                        self.close_archive();
                    }
                }
                self.reload_files();
                if job.stopping {
                    self.status = String::from("Stopped. Nothing was changed.");
                    if let Work::Unpack(..) = work {
                        self.status = String::from("Stopped. Files extracted so far were kept.");
                    }
                } else {
                    serial::write_str(&format!("\narchiver: failed: {}\n", e));
                    self.message("Something went wrong", &e);
                }
            }
        }
        true
    }

    /// Unpack what is chosen on the right (or all of the folder shown)
    /// into the folder on the left.
    fn extract_chosen(&mut self) {
        let Some(a) = self.archive.clone() else {
            self.status = String::from("Open an archive first.");
            return;
        };
        let mut names: Vec<String> = self
            .right
            .picked()
            .into_iter()
            .map(|i| self.rows[i].full.clone())
            .collect();
        if names.is_empty() {
            names = self.rows.iter().map(|r| r.full.clone()).collect();
        }
        if names.is_empty() {
            return;
        }
        let dest = self.dir.clone();
        let strip = self.inside.clone();
        self.run(Work::Unpack(dest.clone(), false), move |progress| {
            archive::extract(&a, &names, &strip, &dest, progress)
        });
    }

    /// Unpack the whole archive into a new folder next to it, named after it.
    fn extract_all(&mut self) {
        let (Some(a), Some(path)) = (self.archive.clone(), self.path.clone()) else {
            self.status = String::from("Open an archive first.");
            return;
        };
        let dir = fs::parent(&path);
        if fs::is_read_only(&dir) {
            // an archive on a disc: into Downloads
            let downloads = fs::join(&home(), "Downloads");
            return self.unpack_to(a, &downloads, fs::file_name(&path));
        }
        self.unpack_to(a, &dir, fs::file_name(&path));
    }

    fn unpack_to(&mut self, a: Rc<Archive>, dir: &str, file: &str) {
        let name = fs::unique_name(dir, &base_name(file), "");
        let dest = fs::join(dir, &name);
        self.run(Work::Unpack(dest.clone(), true), move |progress| {
            archive::extract(&a, &[], "", &dest, progress)
        });
    }

    /// Files' "Extract to folder": unpack an archive next to itself.
    pub fn extract_here(&mut self, path: &str) {
        if self.job.is_some() {
            return;
        }
        self.open_file(path);
        if self.path.as_deref() == Some(path) {
            self.extract_all();
        }
    }

    fn ask_delete(&mut self) {
        let Some(a) = &self.archive else {
            return;
        };
        let names: Vec<String> = self
            .right
            .picked()
            .into_iter()
            .map(|i| self.rows[i].full.clone())
            .collect();
        if names.is_empty() {
            self.status = String::from("Choose files in the archive to remove.");
            return;
        }
        if !a.kind.editable() {
            self.message(
                "Can't change this archive",
                &format!(
                    "Files can only be removed from ZIP archives, and this is {}.",
                    a.kind.name()
                ),
            );
            return;
        }
        self.dialog = Some(Dialog::Delete(names));
    }

    fn delete(&mut self, names: &[String]) {
        let (Some(a), Some(path)) = (&self.archive, self.path.clone()) else {
            return;
        };
        match archive::remove(a, names)
            .and_then(|d| fs::write(&path, &d).map_err(|e| String::from(e.message())))
        {
            Ok(()) => {
                serial::write_str(&format!(
                    "\narchiver: removed {} from {}\n",
                    plural(names.len(), "item", "items"),
                    fs::file_name(&path)
                ));
                self.status = format!(
                    "Removed {} from the archive.",
                    plural(names.len(), "item", "items")
                );
                self.reload_archive();
                self.reload_files();
            }
            Err(e) => self.message("Can't change the archive", &e),
        }
    }

    /// Open a row: go into a folder, or take a file out and open it.
    fn activate(&mut self, side: Side, i: usize) {
        match side {
            Side::Files => {
                let f = &self.files[i];
                let path = fs::join(&self.dir, &f.name);
                if f.dir {
                    self.go(&path);
                } else if is_archive(&f.name) {
                    self.open_file(&path);
                } else {
                    self.open_request = Some(path);
                }
            }
            Side::Archive => {
                let row = &self.rows[i];
                if row.dir {
                    self.inside = row.full.clone();
                    self.build_rows();
                    return;
                }
                let Some(a) = &self.archive else {
                    return;
                };
                let Some(k) = a
                    .entries
                    .iter()
                    .position(|e| !e.dir && fs::same_name(&e.name, &row.full))
                else {
                    return;
                };
                // into a folder of its own, then the desktop opens it
                let user = users::current_name().unwrap_or_default();
                let temp = fs::join(&fs::app_data(user.as_str()), "Archiver");
                let _ = fs::create_dir(&temp);
                let out = fs::join(&temp, &row.name);
                match a
                    .read(k)
                    .and_then(|d| fs::write(&out, &d).map_err(|e| String::from(e.message())))
                {
                    Ok(()) => self.open_request = Some(out),
                    Err(e) => self.message("Can't open the file", &e),
                }
            }
        }
    }

    // ---- input -----------------------------------------------------------------

    fn list_mut(&mut self, side: Side) -> &mut List {
        match side {
            Side::Files => &mut self.left,
            Side::Archive => &mut self.right,
        }
    }

    fn count(&self, side: Side) -> usize {
        match side {
            Side::Files => self.files.len(),
            Side::Archive => self.rows.len(),
        }
    }

    fn max_scroll(&self, side: Side) -> i32 {
        (self.count(side) as i32 * ROW + 6 - list_rect(side).h).max(0)
    }

    fn clamp_scroll(&mut self, side: Side) {
        let max = self.max_scroll(side);
        let list = self.list_mut(side);
        list.scroll = list.scroll.clamp(0, max);
    }

    fn dialog_file(&mut self, event: filedialog::Event) -> bool {
        match event {
            filedialog::Event::None => false,
            filedialog::Event::Redraw => true,
            // only Open dialogs for several files give these
            filedialog::Event::ChosenMany(_) => false,
            filedialog::Event::Cancel => {
                self.dialog = None;
                true
            }
            filedialog::Event::Chosen(path) => {
                let new = matches!(self.dialog, Some(Dialog::New(_)));
                self.dialog = None;
                if new {
                    self.create(&path);
                } else {
                    self.open_file(&path);
                }
                true
            }
        }
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        match &mut self.dialog {
            Some(Dialog::New(d) | Dialog::Open(d)) => {
                let event = d.on_key(key, client());
                return self.dialog_file(event);
            }
            Some(Dialog::Delete(names)) => {
                if matches!(key, Key::Enter) {
                    let names = core::mem::take(names);
                    self.dialog = None;
                    self.delete(&names);
                } else if matches!(key, Key::Escape) {
                    self.dialog = None;
                }
                return true;
            }
            Some(Dialog::Message(..)) => {
                if matches!(key, Key::Enter | Key::Escape) {
                    self.dialog = None;
                }
                return true;
            }
            None => {}
        }
        if self.job.is_some() {
            if matches!(key, Key::Escape) {
                self.press(Button::Stop);
                return true;
            }
            return false;
        }
        let side = self.side;
        let n = self.count(side);
        let cur = self.list_mut(side).chosen.iter().position(|&c| c);
        match key {
            Key::Ctrl('\t') => {
                self.side = if side == Side::Files {
                    Side::Archive
                } else {
                    Side::Files
                };
            }
            Key::Up | Key::Down if n > 0 => {
                let i = match (key, cur) {
                    (Key::Up, Some(i)) => i.saturating_sub(1),
                    (Key::Down, Some(i)) => (i + 1).min(n - 1),
                    _ => 0,
                };
                let list = self.list_mut(side);
                for c in list.chosen.iter_mut() {
                    *c = false;
                }
                list.chosen[i] = true;
                list.anchor = i;
                self.scroll_to(side, i);
            }
            Key::Enter => {
                if let Some(i) = cur {
                    self.activate(side, i);
                }
            }
            Key::Backspace => self.up(side),
            Key::Ctrl('a') => {
                for c in self.list_mut(side).chosen.iter_mut() {
                    *c = true;
                }
            }
            Key::Delete if side == Side::Archive => self.ask_delete(),
            Key::Function(5) => {
                self.reload_files();
                self.reload_archive();
            }
            Key::Ctrl('n') => self.press(Button::New),
            Key::Ctrl('o') => self.press(Button::Open),
            Key::Ctrl('e') => self.press(Button::ExtractAll),
            _ => return false,
        }
        true
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        if let Some(Dialog::New(d) | Dialog::Open(d)) = &mut self.dialog {
            return d.on_wheel(clicks, client());
        }
        let side = if self.mouse_x < cw() / 2 {
            Side::Files
        } else {
            Side::Archive
        };
        self.list_mut(side).scroll += clicks * ROW * 3;
        self.clamp_scroll(side);
        true
    }

    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        self.mouse_x = x;
        let hover = if self.dialog.is_some() || self.job.is_some() {
            None
        } else {
            self.row_at(x, y)
        };
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    fn row_at(&self, x: i32, y: i32) -> Option<(Side, usize)> {
        for side in [Side::Files, Side::Archive] {
            if !list_rect(side).contains(x, y) {
                continue;
            }
            let scroll = match side {
                Side::Files => self.left.scroll,
                Side::Archive => self.right.scroll,
            };
            return (0..self.count(side))
                .find(|&i| row_rect(side, i, scroll).contains(x, y))
                .map(|i| (side, i));
        }
        None
    }

    fn buttons(&self) -> Vec<Button> {
        if self.job.is_some() {
            return vec![Button::Stop];
        }
        let mut v = vec![
            Button::New,
            Button::Open,
            Button::Add,
            Button::Extract,
            Button::Up(Side::Files),
            Button::Up(Side::Archive),
        ];
        if self.archive.is_some() {
            v.push(Button::ExtractAll);
        } else {
            v.push(Button::Hint(0));
            v.push(Button::Hint(1));
        }
        for i in 0..Level::ALL.len() {
            v.push(Button::Level(i));
        }
        if self.archive.is_some() && self.right.chosen.iter().any(|&c| c) {
            v.push(Button::Delete);
        }
        v
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match &mut self.dialog {
            Some(Dialog::New(d) | Dialog::Open(d)) => {
                if let MouseKind::Down { right: false } = ev.kind {
                    let event = d.on_click(client(), ev.x, ev.y);
                    return self.dialog_file(event);
                }
                return false;
            }
            Some(dialog @ (Dialog::Delete(_) | Dialog::Message(..))) => {
                if let MouseKind::Down { right: false } = ev.kind {
                    let (lines, buttons) = dialog_text(dialog);
                    let lines: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
                    let r = widgets::message_rect(client(), &lines, buttons);
                    let rects = widgets::message_buttons(r, buttons.len());
                    match rects.iter().position(|b| b.contains(ev.x, ev.y)) {
                        Some(0) => {
                            if let Some(Dialog::Delete(names)) = self.dialog.take() {
                                self.delete(&names);
                            }
                        }
                        Some(_) => self.dialog = None,
                        None => return false,
                    }
                    return true;
                }
                return false;
            }
            None => {}
        }
        match ev.kind {
            MouseKind::Down { right: false } => {
                if let Some(b) = self
                    .buttons()
                    .into_iter()
                    .find(|&b| button_rect(b).contains(ev.x, ev.y))
                {
                    self.pressed = Some(b);
                    return true;
                }
                if self.job.is_some() {
                    return false;
                }
                self.status.clear();
                for side in [Side::Files, Side::Archive] {
                    if pane(side).contains(ev.x, ev.y) {
                        self.side = side;
                    }
                }
                let Some((side, i)) = self.row_at(ev.x, ev.y) else {
                    // an empty spot clears the choice
                    for side in [Side::Files, Side::Archive] {
                        if list_rect(side).contains(ev.x, ev.y) {
                            for c in self.list_mut(side).chosen.iter_mut() {
                                *c = false;
                            }
                        }
                    }
                    return true;
                };
                let now = interrupts::ticks();
                let (t, s, k) = self.last_click;
                self.last_click = (now, side, i);
                if s == side && k == i && now - t < DOUBLE_CLICK {
                    self.last_click = (0, side, usize::MAX);
                    self.activate(side, i);
                    return true;
                }
                let scroll = self.list_mut(side).scroll;
                let tick_box = ev.x < row_rect(side, i, scroll).x + 30;
                self.list_mut(side).click(i, tick_box);
                true
            }
            MouseKind::Up => {
                let Some(b) = self.pressed.take() else {
                    return false;
                };
                if button_rect(b).contains(ev.x, ev.y) {
                    self.press(b);
                }
                true
            }
            _ => false,
        }
    }

    // ---- drawing ---------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, caret: bool) {
        c.fill_rect(0, 0, cw(), ch(), theme::face());
        self.draw_head(c);
        self.draw_files(c);
        self.draw_archive(c);
        self.draw_gutter(c);
        self.draw_foot(c);
        if let Some(job) = &self.job {
            draw_progress(c, job, self.pressed == Some(Button::Stop));
        }
        match &mut self.dialog {
            Some(Dialog::New(d) | Dialog::Open(d)) => d.draw(c, client(), caret),
            Some(d @ (Dialog::Delete(_) | Dialog::Message(..))) => {
                let (lines, buttons) = dialog_text(d);
                let title = match d {
                    Dialog::Delete(_) => "Remove from the archive?",
                    Dialog::Message(t, _) => *t,
                    _ => "",
                };
                let lines: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
                widgets::draw_message(c, client(), title, &lines, buttons, None);
            }
            None => {}
        }
    }

    fn draw_head(&self, c: &mut Canvas) {
        super::icons::get().draw(c, App::Archiver, super::icons::LARGE, 14, 8);
        let (title, sub) = match (&self.path, &self.archive) {
            (Some(p), Some(a)) => {
                let files = a.entries.iter().filter(|e| !e.dir).count();
                let (size, packed) = a.totals();
                let mut sub = format!(
                    "{} • {} • {}",
                    a.kind.name(),
                    plural(files, "file", "files"),
                    size_text(size)
                );
                if a.kind != archive::Kind::Tar && size > 0 {
                    let disk = a.disk as u64;
                    sub.push_str(&format!(" packed to {}", size_text(disk)));
                    if a.kind == archive::Kind::Zip {
                        sub.push_str(&format!(" • {}% smaller", saved(size, packed)));
                    }
                }
                (String::from(fs::file_name(p)), sub)
            }
            (Some(p), None) => (String::from(fs::file_name(p)), String::from("Packing...")),
            _ => (
                String::from("Archiver"),
                String::from("Pack files into ZIP archives, open ZIP, TAR and GZIP"),
            ),
        };
        let room = button_rect(Button::New).x - 76 - 16;
        c.draw_text_in(
            &TITLE,
            72,
            10,
            &super::search::fit(&title, room),
            theme::text(),
        );
        c.draw_text(72, 38, &super::search::fit(&sub, room), theme::text_dim());
        let pressed = |b| self.pressed == Some(b);
        theme::button(c, button_rect(Button::New), "New ZIP", pressed(Button::New));
        theme::button(
            c,
            button_rect(Button::Open),
            "Open...",
            pressed(Button::Open),
        );
        let r = button_rect(Button::ExtractAll);
        if self.archive.is_some() {
            theme::accent_button(c, r, "Extract all", pressed(Button::ExtractAll));
        } else {
            disabled_button(c, r, "Extract all");
        }
    }

    fn pane_frame(
        &self,
        c: &mut Canvas,
        side: Side,
        title: &str,
        icon: impl Fn(&mut Canvas, i32, i32),
    ) {
        let p = pane(side);
        let focused = self.side == side;
        c.fill_round(p, 8, theme::light());
        c.outline_round(
            p,
            8,
            if focused {
                mix(theme::accent(), theme::light(), 110)
            } else {
                theme::stroke()
            },
        );
        let up = button_rect(Button::Up(side));
        let can_up = match side {
            Side::Files => self.dir != "/",
            Side::Archive => !self.inside.is_empty(),
        };
        theme::button(c, up, "", self.pressed == Some(Button::Up(side)));
        let color = if can_up {
            theme::text()
        } else {
            theme::text_dim()
        };
        let (ax, ay) = (up.x + up.w / 2, up.y + 7);
        c.line(ax, ay, ax, ay + 13, color);
        c.line(ax - 5, ay + 5, ax, ay, color);
        c.line(ax + 5, ay + 5, ax, ay, color);
        icon(c, up.right() + 10, p.y + 14);
        let tx = up.right() + 34;
        c.draw_text_in(
            &UI_BOLD,
            tx,
            p.y + 13,
            &super::search::fit(title, p.right() - tx - 12),
            theme::text(),
        );
        c.fill_rect(p.x + 1, p.y + PANE_HEAD - 1, p.w - 2, 1, theme::stroke());
    }

    fn draw_files(&self, c: &mut Canvas) {
        let side = Side::Files;
        let p = pane(side);
        let title = if self.dir == "/" {
            String::from("Computer")
        } else {
            fs::display(&self.dir)
        };
        self.pane_frame(c, side, &title, |c, x, y| widgets::folder_icon(c, x, y, 16));
        let size_x = p.right() - 16;
        let cy = p.y + PANE_HEAD + 5;
        c.draw_text(p.x + 44, cy, "Name", theme::text_dim());
        right_text(c, size_x, cy, "Size", theme::text_dim());
        c.fill_rect(
            p.x + 8,
            p.y + PANE_HEAD + COLS_H - 1,
            p.w - 16,
            1,
            theme::stroke(),
        );

        let l = list_rect(side);
        let mut lc = c.sub(Rect::new(0, 0, c.width, c.height));
        lc.clip_to(l);
        for (i, f) in self.files.iter().enumerate() {
            let r = row_rect(side, i, self.left.scroll);
            if r.bottom() < l.y || r.y > l.bottom() {
                continue;
            }
            let chosen = self.left.chosen[i];
            row_back(&mut lc, r, chosen, self.hover == Some((side, i)));
            tick_box(&mut lc, r.x + 8, r.y + 7, chosen);
            if f.dir {
                widgets::folder_icon(&mut lc, r.x + 32, r.y + 7, 16);
            } else if is_archive(&f.name) {
                zip_icon(&mut lc, r.x + 32, r.y + 7, 16);
            } else {
                widgets::file_icon(&mut lc, r.x + 32, r.y + 7, 16);
            }
            let name_w = size_x - 90 - (r.x + 56);
            lc.draw_text(
                r.x + 56,
                r.y + 6,
                &super::search::fit(&f.name, name_w),
                theme::text(),
            );
            if !f.dir {
                right_text(
                    &mut lc,
                    size_x,
                    r.y + 6,
                    &size_text(f.size as u64),
                    theme::text_dim(),
                );
            }
        }
        if self.files.is_empty() {
            lc.text_centered(
                Rect::new(l.x, l.y + 20, l.w, 24),
                "This folder is empty.",
                theme::text_dim(),
            );
        }
        drop(lc);
        scrollbar(c, l, self.count(side), self.left.scroll);
    }

    fn draw_archive(&self, c: &mut Canvas) {
        let side = Side::Archive;
        let p = pane(side);
        let title = match &self.path {
            Some(path) => {
                let mut t = String::from(fs::file_name(path));
                for part in self.inside.split('/').filter(|s| !s.is_empty()) {
                    t.push_str(" › ");
                    t.push_str(part);
                }
                t
            }
            None => String::from("No archive open"),
        };
        self.pane_frame(c, side, &title, |c, x, y| zip_icon(c, x, y, 16));
        let l = list_rect(side);
        if self.archive.is_none() {
            // a new archive being packed has nothing to show yet
            if self.path.is_none() {
                self.draw_hint(c, p);
            }
            return;
        }
        let ratio_x = p.right() - 16 - 110;
        let size_x = ratio_x - 16;
        let cy = p.y + PANE_HEAD + 5;
        c.draw_text(p.x + 44, cy, "Name", theme::text_dim());
        right_text(c, size_x, cy, "Size", theme::text_dim());
        c.draw_text(ratio_x, cy, "Packed", theme::text_dim());
        c.fill_rect(
            p.x + 8,
            p.y + PANE_HEAD + COLS_H - 1,
            p.w - 16,
            1,
            theme::stroke(),
        );

        let mut lc = c.sub(Rect::new(0, 0, c.width, c.height));
        lc.clip_to(l);
        for (i, row) in self.rows.iter().enumerate() {
            let r = row_rect(side, i, self.right.scroll);
            if r.bottom() < l.y || r.y > l.bottom() {
                continue;
            }
            let chosen = self.right.chosen[i];
            row_back(&mut lc, r, chosen, self.hover == Some((side, i)));
            tick_box(&mut lc, r.x + 8, r.y + 7, chosen);
            if row.dir {
                widgets::folder_icon(&mut lc, r.x + 32, r.y + 7, 16);
            } else if is_archive(&row.name) {
                zip_icon(&mut lc, r.x + 32, r.y + 7, 16);
            } else {
                widgets::file_icon(&mut lc, r.x + 32, r.y + 7, 16);
            }
            let name_w = size_x - 80 - (r.x + 56);
            lc.draw_text(
                r.x + 56,
                r.y + 6,
                &super::search::fit(&row.name, name_w),
                theme::text(),
            );
            let size = if row.dir {
                plural(row.files, "file", "files")
            } else {
                size_text(row.size)
            };
            right_text(&mut lc, size_x, r.y + 6, &size, theme::text_dim());
            ratio(&mut lc, ratio_x, r.y + 9, row.size, row.packed);
        }
        if self.rows.is_empty() {
            let text = "The archive is empty. Choose files on the left and press Add.";
            lc.text_centered(
                Rect::new(l.x, l.y + 20, l.w, 24),
                &super::search::fit(text, l.w - 20),
                theme::text_dim(),
            );
        }
        drop(lc);
        scrollbar(c, l, self.count(side), self.right.scroll);
    }

    /// With no archive open: what to do.
    fn draw_hint(&self, c: &mut Canvas, p: Rect) {
        let cx = p.x + p.w / 2;
        let cy = p.y + p.h / 2;
        super::icons::get().draw(c, App::Archiver, super::icons::LARGE, cx - 24, cy - 110);
        let lines = [
            "Choose files on the left and press Add:",
            "they are packed into a new ZIP next to them.",
            "Or make an empty archive, or open one.",
        ];
        for (k, line) in lines.iter().enumerate() {
            let r = Rect::new(p.x + 10, cy - 50 + k as i32 * 22, p.w - 20, 20);
            c.text_centered(r, &super::search::fit(line, r.w), theme::text_dim());
        }
        for (i, label) in ["New ZIP", "Open..."].into_iter().enumerate() {
            let b = Button::Hint(i);
            theme::button(c, button_rect(b), label, self.pressed == Some(b));
        }
    }

    fn draw_gutter(&self, c: &mut Canvas) {
        let add_on = self.left.chosen.iter().any(|&c| c);
        let out_on = self.archive.is_some() && !self.rows.is_empty();
        for (b, label, on, right) in [
            (Button::Add, "Add", add_on, true),
            (Button::Extract, "Extract", out_on, false),
        ] {
            let r = button_rect(b);
            let pressed = self.pressed == Some(b);
            let face = if !on {
                theme::face()
            } else if pressed {
                mix(theme::accent(), theme::light(), 60)
            } else {
                theme::accent()
            };
            c.fill_round(r, 10, face);
            c.outline_round(
                r,
                10,
                if on {
                    mix(theme::accent(), theme::text(), 40)
                } else {
                    theme::stroke()
                },
            );
            let fg = if on {
                theme::on_accent()
            } else {
                theme::text_dim()
            };
            arrow(c, r.x + r.w / 2, r.y + 18, right, fg);
            c.text_centered(Rect::new(r.x, r.y + 30, r.w, 22), label, fg);
        }
    }

    fn draw_foot(&self, c: &mut Canvas) {
        let y = ch() - FOOT_H;
        c.draw_text(MARGIN + 2, y + 16, "Packing", theme::text_dim());
        for (i, level) in Level::ALL.into_iter().enumerate() {
            theme::toggle_button(
                c,
                button_rect(Button::Level(i)),
                level.name(),
                self.level == level,
            );
        }
        let status_x = button_rect(Button::Level(3)).right() + 20;
        let mut right = cw() - MARGIN;
        if self.buttons().contains(&Button::Delete) {
            let r = button_rect(Button::Delete);
            theme::button(c, r, "Remove", self.pressed == Some(Button::Delete));
            right = r.x - 12;
        }
        let text = if !self.status.is_empty() {
            self.status.clone()
        } else {
            let (l, r) = (self.left.picked().len(), self.right.picked().len());
            match (l, r) {
                (0, 0) if self.archive.is_some() => {
                    String::from("Double-click to open. Tick rows to choose several.")
                }
                (0, 0) => String::from("Tick files on the left, then press Add."),
                (l, 0) => format!("{} chosen on the left", l),
                (0, r) => format!("{} chosen in the archive", r),
                (l, r) => format!("{} chosen on the left, {} in the archive", l, r),
            }
        };
        c.draw_text(
            status_x,
            y + 16,
            &super::search::fit(&text, right - status_x),
            theme::text_dim(),
        );
    }
}

fn dialog_text(d: &Dialog) -> (Vec<String>, &'static [&'static str]) {
    match d {
        Dialog::Delete(names) => {
            let what = if names.len() == 1 {
                format!("\"{}\"", names[0].rsplit('/').next().unwrap_or(""))
            } else {
                plural(names.len(), "item", "items")
            };
            (
                vec![
                    format!("{} will be taken out of the archive.", what),
                    String::from("The files on the left stay as they are."),
                ],
                &["Remove", "Cancel"],
            )
        }
        Dialog::Message(_, lines) => (lines.clone(), &["OK"]),
        _ => (Vec::new(), &[]),
    }
}

// ---- small drawings --------------------------------------------------------------

fn right_text(c: &mut Canvas, right: i32, y: i32, s: &str, color: Color) {
    c.draw_text(right - UI.width(s), y, s, color);
}

fn disabled_button(c: &mut Canvas, r: Rect, label: &str) {
    c.fill_round(r, theme::CONTROL_RADIUS, theme::face());
    c.outline_round(r, theme::CONTROL_RADIUS, theme::stroke());
    c.text_centered(r, label, mix(theme::text_dim(), theme::face(), 90));
}

fn row_back(c: &mut Canvas, r: Rect, chosen: bool, hover: bool) {
    if chosen {
        c.fill_round(r, 5, theme::selection());
    } else if hover {
        c.fill_round(r, 5, theme::row_hover());
    }
}

/// A round-cornered tick box.
fn tick_box(c: &mut Canvas, x: i32, y: i32, on: bool) {
    let r = Rect::new(x, y, 16, 16);
    if on {
        c.fill_round(r, 4, theme::accent());
        let fg = theme::on_accent();
        for d in 0..2 {
            c.line(x + 4, y + 8 + d, x + 7, y + 11 + d, fg);
            c.line(x + 7, y + 11 + d, x + 12, y + 5 + d, fg);
        }
    } else {
        c.fill_round(r, 4, theme::light());
        c.outline_round(r, 4, mix(theme::text_dim(), theme::light(), 110));
    }
}

/// How much a file was packed: a bar that fills with what was saved, and
/// the percentage.
fn ratio(c: &mut Canvas, x: i32, y: i32, size: u64, packed: u64) {
    let pct = saved(size, packed);
    let bar = Rect::new(x, y + 4, 56, 6);
    let green = rgb(0x2e, 0xb8, 0x6e);
    c.fill_round(bar, 3, mix(theme::stroke(), theme::light(), 60));
    if pct > 0 {
        c.fill_round(
            Rect::new(bar.x, bar.y, (bar.w * pct as i32 / 100).max(6), bar.h),
            3,
            green,
        );
    }
    let text = if size == 0 {
        String::from("–")
    } else if pct == 0 {
        String::from("stored")
    } else {
        format!("-{}%", pct)
    };
    c.draw_text(
        bar.right() + 8,
        y - 3,
        &text,
        if pct > 0 {
            theme::text()
        } else {
            theme::text_dim()
        },
    );
}

/// An arrow pointing right or left, centred on (cx, cy).
fn arrow(c: &mut Canvas, cx: i32, cy: i32, right: bool, color: Color) {
    let d = if right { 1 } else { -1 };
    for t in -1..=1 {
        c.line(cx - 9 * d, cy + t, cx + 8 * d, cy + t, color);
    }
    let tip = cx + 9 * d;
    c.fill_polygon(
        &[(tip, cy), (tip - 8 * d, cy - 7), (tip - 8 * d, cy + 7)],
        color,
    );
}

/// A page with a zip down its middle.
pub fn zip_icon(c: &mut Canvas, x: i32, y: i32, s: i32) {
    let w = s * 3 / 4;
    let px = x + (s - w) / 2;
    let page = Rect::new(px, y, w, s);
    c.fill_round(page, (s / 8).max(2), rgb(0xff, 0xd2, 0x4c));
    c.outline_round(page, (s / 8).max(2), rgb(0x9a, 0x70, 0x10));
    let zx = px + w / 2 - s / 10;
    let zw = (s / 5).max(3);
    c.fill(Rect::new(zx, y + 1, zw, s - 2), rgb(0x8a, 0x7c, 0xf0));
    let step = (s / 8).max(2);
    let mut ty = y + 2;
    let mut k = 0;
    while ty < y + s - 3 {
        let tx = if k % 2 == 0 { zx } else { zx + zw / 2 };
        c.fill(Rect::new(tx, ty, zw / 2, 1), rgb(0x30, 0x28, 0x60));
        ty += step;
        k += 1;
    }
    c.fill_round(
        Rect::new(zx - 1, y + s / 2, zw + 2, s / 5 + 1),
        1,
        rgb(0x3c, 0x8c, 0xf0),
    );
}

fn scrollbar(c: &mut Canvas, l: Rect, count: usize, scroll: i32) {
    let total = count as i32 * ROW + 6;
    if total <= l.h {
        return;
    }
    let track = Rect::new(l.right() - 10, l.y + 2, 8, l.h - 4);
    let t = widgets::thumb(track, true, total, l.h, scroll).inset(1);
    c.fill_round(t, 3, theme::thumb());
}

fn draw_progress(c: &mut Canvas, job: &Job, pressed: bool) {
    c.fill_round_alpha(client(), 0, rgb(0x20, 0x20, 0x28), 60);
    let card = progress_card();
    c.shadow(card, 10, 16, 4, 120);
    c.fill_round(card, 10, theme::light());
    c.outline_round(card, 10, theme::frame());
    let s = job.state.borrow();
    let title = match (&job.work, job.stopping) {
        (_, true) => String::from("Stopping..."),
        (Work::Pack(p), _) => format!("Packing {}", fs::file_name(p)),
        (Work::Unpack(d, _), _) => format!("Extracting to {}", fs::file_name(d)),
    };
    c.draw_text_in(
        &TITLE,
        card.x + 20,
        card.y + 18,
        &super::search::fit(&title, card.w - 40),
        theme::text(),
    );
    let name = s.name.rsplit('/').next().unwrap_or("");
    c.draw_text(
        card.x + 20,
        card.y + 54,
        &super::search::fit(name, card.w - 40),
        theme::text_dim(),
    );
    let bar = Rect::new(card.x + 20, card.y + 84, card.w - 40, 10);
    c.fill_round(bar, 5, theme::track());
    let pct = percent(s.done, s.total);
    if pct > 0 {
        c.fill_round(
            Rect::new(bar.x, bar.y, (bar.w * pct / 100).max(10), bar.h),
            5,
            theme::accent(),
        );
    }
    let text = if s.total > 0 {
        format!("{}% • {} of {}", pct, size_text(s.done), size_text(s.total))
    } else {
        String::from("Getting ready...")
    };
    c.draw_text(card.x + 20, card.bottom() - 42, &text, theme::text_dim());
    theme::button(c, button_rect(Button::Stop), "Stop", pressed);
}
