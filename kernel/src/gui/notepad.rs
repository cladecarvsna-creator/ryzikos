//! Notepad, like the one in Windows 11: File and Edit menus, open and
//! save on the disk (Ctrl+O, Ctrl+S, Ctrl+Shift+S), selection with the
//! mouse and Shift+arrows, clipboard, undo and redo, scroll bars, and
//! text in any language in UTF-8 with Windows line ends.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use super::canvas::{Canvas, Rect};
use super::filedialog::{self, FileDialog, Mode};
use super::text::{MONO, UI};
use super::theme;
use super::widgets::{self, Item};
use super::{MouseEvent, MouseKind};
use crate::fs;
use crate::keyboard::{self, Key};
use crate::{interrupts, rtc, users};

pub const CLIENT_W: i32 = 1000;
pub const CLIENT_H: i32 = 640;

/// The window's size now; it opens at CLIENT_W x CLIENT_H.
fn cw() -> i32 {
    super::client_w(super::App::Notepad)
}
fn ch() -> i32 {
    super::client_h(super::App::Notepad)
}

const MENU_H: i32 = 34;
const STATUS_H: i32 = 26;
const SB: i32 = 14;
const PAD: i32 = 8;
const LINE_H: i32 = 20;
const TAB: usize = 4;
const MAX_UNDO: usize = 200;
/// Bigger files are not opened.
const MAX_FILE: usize = 8 * 1024 * 1024;
const DOUBLE_CLICK: u64 = interrupts::TIMER_HZ / 2;

/// A place in the text: a line and a character in it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
struct Pos {
    line: usize,
    col: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    None,
    Typing,
    Deleting,
    Other,
}

struct Snapshot {
    text: String,
    cur: Pos,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    File,
    Edit,
    /// The Edit menu at the mouse, from a right click.
    Context,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    New,
    Open,
    Save,
    SaveAs,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    TimeDate,
}

const FILE_CMDS: [Option<Cmd>; 6] = [
    Some(Cmd::New),
    Some(Cmd::Open),
    Some(Cmd::Save),
    Some(Cmd::SaveAs),
    None,
    Some(Cmd::Exit),
];

const EDIT_CMDS: [Option<Cmd>; 11] = [
    Some(Cmd::Undo),
    Some(Cmd::Redo),
    None,
    Some(Cmd::Cut),
    Some(Cmd::Copy),
    Some(Cmd::Paste),
    Some(Cmd::Delete),
    None,
    Some(Cmd::SelectAll),
    None,
    Some(Cmd::TimeDate),
];

/// What to do once the question about unsaved changes is answered.
#[derive(Clone)]
enum After {
    New,
    Open(Option<String>),
    Close,
}

enum Dialog {
    File(FileDialog),
    /// Save changes before going on?
    SaveChanges,
    /// Replace an existing file with Save as?
    Replace(String),
    Message(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    None,
    Text,
    VThumb(i32),
    HThumb(i32),
}

pub struct Notepad {
    lines: Vec<Vec<char>>,
    cur: Pos,
    /// The other end of the selection.
    anchor: Option<Pos>,
    /// The column Up and Down try to keep.
    want_col: Option<usize>,
    /// First line shown, and how far the text is scrolled right in pixels.
    scroll_y: usize,
    scroll_x: i32,
    path: Option<String>,
    modified: bool,
    /// Save with CRLF (Windows) or LF line ends.
    crlf: bool,
    /// How the file was encoded; it is saved as UTF-8.
    encoding: &'static str,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: EditKind,
    menu: Option<(MenuKind, Rect)>,
    menu_hover: Option<usize>,
    dialog: Option<Dialog>,
    pending: Option<After>,
    drag: Drag,
    last_click: (u64, Pos),
    /// Asks the desktop to close the window.
    pub want_close: bool,
}

fn area() -> Rect {
    Rect::new(0, MENU_H, cw() - SB, ch() - MENU_H - STATUS_H - SB)
}

fn client() -> Rect {
    Rect::new(0, 0, cw(), ch())
}

fn vtrack() -> Rect {
    let a = area();
    Rect::new(a.right(), a.y, SB, a.h)
}

fn htrack() -> Rect {
    let a = area();
    Rect::new(a.x, a.bottom(), a.w, SB)
}

fn rows() -> usize {
    ((area().h - 2 * 4) / LINE_H) as usize
}

/// Character width in 1/16 pixels: the font is monospaced.
fn cw16() -> i32 {
    MONO.advance16('M') as i32
}

/// Where a column of a line is on the screen, counting tabs.
fn visual_col(line: &[char], col: usize) -> usize {
    let mut v = 0;
    for &c in &line[..col.min(line.len())] {
        v = if c == '\t' {
            (v / TAB + 1) * TAB
        } else {
            v + 1
        };
    }
    v
}

/// The character column nearest to a screen column.
fn col_at(line: &[char], target: usize) -> usize {
    let mut v = 0;
    for (i, &c) in line.iter().enumerate() {
        let next = if c == '\t' {
            (v / TAB + 1) * TAB
        } else {
            v + 1
        };
        if target < next {
            // closer to this character's left or right side
            return if target - v <= (next - v) / 2 {
                i
            } else {
                i + 1
            };
        }
        v = next;
    }
    line.len()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Bytes of a text file to text and the name of its encoding: UTF-8
/// (with or without a byte order mark), else Windows-1251, which older
/// Russian files use.
fn decode(bytes: &[u8]) -> (String, &'static str) {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    match core::str::from_utf8(bytes) {
        Ok(s) => (String::from(s), "UTF-8"),
        Err(_) => (bytes.iter().map(|&b| cp1251(b)).collect(), "Windows-1251"),
    }
}

fn cp1251(b: u8) -> char {
    match b {
        0..=0x7f => b as char,
        0xc0..=0xff => char::from_u32(0x410 + (b - 0xc0) as u32).unwrap_or('?'),
        0xa8 => 'Ё',
        0xb8 => 'ё',
        0xb9 => '№',
        0xab => '«',
        0xbb => '»',
        0x96 => '–',
        0x97 => '—',
        0x85 => '…',
        0xa0 => ' ',
        _ => '?',
    }
}

impl Notepad {
    /// The window got a new size.
    pub fn resized(&mut self) {
        self.clamp_scroll();
    }

    pub fn new() -> Self {
        Self {
            lines: vec![Vec::new()],
            cur: Pos::default(),
            anchor: None,
            want_col: None,
            scroll_y: 0,
            scroll_x: 0,
            path: None,
            modified: false,
            crlf: true,
            encoding: "UTF-8",
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: EditKind::None,
            menu: None,
            menu_hover: None,
            dialog: None,
            pending: None,
            drag: Drag::None,
            last_click: (0, Pos::default()),
            want_close: false,
        }
    }

    /// The window title: the file name, with a star when it has unsaved
    /// changes.
    pub fn title(&self) -> String {
        let mut t = String::new();
        if self.modified {
            t.push('*');
        }
        t.push_str(self.path.as_deref().map_or("Untitled", fs::file_name));
        t.push_str(" - Text Editor");
        t
    }

    fn name(&self) -> &str {
        self.path.as_deref().map_or("Untitled", fs::file_name)
    }

    // ---- text -----------------------------------------------------------------

    fn text(&self) -> String {
        let mut s = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.extend(line.iter());
        }
        s
    }

    fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(|l| l.chars().collect()).collect();
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
    }

    fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos {
            line,
            col: p.col.min(self.lines[line].len()),
        }
    }

    fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        (a != self.cur).then(|| (a.min(self.cur), a.max(self.cur)))
    }

    fn text_between(&self, a: Pos, b: Pos) -> String {
        let mut s = String::new();
        for i in a.line..=b.line {
            let line = &self.lines[i];
            let from = if i == a.line { a.col } else { 0 };
            let to = if i == b.line { b.col } else { line.len() };
            s.extend(line[from..to].iter());
            if i < b.line {
                s.push('\n');
            }
        }
        s
    }

    fn remove(&mut self, a: Pos, b: Pos) {
        if a.line == b.line {
            self.lines[a.line].drain(a.col..b.col);
        } else {
            let tail = self.lines[b.line].split_off(b.col);
            self.lines[a.line].truncate(a.col);
            self.lines[a.line].extend(tail);
            self.lines.drain(a.line + 1..=b.line);
        }
        self.cur = a;
        self.anchor = None;
    }

    fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((a, b)) => {
                self.remove(a, b);
                true
            }
            None => {
                self.anchor = None;
                false
            }
        }
    }

    /// Put text at the caret, over the selection.
    fn insert(&mut self, s: &str) {
        self.delete_selection();
        let Pos { mut line, mut col } = self.cur;
        let tail = self.lines[line].split_off(col);
        for c in s.chars() {
            match c {
                '\r' => {}
                '\n' => {
                    line += 1;
                    col = 0;
                    self.lines.insert(line, Vec::new());
                }
                c => {
                    self.lines[line].push(c);
                    col += 1;
                }
            }
        }
        self.lines[line].extend(tail);
        self.cur = Pos { line, col };
        self.want_col = None;
    }

    // ---- undo -----------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text(),
            cur: self.cur,
        }
    }

    /// Call before changing the text. Typing and deleting in a row make
    /// one step to undo.
    fn edit(&mut self, kind: EditKind) {
        if kind == EditKind::Other || kind != self.last_edit {
            let s = self.snapshot();
            self.undo.push(s);
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }
        self.last_edit = kind;
        self.redo.clear();
        self.modified = true;
    }

    fn restore(&mut self, s: Snapshot) {
        self.set_text(&s.text);
        self.cur = self.clamp(s.cur);
        self.anchor = None;
        self.modified = true;
        self.last_edit = EditKind::None;
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            let now = self.snapshot();
            self.redo.push(now);
            self.restore(s);
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            let now = self.snapshot();
            self.undo.push(now);
            self.restore(s);
        }
    }

    // ---- files ----------------------------------------------------------------

    fn clear(&mut self) {
        let want_close = self.want_close;
        *self = Self::new();
        self.want_close = want_close;
    }

    fn load(&mut self, path: &str) {
        match fs::read(path) {
            Ok(bytes) if bytes.len() > MAX_FILE => {
                self.dialog = Some(Dialog::Message("This file is too big for the Text Editor."));
            }
            Ok(bytes) => {
                self.clear();
                let (text, encoding) = decode(&bytes);
                self.encoding = encoding;
                self.crlf = text.contains("\r\n") || !text.contains('\n');
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                self.set_text(&text);
                self.path = Some(String::from(path));
            }
            Err(e) => self.dialog = Some(Dialog::Message(e.message())),
        }
    }

    /// Save to `path`. Returns whether it worked.
    fn save_to(&mut self, path: &str) -> bool {
        let mut text = self.text();
        if self.crlf {
            text = text.replace('\n', "\r\n");
        }
        match fs::write(path, text.as_bytes()) {
            Ok(()) => {
                self.path = Some(String::from(path));
                self.modified = false;
                self.encoding = "UTF-8";
                crate::serial::write_str("notepad: saved ");
                crate::serial::write_str(path);
                crate::serial::write_str("\n");
                true
            }
            Err(e) => {
                self.dialog = Some(Dialog::Message(e.message()));
                false
            }
        }
    }

    /// Where the Open and Save as dialogs start.
    fn start_dir(&self) -> String {
        match &self.path {
            Some(p) => fs::parent(p),
            None => {
                let home = fs::home(users::current_name().unwrap_or_default().as_str());
                let docs = fs::join(&home, "Documents");
                if fs::is_dir(&docs) {
                    docs
                } else {
                    String::from("/")
                }
            }
        }
    }

    fn save_as(&mut self) {
        let name = match &self.path {
            Some(p) => String::from(fs::file_name(p)),
            None => String::from("Untitled.txt"),
        };
        let dir = self.start_dir();
        self.dialog = Some(Dialog::File(FileDialog::new(Mode::Save, &dir, &name)));
    }

    /// Save to the file's own path, or ask for one. Returns whether it
    /// is saved now.
    fn save(&mut self) -> bool {
        match self.path.clone() {
            Some(p) => self.save_to(&p),
            None => {
                self.save_as();
                false
            }
        }
    }

    /// Do something that throws the text away, asking first if it has
    /// unsaved changes.
    fn request(&mut self, after: After) {
        self.menu = None;
        if self.modified {
            self.pending = Some(after);
            self.dialog = Some(Dialog::SaveChanges);
        } else {
            self.perform(after);
        }
    }

    fn perform(&mut self, after: After) {
        match after {
            After::New => self.clear(),
            After::Open(None) => {
                let dir = self.start_dir();
                self.dialog = Some(Dialog::File(FileDialog::new(Mode::Open, &dir, "")));
            }
            After::Open(Some(path)) => self.load(&path),
            After::Close => {
                self.clear();
                self.want_close = true;
            }
        }
    }

    /// Saving finished: carry on with what was waiting for it.
    fn saved(&mut self) {
        if let Some(after) = self.pending.take() {
            self.perform(after);
        }
    }

    /// Open a file, from File Explorer or the shell.
    pub fn open_file(&mut self, path: &str) {
        if self.path.as_deref() == Some(path) && !self.modified {
            return;
        }
        self.dialog = None;
        self.request(After::Open(Some(String::from(path))));
    }

    /// The window's close button: returns whether it may close now.
    pub fn try_close(&mut self) -> bool {
        if self.modified {
            self.dialog = None;
            self.request(After::Close);
            false
        } else {
            self.clear();
            true
        }
    }

    fn run(&mut self, cmd: Cmd) {
        self.menu = None;
        match cmd {
            Cmd::New => self.request(After::New),
            Cmd::Open => self.request(After::Open(None)),
            Cmd::Save => {
                if self.save() {
                    self.saved();
                }
            }
            Cmd::SaveAs => self.save_as(),
            Cmd::Exit => self.request(After::Close),
            Cmd::Undo => self.undo(),
            Cmd::Redo => self.redo(),
            Cmd::Cut | Cmd::Copy => {
                if let Some((a, b)) = self.selection() {
                    widgets::copy(&self.text_between(a, b));
                    if cmd == Cmd::Cut {
                        self.edit(EditKind::Other);
                        self.remove(a, b);
                    }
                }
            }
            Cmd::Paste => {
                let clip = widgets::paste();
                if !clip.is_empty() {
                    self.edit(EditKind::Other);
                    self.insert(&clip);
                }
            }
            Cmd::Delete => {
                if self.selection().is_some() {
                    self.edit(EditKind::Other);
                    self.delete_selection();
                }
            }
            Cmd::SelectAll => {
                self.anchor = Some(Pos::default());
                let last = self.lines.len() - 1;
                self.cur = Pos {
                    line: last,
                    col: self.lines[last].len(),
                };
            }
            Cmd::TimeDate => {
                let (h, m, _) = rtc::time();
                let (y, mo, d) = rtc::date();
                let mut s = String::new();
                let _ = write!(s, "{:02}:{:02} {:02}.{:02}.{}", h, m, d, mo, y);
                self.edit(EditKind::Other);
                self.insert(&s);
            }
        }
        self.scroll_to_cursor();
    }

    // ---- moving the caret -------------------------------------------------------

    fn move_to(&mut self, to: Pos, keep_col: bool) {
        if keyboard::shift_held() {
            self.anchor.get_or_insert(self.cur);
        } else {
            self.anchor = None;
        }
        self.cur = self.clamp(to);
        if !keep_col {
            self.want_col = None;
        }
        self.last_edit = EditKind::None;
    }

    fn vertical(&mut self, lines: isize) {
        let line = &self.lines[self.cur.line];
        let want = *self
            .want_col
            .get_or_insert_with(|| visual_col(line, self.cur.col));
        let target = (self.cur.line as isize + lines).clamp(0, self.lines.len() as isize - 1);
        let target = target as usize;
        let col = col_at(&self.lines[target], want);
        self.move_to(Pos { line: target, col }, true);
    }

    fn word_left(&self) -> Pos {
        let Pos { line, col } = self.cur;
        if col == 0 {
            return if line > 0 {
                Pos {
                    line: line - 1,
                    col: self.lines[line - 1].len(),
                }
            } else {
                self.cur
            };
        }
        let l = &self.lines[line];
        let mut i = col;
        while i > 0 && !is_word(l[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(l[i - 1]) {
            i -= 1;
        }
        Pos { line, col: i }
    }

    fn word_right(&self) -> Pos {
        let Pos { line, col } = self.cur;
        let l = &self.lines[line];
        if col == l.len() {
            return if line + 1 < self.lines.len() {
                Pos {
                    line: line + 1,
                    col: 0,
                }
            } else {
                self.cur
            };
        }
        let mut i = col;
        while i < l.len() && is_word(l[i]) {
            i += 1;
        }
        while i < l.len() && !is_word(l[i]) {
            i += 1;
        }
        Pos { line, col: i }
    }

    fn scroll_to_cursor(&mut self) {
        let rows = rows();
        if self.cur.line < self.scroll_y {
            self.scroll_y = self.cur.line;
        } else if self.cur.line >= self.scroll_y + rows {
            self.scroll_y = self.cur.line + 1 - rows;
        }
        let a = area();
        let x = visual_col(&self.lines[self.cur.line], self.cur.col) as i32 * cw16() / 16;
        let view = a.w - 2 * PAD - 2;
        if x < self.scroll_x {
            self.scroll_x = (x - view / 4).max(0);
        } else if x > self.scroll_x + view {
            self.scroll_x = x - view + view / 4;
        }
        self.clamp_scroll();
    }

    fn content_width(&self) -> i32 {
        let cols = self
            .lines
            .iter()
            .map(|l| visual_col(l, l.len()))
            .max()
            .unwrap_or(0);
        cols as i32 * cw16() / 16 + 2 * PAD + 16
    }

    fn clamp_scroll(&mut self) {
        let max_y = self.lines.len().saturating_sub(rows());
        self.scroll_y = self.scroll_y.min(max_y);
        let max_x = (self.content_width() - area().w).max(0);
        self.scroll_x = self.scroll_x.clamp(0, max_x);
    }

    /// The text position under a point.
    fn pos_at(&self, x: i32, y: i32) -> Pos {
        let a = area();
        let row = ((y - a.y - 4).max(0) / LINE_H) as usize;
        let line = (self.scroll_y + row).min(self.lines.len() - 1);
        let px = x - a.x - PAD + self.scroll_x;
        let cw = cw16();
        let v = ((px * 16 + cw / 2).max(0) / cw) as usize;
        Pos {
            line,
            col: col_at(&self.lines[line], v),
        }
    }

    fn select_word(&mut self, p: Pos) {
        let l = &self.lines[p.line];
        let (mut a, mut b) = (p.col, p.col);
        if a < l.len() && is_word(l[a]) || a > 0 && is_word(l[a - 1]) {
            while a > 0 && is_word(l[a - 1]) {
                a -= 1;
            }
            while b < l.len() && is_word(l[b]) {
                b += 1;
            }
        } else if b < l.len() {
            b += 1;
        }
        self.anchor = Some(Pos {
            line: p.line,
            col: a,
        });
        self.cur = Pos {
            line: p.line,
            col: b,
        };
    }

    // ---- input ----------------------------------------------------------------

    pub fn on_key(&mut self, key: Key) -> bool {
        if self.dialog.is_some() {
            return self.dialog_key(key);
        }
        if self.menu.is_some() {
            if let Key::Escape = key {
                self.menu = None;
                return true;
            }
        }
        let ctrl = keyboard::ctrl_held();
        match key {
            Key::Char(c) if !c.is_control() || c == '\t' => {
                if c == ' ' {
                    // each word is its own undo step
                    self.last_edit = EditKind::None;
                }
                self.edit(EditKind::Typing);
                let mut buf = [0u8; 4];
                self.insert(c.encode_utf8(&mut buf));
            }
            Key::Enter => {
                self.edit(EditKind::Other);
                self.insert("\n");
            }
            Key::Backspace => {
                if self.selection().is_some() {
                    self.edit(EditKind::Other);
                    self.delete_selection();
                } else if self.cur != Pos::default() {
                    self.edit(EditKind::Deleting);
                    let b = self.cur;
                    let a = if b.col > 0 {
                        Pos {
                            line: b.line,
                            col: b.col - 1,
                        }
                    } else {
                        Pos {
                            line: b.line - 1,
                            col: self.lines[b.line - 1].len(),
                        }
                    };
                    self.remove(a, b);
                }
            }
            Key::Delete => {
                if self.selection().is_some() {
                    self.edit(EditKind::Other);
                    self.delete_selection();
                } else {
                    let a = self.cur;
                    let b = if a.col < self.lines[a.line].len() {
                        Pos {
                            line: a.line,
                            col: a.col + 1,
                        }
                    } else if a.line + 1 < self.lines.len() {
                        Pos {
                            line: a.line + 1,
                            col: 0,
                        }
                    } else {
                        return false;
                    };
                    self.edit(EditKind::Deleting);
                    self.remove(a, b);
                }
            }
            Key::Left => {
                let to = match self.selection() {
                    Some((a, _)) if !keyboard::shift_held() => a,
                    _ if ctrl => self.word_left(),
                    _ if self.cur.col > 0 => Pos {
                        line: self.cur.line,
                        col: self.cur.col - 1,
                    },
                    _ if self.cur.line > 0 => Pos {
                        line: self.cur.line - 1,
                        col: self.lines[self.cur.line - 1].len(),
                    },
                    _ => self.cur,
                };
                self.move_to(to, false);
            }
            Key::Right => {
                let len = self.lines[self.cur.line].len();
                let to = match self.selection() {
                    Some((_, b)) if !keyboard::shift_held() => b,
                    _ if ctrl => self.word_right(),
                    _ if self.cur.col < len => Pos {
                        line: self.cur.line,
                        col: self.cur.col + 1,
                    },
                    _ if self.cur.line + 1 < self.lines.len() => Pos {
                        line: self.cur.line + 1,
                        col: 0,
                    },
                    _ => self.cur,
                };
                self.move_to(to, false);
            }
            Key::Up => self.vertical(-1),
            Key::Down => self.vertical(1),
            Key::PageUp => self.vertical(-(rows() as isize - 1)),
            Key::PageDown => self.vertical(rows() as isize - 1),
            Key::Home if ctrl => self.move_to(Pos::default(), false),
            Key::End if ctrl => {
                let last = self.lines.len() - 1;
                let end = Pos {
                    line: last,
                    col: self.lines[last].len(),
                };
                self.move_to(end, false);
            }
            Key::Home => {
                // first to the indent, then to the very start
                let l = &self.lines[self.cur.line];
                let indent = l.iter().take_while(|c| c.is_whitespace()).count();
                let col = if self.cur.col == indent { 0 } else { indent };
                self.move_to(Pos { col, ..self.cur }, false);
            }
            Key::End => {
                let col = self.lines[self.cur.line].len();
                self.move_to(Pos { col, ..self.cur }, false);
            }
            Key::Ctrl('s') if keyboard::shift_held() => self.run(Cmd::SaveAs),
            Key::Ctrl(c) => {
                let cmd = match c {
                    'n' => Cmd::New,
                    'o' => Cmd::Open,
                    's' => Cmd::Save,
                    'z' => Cmd::Undo,
                    'y' => Cmd::Redo,
                    'x' => Cmd::Cut,
                    'c' => Cmd::Copy,
                    'v' => Cmd::Paste,
                    'a' => Cmd::SelectAll,
                    _ => return false,
                };
                self.run(cmd);
            }
            Key::Function(5) => self.run(Cmd::TimeDate),
            _ => return false,
        }
        self.scroll_to_cursor();
        true
    }

    fn dialog_key(&mut self, key: Key) -> bool {
        match self.dialog.as_mut() {
            Some(Dialog::File(d)) => {
                let ev = d.on_key(key, client());
                self.file_event(ev);
                true
            }
            Some(_) => match key {
                Key::Enter => {
                    self.dialog_button(0);
                    true
                }
                Key::Escape => {
                    let n = self.dialog_spec().map_or(1, |(_, _, b)| b.len());
                    self.dialog_button(n - 1);
                    true
                }
                _ => false,
            },
            None => false,
        }
    }

    fn file_event(&mut self, ev: filedialog::Event) {
        let Some(Dialog::File(d)) = &self.dialog else {
            return;
        };
        let mode = d.mode;
        match ev {
            filedialog::Event::None | filedialog::Event::Redraw => {}
            filedialog::Event::Cancel => {
                self.dialog = None;
                self.pending = None;
            }
            filedialog::Event::Chosen(path) => {
                self.dialog = None;
                match mode {
                    Mode::Open => self.load(&path),
                    Mode::Save => {
                        let same = self.path.as_deref() == Some(path.as_str());
                        if fs::exists(&path) && !same {
                            self.dialog = Some(Dialog::Replace(path));
                        } else if self.save_to(&path) {
                            self.saved();
                        }
                    }
                }
            }
        }
    }

    /// Title, text and buttons of a question or message.
    fn dialog_spec(&self) -> Option<(&'static str, Vec<String>, &'static [&'static str])> {
        match self.dialog.as_ref()? {
            Dialog::File(_) => None,
            Dialog::SaveChanges => {
                let mut line = String::from("Do you want to save changes to ");
                line.push_str(self.name());
                line.push('?');
                Some(("Text Editor", vec![line], &["Save", "Don't save", "Cancel"]))
            }
            Dialog::Replace(path) => {
                let mut line = String::from(fs::file_name(path));
                line.push_str(" already exists.");
                let lines = vec![line, String::from("Do you want to replace it?")];
                Some(("Confirm Save As", lines, &["Yes", "No"]))
            }
            Dialog::Message(m) => Some(("Text Editor", vec![String::from(*m)], &["OK"])),
        }
    }

    fn dialog_button(&mut self, i: usize) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        match (dialog, i) {
            (Dialog::SaveChanges, 0) => {
                if self.save() {
                    self.saved();
                }
            }
            (Dialog::SaveChanges, 1) => {
                if let Some(after) = self.pending.take() {
                    self.perform(after);
                }
            }
            (Dialog::Replace(path), 0) => {
                if self.save_to(&path) {
                    self.saved();
                }
            }
            (Dialog::Message(_), _) => {}
            _ => self.pending = None,
        }
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        if let Some(Dialog::File(d)) = &mut self.dialog {
            return d.on_wheel(clicks, client());
        }
        let old = self.scroll_y;
        self.scroll_y = (self.scroll_y as i32 + clicks * 3).max(0) as usize;
        self.clamp_scroll();
        old != self.scroll_y
    }

    fn menu_items(&self, kind: MenuKind) -> Vec<Item<'static>> {
        let sel = self.selection().is_some();
        match kind {
            MenuKind::File => vec![
                ("New", "Ctrl+N", true),
                ("Open...", "Ctrl+O", true),
                ("Save", "Ctrl+S", true),
                ("Save as...", "Ctrl+Shift+S", true),
                ("", "", false),
                ("Exit", "", true),
            ],
            MenuKind::Edit | MenuKind::Context => vec![
                ("Undo", "Ctrl+Z", !self.undo.is_empty()),
                ("Redo", "Ctrl+Y", !self.redo.is_empty()),
                ("", "", false),
                ("Cut", "Ctrl+X", sel),
                ("Copy", "Ctrl+C", sel),
                ("Paste", "Ctrl+V", !widgets::paste().is_empty()),
                ("Delete", "Del", sel),
                ("", "", false),
                ("Select all", "Ctrl+A", true),
                ("", "", false),
                ("Time/Date", "F5", true),
            ],
        }
    }

    fn menu_button(i: usize) -> Rect {
        Rect::new(6 + i as i32 * 52, 3, 48, MENU_H - 6)
    }

    fn open_menu(&mut self, kind: MenuKind, x: i32, y: i32) {
        let items = self.menu_items(kind);
        let mut r = widgets::menu_rect(x, y, &items);
        r.x = r.x.min(cw() - r.w - 4).max(0);
        if r.bottom() > ch() - 4 {
            r.y = (y - r.h).max(0);
        }
        self.menu = Some((kind, r));
        self.menu_hover = None;
    }

    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let Some((kind, r)) = self.menu else {
            return false;
        };
        let items = self.menu_items(kind);
        let hover = widgets::menu_item_at(r, &items, x, y);
        let changed = hover != self.menu_hover;
        self.menu_hover = hover;
        changed
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        let (x, y) = (ev.x, ev.y);
        match ev.kind {
            MouseKind::Down { right } => self.press(x, y, right),
            MouseKind::Move => match self.drag {
                Drag::Text => {
                    // scroll when dragging past the edges
                    let a = area();
                    if y < a.y && self.scroll_y > 0 {
                        self.scroll_y -= 1;
                    } else if y >= a.bottom() {
                        self.scroll_y += 1;
                    }
                    let p = self.pos_at(x, y.clamp(a.y, a.bottom() - 1));
                    self.cur = p;
                    self.clamp_scroll();
                    true
                }
                Drag::VThumb(grab) => {
                    let total = self.lines.len() as i32;
                    let s = widgets::thumb_drag(vtrack(), true, total, rows() as i32, y, grab);
                    self.scroll_y = s as usize;
                    self.clamp_scroll();
                    true
                }
                Drag::HThumb(grab) => {
                    let total = self.content_width();
                    self.scroll_x = widgets::thumb_drag(htrack(), false, total, area().w, x, grab);
                    self.clamp_scroll();
                    true
                }
                Drag::None => false,
            },
            MouseKind::Up => {
                self.drag = Drag::None;
                false
            }
        }
    }

    fn press(&mut self, x: i32, y: i32, right: bool) -> bool {
        if self.dialog.is_some() {
            if right {
                return false;
            }
            if let Some(Dialog::File(d)) = &mut self.dialog {
                let ev = d.on_click(client(), x, y);
                self.file_event(ev);
                return true;
            }
            if let Some((_, lines, buttons)) = self.dialog_spec() {
                let lines: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
                let r = widgets::message_rect(client(), &lines, buttons);
                let rects = widgets::message_buttons(r, buttons.len());
                if let Some(i) = rects.iter().position(|b| b.contains(x, y)) {
                    self.dialog_button(i);
                    return true;
                }
            }
            return false;
        }
        if let Some((kind, r)) = self.menu.take() {
            let items = self.menu_items(kind);
            if let Some(i) = widgets::menu_item_at(r, &items, x, y) {
                let cmds: &[Option<Cmd>] = if kind == MenuKind::File {
                    &FILE_CMDS
                } else {
                    &EDIT_CMDS
                };
                if let Some(cmd) = cmds[i] {
                    self.run(cmd);
                }
                return true;
            }
            // a click on the open menu's own button just closes it
            let own = match kind {
                MenuKind::File => Some(0),
                MenuKind::Edit => Some(1),
                MenuKind::Context => None,
            };
            if own.is_some_and(|i| Self::menu_button(i).contains(x, y)) || r.contains(x, y) {
                return true;
            }
        }
        if y < MENU_H {
            if right {
                return false;
            }
            for (i, kind) in [MenuKind::File, MenuKind::Edit].into_iter().enumerate() {
                let b = Self::menu_button(i);
                if b.contains(x, y) {
                    self.open_menu(kind, b.x, b.bottom() + 2);
                }
            }
            return true;
        }
        let a = area();
        if vtrack().contains(x, y) && !right {
            let total = self.lines.len() as i32;
            let t = widgets::thumb(vtrack(), true, total, rows() as i32, self.scroll_y as i32);
            if t.contains(x, y) {
                self.drag = Drag::VThumb(y - t.y);
            } else {
                let page = rows() - 1;
                self.scroll_y = if y < t.y {
                    self.scroll_y.saturating_sub(page)
                } else {
                    self.scroll_y + page
                };
                self.clamp_scroll();
            }
            return true;
        }
        if htrack().contains(x, y) && !right {
            let total = self.content_width();
            let t = widgets::thumb(htrack(), false, total, a.w, self.scroll_x);
            if t.contains(x, y) {
                self.drag = Drag::HThumb(x - t.x);
            } else {
                let step = if x < t.x { -a.w / 2 } else { a.w / 2 };
                self.scroll_x += step;
                self.clamp_scroll();
            }
            return true;
        }
        if !a.contains(x, y) {
            return false;
        }
        let p = self.pos_at(x, y);
        if right {
            // keep the selection if the click is in it
            let inside = self.selection().is_some_and(|(s, e)| p >= s && p <= e);
            if !inside {
                self.anchor = None;
                self.cur = p;
            }
            self.open_menu(MenuKind::Context, x, y);
            return true;
        }
        let now = interrupts::ticks();
        if self.last_click.1 == p && now - self.last_click.0 <= DOUBLE_CLICK {
            self.select_word(p);
            self.last_click = (0, Pos::default());
            return true;
        }
        self.last_click = (now, p);
        self.move_to(p, false);
        if !keyboard::shift_held() {
            self.anchor = Some(p);
        }
        self.drag = Drag::Text;
        true
    }

    // ---- drawing --------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, caret: bool) {
        let bg = theme::light();
        c.fill(client(), bg);
        self.draw_menu_bar(c);
        self.draw_text(c, caret && self.dialog.is_none() && self.menu.is_none());
        self.draw_status(c);

        let total = self.lines.len() as i32;
        widgets::draw_scrollbar(
            c,
            vtrack(),
            true,
            total,
            rows() as i32,
            self.scroll_y as i32,
        );
        let width = self.content_width();
        let a = area();
        widgets::draw_scrollbar(c, htrack(), false, width, a.w, self.scroll_x);
        c.fill(Rect::new(a.right(), a.bottom(), SB, SB), theme::track());

        if let Some((kind, r)) = self.menu {
            let items = self.menu_items(kind);
            widgets::draw_menu(c, r, &items, self.menu_hover);
        }
        if let Some(Dialog::File(d)) = &mut self.dialog {
            d.draw(c, client(), caret);
        } else if let Some((title, lines, buttons)) = self.dialog_spec() {
            let lines: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
            widgets::draw_message(c, client(), title, &lines, buttons, None);
        }
    }

    fn draw_menu_bar(&self, c: &mut Canvas) {
        let bar = Rect::new(0, 0, cw(), MENU_H);
        c.fill(bar, theme::face());
        c.fill_rect(0, MENU_H - 1, cw(), 1, theme::stroke());
        for (i, label) in ["File", "Edit"].into_iter().enumerate() {
            let b = Self::menu_button(i);
            let open = match self.menu {
                Some((MenuKind::File, _)) => i == 0,
                Some((MenuKind::Edit, _)) => i == 1,
                _ => false,
            };
            if open {
                c.fill_round(b, 4, theme::hover());
            }
            c.text_centered(b, label, theme::text());
        }
    }

    fn draw_text(&self, c: &mut Canvas, caret: bool) {
        let a = area();
        let mut t = c.sub(Rect::new(0, 0, c.width, c.height));
        t.clip_to(a);
        let cw = cw16();
        let x0 = a.x + PAD - self.scroll_x;
        let sel = self.selection();
        let rows = rows() + 1;
        for (k, line) in self.lines.iter().enumerate().skip(self.scroll_y).take(rows) {
            let y = a.y + 4 + (k - self.scroll_y) as i32 * LINE_H;
            let x_of = |col: usize| x0 + visual_col(line, col) as i32 * cw / 16;
            if let Some((s, e)) = sel {
                if k >= s.line && k <= e.line {
                    let from = if k == s.line { s.col } else { 0 };
                    let to = if k == e.line { e.col } else { line.len() };
                    // the line end counts as a little space
                    let extra = if k < e.line { cw / 32 } else { 0 };
                    let (xa, xb) = (x_of(from), x_of(to) + extra);
                    t.fill(Rect::new(xa, y, xb - xa, LINE_H), theme::selection());
                }
            }
            let text_y = y + (LINE_H - MONO.line_height) / 2;
            let mut v = 0usize;
            for &ch in line {
                let x = x0 + v as i32 * cw / 16;
                let next = if ch == '\t' {
                    (v / TAB + 1) * TAB
                } else {
                    v + 1
                };
                v = next;
                if x > a.right() {
                    break;
                }
                if ch == '\t' || ch == ' ' || x + 16 < a.x {
                    continue;
                }
                if !t.draw_glyph(&MONO, x, text_y, ch, theme::text()) {
                    t.draw_glyph(&MONO, x, text_y, '?', theme::text_dim());
                }
            }
            if caret && k == self.cur.line {
                let x = x_of(self.cur.col);
                t.fill_rect(x, y + 1, 2, LINE_H - 2, theme::text());
            }
        }
    }

    fn draw_status(&self, c: &mut Canvas) {
        let r = Rect::new(0, ch() - STATUS_H, cw(), STATUS_H);
        c.fill(r, theme::face());
        c.fill_rect(0, r.y, cw(), 1, theme::stroke());
        let ty = r.y + (STATUS_H - UI.line_height) / 2;
        let mut s = String::new();
        let col = visual_col(&self.lines[self.cur.line], self.cur.col) + 1;
        let _ = write!(s, "Ln {}, Col {}", self.cur.line + 1, col);
        c.draw_text(r.x + 12, ty, &s, theme::text());
        s.clear();
        let chars: usize = self.lines.iter().map(|l| l.len()).sum::<usize>() + self.lines.len() - 1;
        let _ = write!(s, "{} characters", chars);
        c.draw_text(r.x + 170, ty, &s, theme::text());
        if fs::storage() != fs::Storage::Disk {
            let warn = "No disk: files are lost on restart";
            c.draw_text(r.x + 340, ty, warn, theme::warning());
        }
        let right = [
            self.encoding,
            if self.crlf {
                "Windows (CRLF)"
            } else {
                "Unix (LF)"
            },
        ];
        let mut x = r.right() - 16;
        for label in right {
            let w = UI.width(label);
            x -= w;
            c.draw_text(x, ty, label, theme::text());
            x -= 20;
            c.fill_rect(x + 10, r.y + 6, 1, STATUS_H - 12, theme::stroke());
        }
    }
}
