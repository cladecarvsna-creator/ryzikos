//! Telegram: a window in the style of Telegram Desktop. It asks for the
//! api_id and api_hash once (unless the build carries them), then shows a
//! QR code to scan with the phone, or asks for the phone number and the
//! code; then the two-step verification password if there is one. Signed
//! in, it shows the chat list on the left and the open chat on the right,
//! with a box to write in and a paper clip to send files.
//!
//! The client itself (crate::tg) runs in a fiber; this file only draws
//! what it shares and passes on what the user does.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::filedialog::{self, FileDialog, Mode};
use super::text::{Font, HEADING, TITLE, UI, UI_BOLD};
use super::widgets::{self, FieldEvent, TextField};
use super::{theme, MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::keyboard::Key;
use crate::tg::{self, ChatKind, Cmd, History, Message, Peer, Shared, Stage};
use crate::{fs, users};

pub const CLIENT_W: i32 = 1100;
pub const CLIENT_H: i32 = 720;

const LIST_W: i32 = 340;
const TOP_H: i32 = 56;
const ROW_H: i32 = 68;
const INPUT_H: i32 = 56;
const AVATAR: i32 = 50;
const LINE_H: i32 = 20;
const BUBBLE_MAX: i32 = 470;
const PAD_X: i32 = 12;
const PAD_Y: i32 = 7;

/// Telegram's colours for avatars and names in groups.
const PALETTE: [Color; 7] = [
    rgb(0xe1, 0x70, 0x76),
    rgb(0xfa, 0xa7, 0x74),
    rgb(0xa6, 0x95, 0xe7),
    rgb(0x7b, 0xc8, 0x62),
    rgb(0x6e, 0xc9, 0xcb),
    rgb(0x65, 0xaa, 0xdd),
    rgb(0xee, 0x7a, 0xae),
];
const BLUE: Color = rgb(0x41, 0x9f, 0xd9);

fn palette(id: i64) -> Color {
    PALETTE[(id.unsigned_abs() % 7) as usize]
}

// ---- colours for the light and the dark look --------------------------------------------

fn pick(light: Color, dark: Color) -> Color {
    if theme::dark() {
        dark
    } else {
        light
    }
}

fn panel() -> Color {
    pick(rgb(0xff, 0xff, 0xff), rgb(0x17, 0x21, 0x2b))
}

fn text() -> Color {
    pick(rgb(0x00, 0x00, 0x00), rgb(0xf5, 0xf5, 0xf5))
}

fn dim() -> Color {
    pick(rgb(0x70, 0x79, 0x81), rgb(0x70, 0x84, 0x99))
}

fn line() -> Color {
    pick(rgb(0xe7, 0xe7, 0xe7), rgb(0x0e, 0x16, 0x21))
}

fn hover() -> Color {
    pick(rgb(0xf1, 0xf1, 0xf1), rgb(0x20, 0x2b, 0x36))
}

fn selected() -> Color {
    pick(BLUE, rgb(0x2b, 0x52, 0x78))
}

fn bubble_in() -> Color {
    pick(rgb(0xff, 0xff, 0xff), rgb(0x18, 0x25, 0x33))
}

fn bubble_out() -> Color {
    pick(rgb(0xef, 0xfd, 0xde), rgb(0x2b, 0x52, 0x78))
}

fn time_in() -> Color {
    pick(rgb(0xa0, 0xac, 0xb6), rgb(0x6d, 0x7f, 0x8f))
}

fn time_out() -> Color {
    pick(rgb(0x6c, 0xb3, 0x5f), rgb(0x7d, 0xa8, 0xd3))
}

fn wall_top() -> Color {
    pick(rgb(0xd6, 0xe2, 0xb8), rgb(0x0e, 0x16, 0x21))
}

fn wall_bottom() -> Color {
    pick(rgb(0x9c, 0xc4, 0x98), rgb(0x0e, 0x16, 0x21))
}

fn pill() -> Color {
    pick(rgb(0x6f, 0x8f, 0x72), rgb(0x1e, 0x2c, 0x3a))
}

// ---- text -----------------------------------------------------------------------------

/// Make text drawable with our fonts: typographic quotes become plain
/// ones, and emoji and other signs the fonts don't have become a dot.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{2032}' => out.push('\''),
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{2033}' => out.push('"'),
            '\u{2010}'..='\u{2012}' | '\u{2212}' => out.push('-'),
            '\u{2116}' => out.push_str("No."),
            '\u{2192}' => out.push_str("->"),
            '\u{2190}' => out.push_str("<-"),
            // joiners, variation selectors and skin tones go with the emoji
            '\u{200b}'..='\u{200f}' | '\u{fe00}'..='\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}' => {}
            '\r' => {}
            '\n' | '\t' => out.push(c),
            c if UI.glyph(c).is_some() => out.push(c),
            _ => {
                if !out.ends_with('\u{2022}') {
                    out.push('\u{2022}');
                }
            }
        }
    }
    out
}

/// Split text into lines no wider than `max`.
fn wrap(f: &Font, s: &str, max: i32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        let mut width = 0;
        for word in para.split_inclusive(' ') {
            let w = f.width(word);
            if width + w <= max || line.is_empty() && w <= max {
                line.push_str(word);
                width += w;
                continue;
            }
            if !line.is_empty() {
                lines.push(core::mem::take(&mut line));
                width = 0;
            }
            if w <= max {
                line.push_str(word);
                width = w;
                continue;
            }
            // a word longer than a line: break it anywhere
            for c in word.chars() {
                let cw = f.width(c.encode_utf8(&mut [0; 4]));
                if width + cw > max && !line.is_empty() {
                    lines.push(core::mem::take(&mut line));
                    width = 0;
                }
                line.push(c);
                width += cw;
            }
        }
        lines.push(line);
    }
    lines
}

/// Cut text to fit `max` pixels, with "..." at the end.
fn fit(f: &Font, s: &str, max: i32) -> String {
    if f.width(s) <= max {
        return String::from(s);
    }
    let dots = f.width("\u{2026}");
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = f.width(c.encode_utf8(&mut [0; 4]));
        if w + cw + dots > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('\u{2026}');
    out
}

fn initials(title: &str) -> String {
    let mut out = String::new();
    for word in title.split_whitespace().take(2) {
        if let Some(c) = word.chars().find(|c| c.is_alphanumeric()) {
            out.extend(c.to_uppercase());
        }
    }
    if out.is_empty() {
        out.push('?');
    }
    out
}

// ---- time -------------------------------------------------------------------------------

/// (year, month, day, hour, minute, weekday 0 = Monday) of a local time.
fn civil(t: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = crate::rtc::civil_from_days(days);
    let weekday = (days + 3).rem_euclid(7);
    (y, m, d, secs / 3600, secs / 60 % 60, weekday)
}

fn clock(t: i64) -> String {
    let (_, _, _, h, m, _) = civil(t);
    alloc::format!("{:02}:{:02}", h, m)
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A time for the chat list: the clock today, the weekday this week,
/// the date before.
fn short_date(t: i64, now: i64) -> String {
    if t == 0 {
        return String::new();
    }
    let day = t.div_euclid(86400);
    let today = now.div_euclid(86400);
    let (y, m, d, _, _, wd) = civil(t);
    if day == today {
        clock(t)
    } else if today - day < 7 && day <= today {
        String::from(WEEKDAYS[wd as usize])
    } else {
        alloc::format!("{:02}.{:02}.{:02}", d, m, y % 100)
    }
}

fn long_date(t: i64, now: i64) -> String {
    let (y, m, d, _, _, _) = civil(t);
    let (ny, _, _, _, _, _) = civil(now);
    let month = MONTHS[(m as usize).clamp(1, 12) - 1];
    if y == ny {
        alloc::format!("{} {}", d, month)
    } else {
        alloc::format!("{} {} {}", d, month, y)
    }
}

// ---- the window ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    ApiId,
    ApiHash,
    Search,
    Input,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Next,
    Send,
    Menu,
    Back,
    /// QR code or phone number.
    Switch,
    Attach,
}

/// A message laid out for drawing.
struct Laid {
    /// Offset of its top from the top of everything, and its height.
    y: i32,
    h: i32,
    kind: LaidKind,
}

enum LaidKind {
    Date(String),
    Service(String),
    Bubble {
        index: usize,
        lines: Vec<String>,
        w: i32,
        /// The name above the text, in groups.
        name: Option<String>,
        /// The time sits on its own line under the text.
        time_below: bool,
    },
}

pub struct Telegram {
    shared: Rc<RefCell<Shared>>,
    fiber: Option<Fiber>,
    /// Stopped clients still finishing.
    draining: Vec<Fiber>,
    drawn: u64,
    api_id: TextField,
    api_hash: TextField,
    /// The phone number, the code or the password.
    form: TextField,
    form_stage: Stage,
    search: TextField,
    input: TextField,
    focus: Focus,
    open: Option<Peer>,
    /// The first chat row shown.
    list_top: i32,
    /// How far the chat is scrolled up from its newest message.
    scroll: i32,
    /// The height of the laid out chat last time, to keep the view still
    /// when messages arrive.
    content_h: i32,
    newest: i64,
    hover_row: Option<usize>,
    hover_button: Option<Button>,
    pressed: Option<Button>,
    menu: Option<Option<usize>>,
    /// Choosing a file to send.
    dialog: Option<FileDialog>,
    /// A note over the chat, like a file that could not be sent.
    note: Option<String>,
}

impl Telegram {
    pub fn new() -> Self {
        Self {
            shared: Rc::new(RefCell::new(Shared::new())),
            fiber: None,
            draining: Vec::new(),
            drawn: 0,
            api_id: TextField::default(),
            api_hash: TextField::default(),
            form: TextField::default(),
            form_stage: Stage::Starting,
            search: TextField::default(),
            input: TextField::default(),
            focus: Focus::ApiId,
            open: None,
            list_top: 0,
            scroll: 0,
            content_h: 0,
            newest: 0,
            hover_row: None,
            hover_button: None,
            pressed: None,
            menu: None,
            dialog: None,
            note: None,
        }
    }

    /// The window opened: start the client.
    pub fn start(&mut self) {
        if self.fiber.is_none() {
            crate::net::init();
            self.shared = Rc::new(RefCell::new(Shared::new()));
            self.open = None;
            let shared = self.shared.clone();
            self.fiber = Some(Fiber::new(move || tg::client::run(shared)));
            crate::serial::write_str("\ntelegram: started\n");
        }
    }

    /// The window closed: disconnect.
    pub fn stop(&mut self) {
        if let Some(mut f) = self.fiber.take() {
            f.cancel();
            self.draining.push(f);
        }
    }

    /// Let the client run a little. True when the window must be drawn
    /// again.
    pub fn tick(&mut self) -> bool {
        self.draining.retain_mut(|f| !f.resume());
        if let Some(f) = &mut self.fiber {
            if f.resume() {
                self.fiber = None;
            }
        }
        let version = self.shared.borrow().version;
        version != self.drawn
    }

    /// Whether the client waits for an answer the user is waiting for.
    pub fn busy(&self) -> bool {
        !self.draining.is_empty() || self.shared.borrow().busy
    }

    fn command(&mut self, cmd: Cmd) {
        self.shared.borrow_mut().commands.push_back(cmd);
    }

    fn stage(&self) -> Stage {
        self.shared.borrow().stage.clone()
    }

    // ---- places -----------------------------------------------------------------------

    fn card() -> Rect {
        Rect::new((CLIENT_W - 420) / 2, 70, 420, 520)
    }

    fn field_rect(i: i32) -> Rect {
        let c = Self::card();
        Rect::new(c.x + 40, c.y + 290 + i * 70, c.w - 80, 36)
    }

    fn next_rect(&self) -> Rect {
        let c = Self::card();
        let fields = if self.stage() == Stage::Config { 2 } else { 1 };
        Rect::new(c.x + 40, c.y + 290 + fields * 70 + 4, c.w - 80, 42)
    }

    fn back_rect(&self) -> Rect {
        let n = self.next_rect();
        Rect::new(n.x, n.bottom() + 12, n.w, 30)
    }

    /// "Log in by phone number" under the QR code.
    fn qr_switch_rect() -> Rect {
        let c = Self::card();
        Rect::new(c.x + 40, c.y + 440, c.w - 80, 30)
    }

    fn attach_rect() -> Rect {
        Rect::new(LIST_W + 10, CLIENT_H - INPUT_H + 8, 40, 40)
    }

    fn search_rect() -> Rect {
        Rect::new(58, 11, LIST_W - 72, 34)
    }

    fn menu_button() -> Rect {
        Rect::new(10, 10, 38, 36)
    }

    fn menu_items() -> [widgets::Item<'static>; 3] {
        [
            ("Reload chats", "", true),
            ("", "", false),
            ("Log out", "", true),
        ]
    }

    fn menu_rect() -> Rect {
        widgets::menu_rect(10, 50, &Self::menu_items())
    }

    fn chat_area() -> Rect {
        Rect::new(
            LIST_W + 1,
            TOP_H,
            CLIENT_W - LIST_W - 1,
            CLIENT_H - TOP_H - INPUT_H,
        )
    }

    fn input_rect() -> Rect {
        Rect::new(
            LIST_W + 56,
            CLIENT_H - INPUT_H + 10,
            CLIENT_W - LIST_W - 120,
            36,
        )
    }

    fn send_rect() -> Rect {
        Rect::new(CLIENT_W - 52, CLIENT_H - INPUT_H + 8, 40, 40)
    }

    fn list_rows() -> i32 {
        (CLIENT_H - TOP_H + ROW_H - 1) / ROW_H
    }

    /// The chats the search box lets through, as indices.
    fn visible_chats(&self) -> Vec<usize> {
        let s = self.shared.borrow();
        let q = self.search.string().to_lowercase();
        s.chats
            .iter()
            .enumerate()
            .filter(|(_, c)| q.is_empty() || c.title.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if x >= LIST_W || y < TOP_H {
            return None;
        }
        let i = (y - TOP_H) / ROW_H + self.list_top;
        let chats = self.visible_chats();
        chats.get(i as usize).copied()
    }

    // ---- input ------------------------------------------------------------------------

    pub fn on_key(&mut self, key: Key) -> bool {
        if let Some(d) = &mut self.dialog {
            let event = d.on_key(key, full());
            return self.dialog_event(event);
        }
        let stage = self.stage();
        match stage {
            Stage::Starting | Stage::Qr(_) => false,
            Stage::Config => {
                if matches!(key, Key::Char('\t')) {
                    self.focus = if self.focus == Focus::ApiId {
                        Focus::ApiHash
                    } else {
                        Focus::ApiId
                    };
                    return true;
                }
                let field = if self.focus == Focus::ApiHash {
                    &mut self.api_hash
                } else {
                    &mut self.api_id
                };
                match field.on_key(key) {
                    FieldEvent::Enter => {
                        if self.focus == Focus::ApiId {
                            self.focus = Focus::ApiHash;
                        } else {
                            self.submit();
                        }
                        true
                    }
                    FieldEvent::None => false,
                    _ => true,
                }
            }
            Stage::Phone | Stage::Code(_) | Stage::Password(_) => match self.form.on_key(key) {
                FieldEvent::Enter => {
                    self.submit();
                    true
                }
                FieldEvent::Escape if !matches!(stage, Stage::Phone) => {
                    self.command(Cmd::Phone(String::new()));
                    true
                }
                FieldEvent::None => false,
                _ => true,
            },
            Stage::Ready => self.ready_key(key),
        }
    }

    fn ready_key(&mut self, key: Key) -> bool {
        if self.menu.is_some() {
            self.menu = None;
            return true;
        }
        match key {
            Key::Escape if self.focus == Focus::Search && !self.search.text.is_empty() => {
                self.search.set("");
                self.list_top = 0;
                return true;
            }
            Key::Escape if self.open.is_some() => {
                self.open = None;
                self.shared.borrow_mut().open = None;
                return true;
            }
            Key::PageUp => return self.on_wheel(-8),
            Key::PageDown => return self.on_wheel(8),
            _ => {}
        }
        if self.focus == Focus::Search {
            let event = self.search.on_key(key);
            if event == FieldEvent::Enter {
                // open the first chat found
                if let Some(&i) = self.visible_chats().first() {
                    let peer = self.shared.borrow().chats[i].peer;
                    self.open_chat(peer);
                }
            }
            self.list_top = 0;
            return event != FieldEvent::None;
        }
        if self.open.is_none() {
            return false;
        }
        self.focus = Focus::Input;
        if matches!(key, Key::Ctrl('v')) && widgets::has_image() {
            self.paste_image();
            return true;
        }
        match self.input.on_key(key) {
            FieldEvent::Enter => {
                self.send();
                true
            }
            FieldEvent::None => false,
            _ => true,
        }
    }

    fn submit(&mut self) {
        let cmd = match self.stage() {
            Stage::Config => Cmd::Config {
                api_id: self.api_id.string(),
                api_hash: self.api_hash.string(),
            },
            Stage::Phone => Cmd::Phone(self.form.string()),
            Stage::Code(_) => Cmd::Code(self.form.string()),
            Stage::Password(_) => Cmd::Password(self.form.string()),
            _ => return,
        };
        let mut s = self.shared.borrow_mut();
        s.busy = true;
        s.error = None;
        s.commands.push_back(cmd);
        s.changed();
    }

    fn send(&mut self) {
        let text = self.input.string();
        let Some(peer) = self.open else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        self.input.set("");
        self.scroll = 0;
        self.command(Cmd::Send(peer, String::from(text.trim())));
    }

    fn dialog_event(&mut self, event: filedialog::Event) -> bool {
        match event {
            filedialog::Event::None => false,
            filedialog::Event::Redraw => true,
            filedialog::Event::Cancel => {
                self.dialog = None;
                true
            }
            filedialog::Event::Chosen(path) => {
                self.dialog = None;
                self.send_file(path);
                true
            }
        }
    }

    fn send_file(&mut self, path: String) {
        let Some(peer) = self.open else {
            return;
        };
        self.scroll = 0;
        self.note = None;
        self.command(Cmd::SendFile(peer, path));
    }

    /// Choose a file to send in the open chat.
    fn attach(&mut self) {
        let user = users::current_name().unwrap_or_default();
        let dir = fs::home(user.as_str());
        self.dialog = Some(FileDialog::new(Mode::Open, &dir, ""));
    }

    /// Ctrl+V with a picture on the clipboard: send it as a photo.
    fn paste_image(&mut self) {
        let Some(img) = widgets::paste_image() else {
            return;
        };
        if let Some(path) = img.path.filter(|p| fs::exists(p)) {
            self.send_file(path);
            return;
        }
        let user = users::current_name().unwrap_or_default();
        let dir = fs::app_data(user.as_str());
        let path = fs::join(&dir, &fs::unique_name(&dir, "Pasted picture", ".png"));
        let png = super::picture::encode_png(&img.pixels, img.w, img.h);
        match fs::write(&path, &png) {
            Ok(()) => self.send_file(path),
            Err(e) => self.note = Some(alloc::format!("Can't paste the picture: {}", e.message())),
        }
    }

    fn open_chat(&mut self, peer: Peer) {
        self.open = Some(peer);
        self.shared.borrow_mut().open = Some(peer);
        self.scroll = 0;
        self.content_h = 0;
        self.newest = 0;
        self.focus = Focus::Input;
        self.input.set("");
        self.command(Cmd::Open(peer));
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        let (x, y) = (ev.x, ev.y);
        if let Some(d) = &mut self.dialog {
            if let MouseKind::Down { right: false } = ev.kind {
                let event = d.on_click(full(), x, y);
                return self.dialog_event(event);
            }
            return false;
        }
        match ev.kind {
            MouseKind::Down { right: false } => {}
            MouseKind::Up => {
                let pressed = self.pressed.take();
                if pressed.is_some() && pressed == self.button_at(x, y) {
                    match pressed {
                        Some(Button::Next) => self.submit(),
                        Some(Button::Send) => self.send(),
                        Some(Button::Back) => self.command(Cmd::Phone(String::new())),
                        Some(Button::Switch) => {
                            let cmd = if self.stage() == Stage::Phone {
                                Cmd::UseQr
                            } else {
                                Cmd::UsePhone
                            };
                            self.command(cmd);
                        }
                        Some(Button::Attach) => self.attach(),
                        _ => {}
                    }
                }
                return pressed.is_some();
            }
            _ => return false,
        }
        if let Some(hover) = self.menu {
            self.menu = None;
            let items = Self::menu_items();
            match widgets::menu_item_at(Self::menu_rect(), &items, x, y).or(hover) {
                Some(0) => self.command(Cmd::Reload),
                Some(2) => {
                    self.open = None;
                    self.command(Cmd::LogOut);
                }
                _ => {}
            }
            return true;
        }
        let stage = self.stage();
        match stage {
            Stage::Config => {
                for (i, focus) in [(0, Focus::ApiId), (1, Focus::ApiHash)] {
                    let r = Self::field_rect(i);
                    if r.contains(x, y) {
                        self.focus = focus;
                        let field = if i == 0 {
                            &mut self.api_id
                        } else {
                            &mut self.api_hash
                        };
                        field.click(r, x);
                        return true;
                    }
                }
            }
            Stage::Phone | Stage::Code(_) | Stage::Password(_) => {
                let r = Self::field_rect(0);
                if r.contains(x, y) {
                    self.form.click(r, x);
                    return true;
                }
            }
            Stage::Ready => {
                if Self::menu_button().contains(x, y) {
                    self.menu = Some(None);
                    return true;
                }
                if Self::search_rect().contains(x, y) {
                    self.focus = Focus::Search;
                    self.search.click(Self::search_rect(), x);
                    return true;
                }
                if let Some(i) = self.row_at(x, y) {
                    let peer = self.shared.borrow().chats[i].peer;
                    self.open_chat(peer);
                    return true;
                }
                if Self::input_rect().contains(x, y) && self.open.is_some() {
                    self.focus = Focus::Input;
                    self.input.click(Self::input_rect(), x);
                    return true;
                }
                if self.open.is_some() && x > LIST_W {
                    self.focus = Focus::Input;
                }
            }
            Stage::Starting | Stage::Qr(_) => {}
        }
        if let Some(b) = self.button_at(x, y) {
            self.pressed = Some(b);
            return true;
        }
        true
    }

    fn button_at(&self, x: i32, y: i32) -> Option<Button> {
        match self.stage() {
            Stage::Config | Stage::Phone | Stage::Code(_) | Stage::Password(_) => {
                if self.next_rect().contains(x, y) {
                    return Some(Button::Next);
                }
                let back = !matches!(self.stage(), Stage::Config | Stage::Phone);
                if back && self.back_rect().contains(x, y) {
                    return Some(Button::Back);
                }
                if self.stage() == Stage::Phone && self.back_rect().contains(x, y) {
                    return Some(Button::Switch);
                }
                None
            }
            Stage::Qr(_) => Self::qr_switch_rect()
                .contains(x, y)
                .then_some(Button::Switch),
            Stage::Ready => {
                if self.open.is_some() && Self::send_rect().contains(x, y) {
                    Some(Button::Send)
                } else if self.open.is_some() && Self::attach_rect().contains(x, y) {
                    Some(Button::Attach)
                } else if Self::menu_button().contains(x, y) {
                    Some(Button::Menu)
                } else {
                    None
                }
            }
            Stage::Starting => None,
        }
    }

    pub fn on_wheel(&mut self, delta: i32) -> bool {
        if let Some(d) = &mut self.dialog {
            return d.on_wheel(delta, full());
        }
        if self.stage() != Stage::Ready {
            return false;
        }
        if self.hover_row.is_some() || self.open.is_none() {
            let n = self.visible_chats().len() as i32;
            let top =
                (self.list_top + delta.signum() * 2).clamp(0, (n - Self::list_rows() + 1).max(0));
            let changed = top != self.list_top;
            self.list_top = top;
            return changed;
        }
        // up (negative) goes back in time
        self.scroll = (self.scroll - delta * 3 * LINE_H).max(0);
        true
    }

    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let row = if self.stage() == Stage::Ready {
            self.row_at(x, y)
        } else {
            None
        };
        let button = self.button_at(x, y);
        let mut changed = row != self.hover_row || button != self.hover_button;
        // keep the list scrollable by the wheel when the mouse is on it
        if row.is_none() && x < LIST_W && y > TOP_H && self.hover_row.is_none() {
            changed = false;
        }
        self.hover_row = row.or(if x < LIST_W && y > TOP_H {
            Some(usize::MAX)
        } else {
            None
        });
        self.hover_button = button;
        if let Some(menu) = self.menu {
            let items = Self::menu_items();
            let h = widgets::menu_item_at(Self::menu_rect(), &items, x, y);
            if h != menu {
                self.menu = Some(h);
                changed = true;
            }
        }
        changed
    }

    // ---- drawing ------------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, focused: bool, caret: bool) {
        self.drawn = self.shared.borrow().version;
        let stage = self.stage();
        if stage != self.form_stage {
            // a new step: an empty box, ready to type in
            if !matches!(
                (&stage, &self.form_stage),
                (Stage::Code(_), Stage::Code(_)) | (Stage::Password(_), Stage::Password(_))
            ) {
                self.form.set("");
            }
            if stage == Stage::Config {
                self.focus = Focus::ApiId;
            }
            if stage == Stage::Ready && self.focus != Focus::Input {
                self.focus = Focus::Search;
            }
            self.form_stage = stage.clone();
        }
        match &stage {
            Stage::Ready => self.draw_main(c, focused && caret),
            Stage::Qr(link) => self.draw_qr(c, link),
            _ => self.draw_form(c, &stage, focused && caret),
        }
        if let Some(d) = &mut self.dialog {
            d.draw(c, full(), focused && caret);
        }
    }

    fn draw_qr(&mut self, c: &mut Canvas, link: &str) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, panel());
        let card = Self::card();
        let cx = CLIENT_W / 2;
        let bx = Rect::new(cx - 120, card.y, 240, 240);
        c.fill_round(bx, 12, rgb(0xff, 0xff, 0xff));
        if link.is_empty() || !draw_qr_code(c, bx, link) {
            c.text_centered(bx, "Getting a code...", rgb(0x70, 0x70, 0x78));
        } else {
            // Telegram's plane in the middle, as its own apps do
            let m = Rect::new(cx - 22, bx.y + 98, 44, 44);
            c.fill_round(m, 22, rgb(0xff, 0xff, 0xff));
            c.fill_round(Rect::new(m.x + 4, m.y + 4, 36, 36), 18, BLUE);
            draw_plane(c, m.x + 10, m.y + 12, 22, rgb(0xff, 0xff, 0xff));
        }
        let title = "Log in to Telegram by QR code";
        let tw = HEADING.width(title);
        c.draw_text_in(&HEADING, cx - tw / 2, card.y + 262, title, text());
        let steps = [
            "1. Open Telegram on your phone",
            "2. Go to Settings > Devices > Link Desktop Device",
            "3. Point your phone at this screen to confirm login",
        ];
        let left = cx - steps.iter().map(|l| UI.width(l)).max().unwrap_or(0) / 2;
        for (i, l) in steps.iter().enumerate() {
            c.draw_text(left, card.y + 312 + i as i32 * (LINE_H + 6), l, text());
        }
        let r = Self::qr_switch_rect();
        let face = if self.hover_button == Some(Button::Switch) {
            hover()
        } else {
            panel()
        };
        c.fill_round(r, 6, face);
        c.text_centered_in(&UI_BOLD, r, "Log in by phone number", BLUE);
        let error = self.shared.borrow().error.clone();
        if let Some(e) = error {
            let mut y = r.bottom() + 10;
            for l in wrap(&UI, &e, card.w) {
                let w = UI.width(&l);
                c.draw_text(cx - w / 2, y, &l, theme::error());
                y += LINE_H;
            }
        }
    }

    fn draw_form(&mut self, c: &mut Canvas, stage: &Stage, caret: bool) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, panel());
        let card = Self::card();
        let cx = CLIENT_W / 2;
        draw_logo(c, cx, card.y + 70, 56);
        let (title, lines): (&str, Vec<String>) = match stage {
            Stage::Starting => ("Telegram", alloc::vec![String::from("Connecting...")]),
            Stage::Config => (
                "Your Telegram app",
                alloc::vec![
                    String::from("RyzikOS needs an api_id and api_hash to talk to"),
                    String::from("Telegram. Get yours at my.telegram.org: API"),
                    String::from("development tools. They stay on this computer."),
                ],
            ),
            Stage::Phone => (
                "Your phone number",
                alloc::vec![
                    String::from("Enter your phone number with the country code."),
                    String::from("Telegram will send you a login code."),
                ],
            ),
            Stage::Code(hint) => ("Enter the code", wrap(&UI, hint, card.w - 60)),
            Stage::Password(hint) => {
                let mut l = alloc::vec![String::from("Your account is protected with a password.")];
                if !hint.is_empty() {
                    l.push(alloc::format!("Hint: {}", clean(hint)));
                }
                ("Two-step verification", l)
            }
            Stage::Ready | Stage::Qr(_) => return,
        };
        let tw = HEADING.width(title);
        c.draw_text_in(&HEADING, cx - tw / 2, card.y + 140, title, text());
        let mut y = card.y + 186;
        for l in &lines {
            let w = UI.width(l);
            c.draw_text(cx - w / 2, y, l, dim());
            y += LINE_H;
        }
        let (error, busy) = {
            let s = self.shared.borrow();
            (s.error.clone(), s.busy)
        };
        if *stage == Stage::Starting {
            if let Some(e) = error {
                let mut y = card.y + 290;
                for l in wrap(&UI, &e, card.w) {
                    let w = UI.width(&l);
                    c.draw_text(cx - w / 2, y, &l, theme::error());
                    y += LINE_H;
                }
            }
            return;
        }
        let labels: &[(&str, bool)] = match stage {
            Stage::Config => &[("api_id", false), ("api_hash", false)],
            Stage::Phone => &[("Phone number", false)],
            Stage::Code(_) => &[("Code", false)],
            _ => &[("Password", true)],
        };
        for (i, (label, secret)) in labels.iter().enumerate() {
            let r = Self::field_rect(i as i32);
            c.draw_text(r.x, r.y - 22, label, dim());
            let field = match (stage, i) {
                (Stage::Config, 0) => &mut self.api_id,
                (Stage::Config, _) => &mut self.api_hash,
                _ => &mut self.form,
            };
            let focused = match stage {
                Stage::Config => (i == 0) == (self.focus == Focus::ApiId),
                _ => true,
            };
            if *secret {
                // dots instead of the letters
                let mut shown = field.clone();
                shown.text = alloc::vec!['\u{2022}'; field.text.len()];
                shown.draw(c, r, focused, caret && focused);
            } else {
                field.draw(c, r, focused, caret && focused);
                if field.text.is_empty() && !focused {
                    c.draw_text(r.x + 8, r.y + 9, label, dim());
                }
            }
        }
        let next = self.next_rect();
        let label = if busy { "Please wait..." } else { "Next" };
        let face = if self.hover_button == Some(Button::Next) {
            mix(BLUE, rgb(0, 0, 0), 20)
        } else {
            BLUE
        };
        c.fill_round(next, 8, face);
        c.text_centered_in(&UI_BOLD, next, label, rgb(0xff, 0xff, 0xff));
        if !matches!(stage, Stage::Config | Stage::Phone) {
            let back = self.back_rect();
            c.text_centered(back, "Use a different number", BLUE);
        }
        if *stage == Stage::Phone {
            let back = self.back_rect();
            if self.hover_button == Some(Button::Switch) {
                c.fill_round(back, 6, hover());
            }
            c.text_centered_in(&UI_BOLD, back, "Log in by QR code", BLUE);
        }
        if let Some(e) = error {
            let mut y = self.back_rect().bottom() + 8;
            for l in wrap(&UI, &e, card.w) {
                let w = UI.width(&l);
                c.draw_text(cx - w / 2, y, &l, theme::error());
                y += LINE_H;
            }
        }
    }

    fn draw_main(&mut self, c: &mut Canvas, caret: bool) {
        c.fill_rect(0, 0, CLIENT_W, CLIENT_H, panel());
        self.draw_list(c, caret);
        c.fill_rect(LIST_W, 0, 1, CLIENT_H, line());
        match self.open {
            Some(peer) => self.draw_chat(c, peer, caret),
            None => {
                let area = Rect::new(LIST_W + 1, 0, CLIENT_W - LIST_W - 1, CLIENT_H);
                c.vertical_gradient(area, wall_top(), wall_bottom());
                let msg = "Select a chat to start messaging";
                let w = UI.width(msg) + 24;
                let r = Rect::new(area.x + (area.w - w) / 2, area.y + area.h / 2 - 14, w, 28);
                c.fill_round(r, 14, pill());
                c.text_centered(r, msg, rgb(0xff, 0xff, 0xff));
            }
        }
        if let Some(hover) = self.menu {
            let items = Self::menu_items();
            widgets::draw_menu(c, Self::menu_rect(), &items, hover);
        }
    }

    fn draw_list(&mut self, c: &mut Canvas, caret: bool) {
        // the menu button: three lines
        let m = Self::menu_button();
        if self.hover_button == Some(Button::Menu) {
            c.fill_round(m, 18, hover());
        }
        for i in 0..3 {
            c.fill_round(Rect::new(m.x + 10, m.y + 11 + i * 6, 18, 2), 1, dim());
        }
        let search_focused = self.focus == Focus::Search;
        let sr = Self::search_rect();
        c.fill_round(sr, 17, pick(rgb(0xf1, 0xf1, 0xf1), rgb(0x24, 0x2f, 0x3d)));
        if search_focused {
            c.outline_round(sr, 17, BLUE);
        }
        if self.search.text.is_empty() {
            c.draw_text(sr.x + 14, sr.y + 9, "Search", dim());
            if search_focused && caret {
                c.fill_rect(sr.x + 14, sr.y + 8, 1, 18, text());
            }
        } else {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_to(sr.inset(4));
            let s = clean(&self.search.string());
            let w = sub.draw_text(sr.x + 14, sr.y + 9, &s, text());
            if search_focused && caret {
                sub.fill_rect(sr.x + 15 + w, sr.y + 8, 1, 18, text());
            }
        }

        let (now, tz, online, error) = {
            let s = self.shared.borrow();
            (
                tg::mtproto::now_ms() / 1000,
                s.tz,
                s.online,
                s.error.clone(),
            )
        };
        let now = now + tz;
        let visible = self.visible_chats();
        let s = self.shared.borrow();
        let mut y = TOP_H;
        // a failed connection says why, not just "Connecting..."
        let status = match error {
            Some(e) => Some(e),
            None if !online => Some(String::from("Connecting...")),
            None => None,
        };
        if let Some(e) = status {
            let r = Rect::new(0, y, LIST_W, 30);
            c.fill(r, pick(rgb(0xff, 0xf4, 0xe0), rgb(0x2a, 0x2a, 0x1c)));
            c.draw_text(12, y + 7, &fit(&UI, &e, LIST_W - 24), dim());
            y += 30;
        }
        let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
        sub.clip_to(Rect::new(0, y, LIST_W, CLIENT_H - y));
        let c = &mut sub;
        for &i in visible.iter().skip(self.list_top as usize) {
            if y >= CLIENT_H {
                break;
            }
            let chat = &s.chats[i];
            let is_open = self.open == Some(chat.peer);
            let r = Rect::new(0, y, LIST_W, ROW_H);
            if is_open {
                c.fill(r, selected());
            } else if self.hover_row == Some(i) {
                c.fill(r, hover());
            }
            let (fg, sub_fg) = if is_open {
                (rgb(0xff, 0xff, 0xff), rgb(0xe8, 0xf2, 0xfa))
            } else {
                (text(), dim())
            };
            let id = match chat.peer {
                Peer::User(id) | Peer::Chat(id) | Peer::Channel(id) => id,
            };
            draw_avatar(
                c,
                10 + AVATAR / 2,
                y + ROW_H / 2,
                AVATAR / 2,
                &chat.title,
                id,
                chat.kind,
            );
            let tx = 10 + AVATAR + 12;
            let date = short_date(chat.date + tz, now);
            let dw = UI.width(&date);
            c.draw_text(LIST_W - 12 - dw, y + 13, &date, sub_fg);
            let mut name_x = tx;
            if chat.kind == ChatKind::Channel || chat.kind == ChatKind::Group {
                draw_group_mark(c, tx, y + 14, fg, chat.kind == ChatKind::Channel);
                name_x += 20;
            }
            let title = fit(&UI_BOLD, &clean(&chat.title), LIST_W - 12 - dw - 8 - name_x);
            c.draw_text_in(&UI_BOLD, name_x, y + 12, &title, fg);
            // the last message and the unread count
            let mut right = LIST_W - 12;
            if chat.unread > 0 {
                let n = if chat.unread > 999 {
                    String::from("999+")
                } else {
                    alloc::format!("{}", chat.unread)
                };
                let w = (UI_BOLD.width(&n) + 14).max(22);
                let badge = Rect::new(right - w, y + 36, w, 22);
                let color = if is_open {
                    rgb(0xff, 0xff, 0xff)
                } else {
                    pick(rgb(0x4f, 0xae, 0x4e), rgb(0x3e, 0x88, 0xc7))
                };
                c.fill_round(badge, 11, color);
                c.text_centered_in(
                    &UI_BOLD,
                    badge,
                    &n,
                    if is_open { BLUE } else { rgb(0xff, 0xff, 0xff) },
                );
                right -= w + 6;
            }
            let mut px = tx;
            if chat.last_out && chat.kind != ChatKind::Saved {
                let you = "You: ";
                px += c.draw_text(px, y + 38, you, if is_open { fg } else { BLUE });
            }
            let last = fit(&UI, &clean(&chat.last), right - px);
            c.draw_text(px, y + 38, &last, sub_fg);
            y += ROW_H;
        }
        if visible.is_empty() {
            let msg = if s.chats.is_empty() {
                "Loading chats..."
            } else {
                "No chats found"
            };
            let w = UI.width(msg);
            c.draw_text((LIST_W - w) / 2, TOP_H + 40, msg, dim());
        }
    }

    fn draw_chat(&mut self, c: &mut Canvas, peer: Peer, caret: bool) {
        let s = self.shared.borrow();
        let (title, kind) = s
            .chat(peer)
            .map(|c| (c.title.clone(), c.kind))
            .unwrap_or((String::new(), ChatKind::Private));
        let read_out = s.chat(peer).map_or(0, |c| c.read_out);
        let tz = s.tz;

        // the header
        let head = Rect::new(LIST_W + 1, 0, CLIENT_W - LIST_W - 1, TOP_H);
        c.fill(head, panel());
        c.draw_text_in(
            &UI_BOLD,
            head.x + 20,
            10,
            &fit(&UI_BOLD, &clean(&title), head.w - 40),
            text(),
        );
        let subtitle = match kind {
            ChatKind::Saved => "your cloud storage",
            ChatKind::Bot => "bot",
            ChatKind::Group => "group",
            ChatKind::Channel => "channel",
            ChatKind::Private => "private chat",
        };
        c.draw_text(head.x + 20, 30, subtitle, dim());
        c.fill_rect(head.x, TOP_H - 1, head.w, 1, line());

        // the messages
        let area = Self::chat_area();
        c.vertical_gradient(area, wall_top(), wall_bottom());
        let empty = History::default();
        let h = s.history.get(&peer).unwrap_or(&empty);
        let group = matches!(kind, ChatKind::Group);
        let max_w = BUBBLE_MAX.min(area.w * 7 / 10);
        let laid = layout(&h.messages, group, max_w, tz);
        let total = laid.last().map_or(0, |l| l.y + l.h) + 12;
        // keep the view still when new messages come while scrolled up
        let newest = h.messages.last().map_or(0, |m| m.id.max(m.date));
        if self.content_h != 0 && self.scroll > 0 && newest != self.newest && total > self.content_h
        {
            self.scroll += total - self.content_h;
        }
        self.content_h = total;
        self.newest = newest;
        let view = area.h;
        self.scroll = self.scroll.min((total - view).max(0));
        let wants_older = !h.complete && !h.loading && self.scroll + view + 200 > total;
        // everything is laid out top down; the bottom of it sits at the
        // bottom of the area, moved down by the scroll
        let base = area.bottom() - total + self.scroll;
        {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_to(area);
            let c = &mut sub;
            if h.loading && h.messages.is_empty() {
                draw_pill(c, area.x + area.w / 2, area.y + area.h / 2, "Loading...");
            } else if h.messages.is_empty() && h.complete {
                draw_pill(
                    c,
                    area.x + area.w / 2,
                    area.y + area.h / 2,
                    "No messages here yet",
                );
            }
            for l in &laid {
                let y = base + l.y;
                if y + l.h < area.y || y > area.bottom() {
                    continue;
                }
                match &l.kind {
                    LaidKind::Date(d) => draw_pill(c, area.x + area.w / 2, y + 14, d),
                    LaidKind::Service(t) => draw_pill(c, area.x + area.w / 2, y + 14, t),
                    LaidKind::Bubble {
                        index,
                        lines,
                        w,
                        name,
                        time_below,
                    } => {
                        let m = &h.messages[*index];
                        let x = if m.out {
                            area.right() - 16 - w
                        } else {
                            area.x + 16
                        };
                        draw_bubble(
                            c,
                            m,
                            x,
                            y,
                            *w,
                            l.h,
                            lines,
                            name.as_deref(),
                            *time_below,
                            read_out,
                            tz,
                        );
                    }
                }
            }
            if h.loading && !h.messages.is_empty() {
                draw_pill(c, area.x + area.w / 2, area.y + 20, "Loading...");
            }
        }
        drop(s);
        if wants_older {
            self.command(Cmd::Older(peer));
        }

        // the box to write in
        let bar = Rect::new(
            LIST_W + 1,
            CLIENT_H - INPUT_H,
            CLIENT_W - LIST_W - 1,
            INPUT_H,
        );
        c.fill(bar, panel());
        c.fill_rect(bar.x, bar.y, bar.w, 1, line());
        let ir = Self::input_rect();
        let focused = self.focus == Focus::Input;
        {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_to(ir);
            if self.input.text.is_empty() {
                sub.draw_text(ir.x + 4, ir.y + 9, "Write a message...", dim());
                if focused && caret {
                    sub.fill_rect(ir.x + 4, ir.y + 8, 1, 18, text());
                }
            } else {
                // the end of long text stays in view
                let shown = clean(&self.input.string());
                let w = UI.width(&shown);
                let x = ir.x + 4 - (w - (ir.w - 12)).max(0);
                sub.draw_text(x, ir.y + 9, &shown, text());
                if focused && caret {
                    let cw = UI.width(&clean(
                        &self.input.text[..self.input.cursor]
                            .iter()
                            .collect::<String>(),
                    ));
                    sub.fill_rect(x + cw, ir.y + 8, 1, 18, text());
                }
            }
        }
        let ar = Self::attach_rect();
        if self.hover_button == Some(Button::Attach) {
            c.fill_round(ar, 20, hover());
        }
        draw_clip(c, ar.x + 12, ar.y + 8, dim());
        if let Some(n) = self.note.clone() {
            let area = Self::chat_area();
            draw_pill(c, area.x + area.w / 2, area.bottom() - 22, &n);
        }
        let sr = Self::send_rect();
        let active = !self.input.text.is_empty();
        let color = if active { BLUE } else { dim() };
        if self.hover_button == Some(Button::Send) {
            c.fill_round(sr, 20, hover());
        }
        draw_plane(c, sr.x + 8, sr.y + 10, 24, color);
    }
}

/// Lay out the messages top down, with date lines between days.
fn layout(messages: &[Message], group: bool, max_w: i32, tz: i64) -> Vec<Laid> {
    let mut out = Vec::new();
    let mut y = 8;
    let mut last_day = i64::MIN;
    let mut last_from = i64::MIN;
    let now = tg::mtproto::now_ms() / 1000 + tz;
    for (i, m) in messages.iter().enumerate() {
        let day = (m.date + tz).div_euclid(86400);
        if day != last_day {
            last_day = day;
            last_from = i64::MIN;
            out.push(Laid {
                y,
                h: 28,
                kind: LaidKind::Date(long_date(m.date + tz, now)),
            });
            y += 36;
        }
        if m.service {
            out.push(Laid {
                y,
                h: 28,
                kind: LaidKind::Service(fit(&UI, &clean(&m.text), max_w)),
            });
            y += 36;
            last_from = i64::MIN;
            continue;
        }
        let mut text = String::new();
        if let Some(media) = &m.media {
            text.push('[');
            text.push_str(media);
            text.push(']');
            if !m.text.is_empty() {
                text.push('\n');
            }
        }
        text.push_str(&m.text);
        let text = clean(&text);
        let inner = max_w - 2 * PAD_X;
        let lines = wrap(&UI, &text, inner);
        let name = (group && !m.out && m.from_id != last_from).then(|| clean(&m.from));
        let time_w = time_width(m);
        let widest = lines.iter().map(|l| UI.width(l)).max().unwrap_or(0);
        let last_w = lines.last().map_or(0, |l| UI.width(l));
        let time_below = last_w + 12 + time_w > inner;
        let mut w = widest.max(if time_below {
            time_w
        } else {
            last_w + 12 + time_w
        });
        if let Some(n) = &name {
            w = w.max(UI_BOLD.width(n).min(inner));
        }
        let w = w + 2 * PAD_X;
        let mut h = lines.len() as i32 * LINE_H + 2 * PAD_Y;
        if time_below {
            h += LINE_H - 4;
        }
        if name.is_some() {
            h += LINE_H;
        }
        // messages in a row from the same person sit closer
        let gap = if m.from_id == last_from { 4 } else { 10 };
        last_from = m.from_id;
        out.push(Laid {
            y,
            h,
            kind: LaidKind::Bubble {
                index: i,
                lines,
                w,
                name,
                time_below,
            },
        });
        y += h + gap;
    }
    out
}

fn time_width(m: &Message) -> i32 {
    let mut w = UI.width("00:00");
    if m.edited {
        w += UI.width("edited ");
    }
    if m.out {
        w += 20;
    }
    w
}

#[allow(clippy::too_many_arguments)]
fn draw_bubble(
    c: &mut Canvas,
    m: &Message,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    lines: &[String],
    name: Option<&str>,
    time_below: bool,
    read_out: i64,
    tz: i64,
) {
    let r = Rect::new(x, y, w, h);
    let face = if m.out { bubble_out() } else { bubble_in() };
    c.shadow(r, 10, 2, 1, 30);
    c.fill_round(r, 10, face);
    let mut ty = y + PAD_Y;
    if let Some(n) = name {
        let n = fit(&UI_BOLD, n, w - 2 * PAD_X);
        c.draw_text_in(&UI_BOLD, x + PAD_X, ty, &n, palette(m.from_id));
        ty += LINE_H;
    }
    let body = if m.failed { theme::error() } else { text() };
    for (i, l) in lines.iter().enumerate() {
        // the [Photo] line of a message with something attached
        let color = if i == 0 && m.media.is_some() && l.starts_with('[') {
            if m.out {
                time_out()
            } else {
                BLUE
            }
        } else {
            body
        };
        c.draw_text(x + PAD_X, ty, l, color);
        ty += LINE_H;
    }
    // the time, and ticks for our messages
    let tcolor = if m.out { time_out() } else { time_in() };
    let mut time = String::new();
    if m.edited {
        time.push_str("edited ");
    }
    time.push_str(&clock(m.date + tz));
    let tw = UI.width(&time) + if m.out { 20 } else { 0 };
    let tx = x + w - PAD_X - tw;
    let tyy = if time_below { ty - 2 } else { ty - LINE_H };
    c.draw_text(tx, tyy, &time, tcolor);
    if m.out {
        let cx = x + w - PAD_X - 16;
        let cy = tyy + 5;
        if m.failed {
            c.draw_text_in(&UI_BOLD, cx + 4, tyy, "!", theme::error());
        } else if m.id == 0 {
            // a small clock: still sending
            c.outline_round(Rect::new(cx + 2, cy, 11, 11), 5, tcolor);
            c.fill_rect(cx + 7, cy + 2, 1, 4, tcolor);
            c.fill_rect(cx + 7, cy + 5, 3, 1, tcolor);
        } else {
            draw_tick(c, cx, cy + 1, tcolor);
            if m.id <= read_out {
                draw_tick(c, cx + 5, cy + 1, tcolor);
            }
        }
    }
}

fn draw_tick(c: &mut Canvas, x: i32, y: i32, color: Color) {
    for d in 0..2 {
        c.line(x, y + 5 + d, x + 3, y + 8 + d, color);
        c.line(x + 3, y + 8 + d, x + 10, y + d, color);
    }
}

fn draw_pill(c: &mut Canvas, cx: i32, cy: i32, text: &str) {
    let w = UI.width(text) + 20;
    let r = Rect::new(cx - w / 2, cy - 12, w, 24);
    c.fill_round(r, 12, pill());
    c.text_centered(r, text, rgb(0xff, 0xff, 0xff));
}

fn draw_avatar(
    c: &mut Canvas,
    cx: i32,
    cy: i32,
    radius: i32,
    title: &str,
    id: i64,
    kind: ChatKind,
) {
    let r = Rect::new(cx - radius, cy - radius, 2 * radius, 2 * radius);
    if kind == ChatKind::Saved {
        c.fill_round(r, radius, BLUE);
        // a bookmark
        let (w, h) = (radius * 7 / 10, radius);
        let (x, y) = (cx - w / 2, cy - h / 2);
        c.fill_polygon(
            &[
                (x, y),
                (x + w, y),
                (x + w, y + h),
                (x + w / 2, y + h * 7 / 10),
                (x, y + h),
            ],
            rgb(0xff, 0xff, 0xff),
        );
        return;
    }
    c.fill_round(r, radius, palette(id));
    let letters = initials(&clean(title));
    c.text_centered_in(&TITLE, r, &letters, rgb(0xff, 0xff, 0xff));
}

/// Two heads for groups, a loudspeaker for channels, before the name.
fn draw_group_mark(c: &mut Canvas, x: i32, y: i32, color: Color, channel: bool) {
    if channel {
        c.fill_polygon(
            &[
                (x, y + 5),
                (x + 5, y + 5),
                (x + 12, y),
                (x + 12, y + 14),
                (x + 5, y + 9),
                (x, y + 9),
            ],
            color,
        );
    } else {
        c.fill_round(Rect::new(x + 1, y + 1, 6, 6), 3, color);
        c.fill_round(Rect::new(x, y + 8, 8, 6), 3, color);
        c.fill_round(Rect::new(x + 8, y + 1, 6, 6), 3, color);
        c.fill_round(Rect::new(x + 7, y + 8, 8, 6), 3, color);
    }
}

/// The paper plane, `s` pixels wide, at (x, y).
fn draw_plane(c: &mut Canvas, x: i32, y: i32, s: i32, color: Color) {
    let p = |fx: i32, fy: i32| (x + fx * s / 100, y + fy * s / 100);
    c.fill_polygon(
        &[
            p(0, 40),
            p(100, 0),
            p(80, 90),
            p(45, 62),
            p(35, 85),
            p(30, 55),
        ],
        color,
    );
    c.fill_polygon(
        &[p(30, 55), p(90, 8), p(45, 62)],
        mix(color, rgb(0, 0, 0), 40),
    );
}

/// The Telegram logo: a white plane on a blue circle.
fn draw_logo(c: &mut Canvas, cx: i32, cy: i32, radius: i32) {
    let r = Rect::new(cx - radius, cy - radius, 2 * radius, 2 * radius);
    c.fill_round(r, radius, rgb(0x2a, 0xa3, 0xd8));
    let s = radius;
    let (x, y) = (cx - s / 2 - s / 12, cy - s / 3);
    let p = |fx: i32, fy: i32| (x + fx * s / 100, y + fy * s / 100);
    let white = rgb(0xff, 0xff, 0xff);
    c.fill_polygon(
        &[
            p(0, 44),
            p(92, 8),
            p(76, 88),
            p(48, 66),
            p(36, 84),
            p(33, 58),
        ],
        white,
    );
    c.fill_polygon(&[p(33, 58), p(82, 18), p(48, 66)], rgb(0xc8, 0xda, 0xea));
}

fn full() -> Rect {
    Rect::new(0, 0, CLIENT_W, CLIENT_H)
}

/// A paper clip, 16 wide and 24 high.
fn draw_clip(c: &mut Canvas, x: i32, y: i32, color: Color) {
    c.outline_round(Rect::new(x, y, 14, 24), 7, color);
    c.outline_round(Rect::new(x + 4, y + 5, 6, 14), 3, color);
    // the open end
    c.fill_rect(x + 7, y, 7, 8, panel());
}

/// A QR code of `text`, as big as fits in `r` with a margin. False if the
/// text does not fit in a QR code.
fn draw_qr_code(c: &mut Canvas, r: Rect, text: &str) -> bool {
    use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};
    let len = Version::MAX.buffer_len();
    let mut temp = alloc::vec![0u8; len];
    let mut out = alloc::vec![0u8; len];
    let Ok(qr) = QrCode::encode_text(
        text,
        &mut temp,
        &mut out,
        QrCodeEcc::Medium,
        Version::MIN,
        Version::MAX,
        None,
        true,
    ) else {
        return false;
    };
    let n = qr.size();
    let cell = (r.w.min(r.h) - 24) / n;
    let x0 = r.x + (r.w - cell * n) / 2;
    let y0 = r.y + (r.h - cell * n) / 2;
    for y in 0..n {
        for x in 0..n {
            if qr.get_module(x, y) {
                c.fill_rect(x0 + x * cell, y0 + y * cell, cell, cell, rgb(0, 0, 0));
            }
        }
    }
    true
}
