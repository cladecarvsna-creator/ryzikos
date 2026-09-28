//! Telegram: a window in the style of Telegram Desktop. It asks for the
//! api_id and api_hash once (unless the build carries them), then shows a
//! QR code to scan with the phone, or asks for the phone number and the
//! code; then the two-step verification password if there is one. Signed
//! in, it shows the chat list on the left and the open chat on the right,
//! with a box to write in and a paper clip to send files.
//!
//! Messages show their photos and files (a click downloads and opens
//! them), emoji as pictures and clickable links; @names and t.me links
//! open the chat, even channels we are not in, with a Join button. The
//! name at the top opens the chat's profile with its @name to copy, and
//! the search box also looks on Telegram for people and channels.
//!
//! The client itself (crate::tg) runs in a fiber; this file only draws
//! what it shares and passes on what the user does.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::emoji::{self, GROUPS};
use super::filedialog::{self, FileDialog, Mode};
use super::rich;
use super::text::{Font, HEADING, TITLE, UI, UI_BOLD};
use super::widgets::{self, FieldEvent, TextField};
use super::{theme, App, MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::keyboard::Key;
use crate::tg::{self, Chat, ChatKind, Cmd, History, Message, Peer, Preview, Shared, Stage};
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
/// The tallest a picture in a message is drawn.
const PIC_MAX_H: i32 = 320;
/// A file's row in a message, and a link preview.
const FILE_H: i32 = 48;
const WEB_H: i32 = 2 * LINE_H + 8;
/// The profile on the right of the chat.
const PROFILE_W: i32 = 320;
/// The emoji picker: cells, columns, and the tabs under them.
const EMOJI_CELL: i32 = 34;
const EMOJI_COLS: i32 = 9;
const EMOJI_TABS_H: i32 = 38;
/// A section title in the chat list.
const SECTION_H: i32 = 30;
/// Look on Telegram after the search box has been still this long.
const SEARCH_WAIT_MS: i64 = 600;
/// How long a note over the chat stays.
const NOTE_MS: i64 = 4000;

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
    /// The smiley that opens the emoji picker.
    Emoji,
    /// Join a channel we are looking at from outside.
    Join,
}

/// Something that can be clicked in the chat, found again from where it
/// was last drawn.
#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Link(String),
    /// A message's photo or file: download and open it.
    Media(i64),
    /// A message, for the right-click menu.
    Bubble(i64),
    /// The chat's name at the top: its profile.
    Header,
    CloseProfile,
    Copy(String),
    /// Join (true) or leave the open channel.
    Membership(bool),
    Emoji(u16),
    EmojiTab(u8),
    /// A panel (the emoji picker, the profile) where clicks do nothing.
    Blank,
}

/// What the right-click menu of a message can do.
#[derive(Clone, Debug)]
enum Action {
    Copy(String),
    Open(String),
    Save(i64),
}

struct Context {
    x: i32,
    y: i32,
    items: Vec<(&'static str, Action)>,
    hover: Option<usize>,
}

impl Context {
    fn list(&self) -> Vec<widgets::Item<'static>> {
        self.items.iter().map(|(l, _)| (*l, "", true)).collect()
    }

    fn rect(&self) -> Rect {
        let r = widgets::menu_rect(self.x, self.y, &self.list());
        // keep it in the window
        let x = r.x.min(CLIENT_W - r.w - 4);
        let y = if r.bottom() > CLIENT_H - 4 {
            self.y - r.h
        } else {
            r.y
        };
        Rect::new(x, y, r.w, r.h)
    }
}

/// A row of the chat list.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    /// A chat of ours, by its place in the chats.
    Chat(usize),
    /// "Global search".
    Section,
    /// A chat the search found on Telegram, by its place in what it found.
    Found(usize),
}

impl Row {
    fn height(self) -> i32 {
        if self == Row::Section {
            SECTION_H
        } else {
            ROW_H
        }
    }
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
    Bubble(Bubble),
}

struct Bubble {
    index: usize,
    w: i32,
    /// The name above the text, in groups.
    name: Option<String>,
    /// A picture on top (a photo, or the first frame of a video): its
    /// size as drawn.
    pic: Option<(i32, i32)>,
    /// A row with the file's name and size.
    file_row: bool,
    /// The first line is "[Sticker]" or the like.
    label: bool,
    lines: Vec<rich::Line>,
    /// Where the links in the lines go.
    links: Vec<String>,
    /// A link preview: the site and the title.
    web: Option<(String, String)>,
    /// The time sits on its own line under the text.
    time_below: bool,
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
    /// The row under the mouse, by its place in the rows shown.
    hover_row: Option<usize>,
    hover_button: Option<Button>,
    pressed: Option<Button>,
    menu: Option<Option<usize>>,
    /// Choosing a file to send.
    dialog: Option<FileDialog>,
    /// A note over the chat, like a file that could not be sent, and
    /// when it goes.
    note: Option<(String, i64)>,
    /// Where things that can be clicked were drawn.
    hits: Vec<(Rect, Hit)>,
    hover_hit: Option<Hit>,
    /// The right-click menu of a message.
    context: Option<Context>,
    /// The profile of the open chat is shown.
    profile: bool,
    /// The emoji picker is open: its tab and how far it is scrolled.
    picker: bool,
    picker_tab: u8,
    picker_scroll: i32,
    /// What was last looked for on Telegram, and when the box changed.
    searched: String,
    search_changed: i64,
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
            hits: Vec::new(),
            hover_hit: None,
            context: None,
            profile: false,
            picker: false,
            picker_tab: 0,
            picker_scroll: 0,
            searched: String::new(),
            search_changed: 0,
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
        let mut redraw = false;
        let (goto, to_open, notice) = {
            let mut s = self.shared.borrow_mut();
            (
                s.goto.take(),
                core::mem::take(&mut s.to_open),
                s.notice.take(),
            )
        };
        if let Some(peer) = goto {
            self.search.set("");
            self.list_top = 0;
            self.open_chat(peer);
            redraw = true;
        }
        for path in to_open {
            open_file(&path);
        }
        if let Some(n) = notice {
            self.say(n);
            redraw = true;
        }
        let now = tg::mtproto::now_ms();
        if self.note.as_ref().is_some_and(|n| now >= n.1) {
            self.note = None;
            redraw = true;
        }
        // look on Telegram once the search box is still
        let q = String::from(self.search.string().trim());
        if q.chars().count() >= 2
            && q != self.searched
            && now - self.search_changed >= SEARCH_WAIT_MS
        {
            self.searched = q.clone();
            self.command(Cmd::Search(q));
        }
        let version = self.shared.borrow().version;
        redraw || version != self.drawn
    }

    /// Show a note over the chat for a few seconds.
    fn say(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), tg::mtproto::now_ms() + NOTE_MS));
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
            CLIENT_W - LIST_W - 160,
            36,
        )
    }

    fn send_rect() -> Rect {
        Rect::new(CLIENT_W - 52, CLIENT_H - INPUT_H + 8, 40, 40)
    }

    fn emoji_rect() -> Rect {
        Rect::new(CLIENT_W - 94, CLIENT_H - INPUT_H + 8, 40, 40)
    }

    fn join_rect(&self) -> Rect {
        let mut w = CLIENT_W - LIST_W - 1;
        if self.profile {
            w -= PROFILE_W;
        }
        Rect::new(LIST_W + 1 + w / 2 - 110, CLIENT_H - INPUT_H + 9, 220, 38)
    }

    fn picker_rect() -> Rect {
        let w = EMOJI_COLS * EMOJI_CELL + 16;
        let h = 7 * EMOJI_CELL + EMOJI_TABS_H + 16;
        Rect::new(CLIENT_W - w - 8, CLIENT_H - INPUT_H - h - 6, w, h)
    }

    fn profile_rect() -> Rect {
        Rect::new(CLIENT_W - PROFILE_W, TOP_H, PROFILE_W, CLIENT_H - TOP_H)
    }

    fn list_rows() -> i32 {
        (CLIENT_H - TOP_H + ROW_H - 1) / ROW_H
    }

    /// The rows of the chat list: our chats the search box lets through,
    /// then what the search found on Telegram.
    fn rows(&self) -> Vec<Row> {
        let s = self.shared.borrow();
        let q = self.search.string();
        let q = q.trim().trim_start_matches('@').to_lowercase();
        let mut rows: Vec<Row> = s
            .chats
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                q.is_empty()
                    || c.title.to_lowercase().contains(&q)
                    || c.username.to_lowercase().contains(&q)
            })
            .map(|(i, _)| Row::Chat(i))
            .collect();
        if let Some((fq, found)) = &s.found {
            if !q.is_empty() && fq.trim().trim_start_matches('@').to_lowercase() == q {
                let new: Vec<Row> = found
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| s.chat(c.peer).is_none())
                    .map(|(i, _)| Row::Found(i))
                    .collect();
                if !new.is_empty() {
                    rows.push(Row::Section);
                    rows.extend(new);
                }
            }
        }
        rows
    }

    /// The top of the chat list, under the connection banner.
    fn list_y(&self) -> i32 {
        let s = self.shared.borrow();
        if s.error.is_some() || !s.online {
            TOP_H + 30
        } else {
            TOP_H
        }
    }

    /// The row under (x, y), by its place in `rows`.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let top = self.list_y();
        if x >= LIST_W || y < top {
            return None;
        }
        let rows = self.rows();
        let mut ry = top;
        for (i, r) in rows.iter().enumerate().skip(self.list_top as usize) {
            if y < ry + r.height() {
                return (*r != Row::Section).then_some(i);
            }
            ry += r.height();
            if ry > CLIENT_H {
                break;
            }
        }
        None
    }

    /// Open what a row is.
    fn click_row(&mut self, row: Row) {
        let peer = {
            let s = self.shared.borrow();
            match row {
                Row::Chat(i) => s.chats.get(i).map(|c| (c.peer, false)),
                Row::Found(i) => s
                    .found
                    .as_ref()
                    .and_then(|f| f.1.get(i))
                    .map(|c| (c.peer, true)),
                Row::Section => None,
            }
        };
        match peer {
            Some((peer, false)) => self.open_chat(peer),
            // into the list first, then the client says to open it
            Some((peer, true)) => self.command(Cmd::Show(peer)),
            None => {}
        }
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
        if self.menu.is_some() || self.context.is_some() {
            self.menu = None;
            self.context = None;
            return true;
        }
        match key {
            Key::Escape if self.picker => {
                self.picker = false;
                return true;
            }
            Key::Escape if self.profile => {
                self.profile = false;
                return true;
            }
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
            let before = self.search.text.clone();
            let event = self.search.on_key(key);
            if event == FieldEvent::Enter {
                let q = self.search.string();
                let q = q.trim();
                if q.starts_with('@') || q.contains("t.me/") {
                    // an @name or a link: straight there
                    self.command(Cmd::Resolve(String::from(q)));
                } else if let Some(&row) = self.rows().first() {
                    self.click_row(row);
                }
            }
            if self.search.text != before {
                self.search_changed = tg::mtproto::now_ms();
                self.list_top = 0;
            }
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
            Err(e) => self.say(alloc::format!("Can't paste the picture: {}", e.message())),
        }
    }

    fn open_chat(&mut self, peer: Peer) {
        if self.open != Some(peer) {
            self.profile = false;
            self.input.set("");
        }
        self.open = Some(peer);
        self.shared.borrow_mut().open = Some(peer);
        self.scroll = 0;
        self.content_h = 0;
        self.newest = 0;
        self.focus = Focus::Input;
        self.context = None;
        // pictures of other chats make room
        self.shared.borrow_mut().previews.retain(|k, _| k.0 == peer);
        self.command(Cmd::Open(peer));
    }

    /// Put text where the caret is in the message box.
    fn insert(&mut self, text: &str) {
        let f = &mut self.input;
        let (a, b) = (f.anchor.min(f.cursor), f.anchor.max(f.cursor));
        f.text.drain(a..b.min(f.text.len()));
        let mut at = a.min(f.text.len());
        for c in text.chars() {
            f.text.insert(at, c);
            at += 1;
        }
        f.cursor = at;
        f.anchor = at;
        self.focus = Focus::Input;
    }

    /// Go where a link in a message points: chats in this window, web
    /// pages in the browser.
    fn follow(&mut self, url: &str) {
        let l = url.trim();
        let lower = l.to_ascii_lowercase();
        let bare = lower
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.");
        if l.starts_with('@')
            || lower.starts_with("tg:")
            || bare.starts_with("t.me/")
            || bare.starts_with("telegram.me/")
            || bare.starts_with("telegram.dog/")
        {
            self.command(Cmd::Resolve(String::from(l)));
            return;
        }
        if lower.starts_with("mailto:") || lower.starts_with("tel:") {
            widgets::copy(l.split_once(':').map_or(l, |p| p.1));
            self.say("Copied");
            return;
        }
        let address = if lower.starts_with("http://") || lower.starts_with("https://") {
            String::from(l)
        } else {
            alloc::format!("https://{}", l)
        };
        super::request_address(&address);
        super::request_open(App::Browser);
    }

    /// Download a message's photo or file and open it.
    fn open_media(&mut self, id: i64) {
        let Some(peer) = self.open else {
            return;
        };
        let busy = self
            .shared
            .borrow()
            .downloads
            .get(&(peer, id))
            .is_some_and(|d| d.path.is_none() && d.failed.is_none());
        if !busy {
            self.command(Cmd::Download(peer, id, true));
        }
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        // drawn last is on top
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| h.clone())
    }

    fn click_hit(&mut self, hit: Hit) {
        match hit {
            Hit::Link(url) => self.follow(&url),
            Hit::Media(id) => self.open_media(id),
            Hit::Bubble(_) | Hit::Blank => {}
            Hit::Header => {
                self.profile = !self.profile;
                if let (true, Some(peer)) = (self.profile, self.open) {
                    self.command(Cmd::Info(peer));
                }
            }
            Hit::CloseProfile => self.profile = false,
            Hit::Copy(text) => {
                widgets::copy(&text);
                self.say(alloc::format!("Copied {}", text));
            }
            Hit::Membership(join) => {
                if let Some(peer) = self.open {
                    self.command(if join {
                        Cmd::Join(peer)
                    } else {
                        Cmd::Leave(peer)
                    });
                }
            }
            Hit::Emoji(e) => {
                let t = emoji::get().text(e);
                self.insert(&t);
            }
            Hit::EmojiTab(t) => {
                self.picker_tab = t;
                self.picker_scroll = 0;
            }
        }
    }

    /// Whether a right-click at (x, y) opens this window's own menu (on
    /// the messages) rather than the desktop's Cut, Copy, Paste.
    pub fn own_menu(&self, x: i32, y: i32) -> bool {
        self.stage() == Stage::Ready
            && self.open.is_some()
            && Self::chat_area().contains(x, y)
            && self.hits.iter().any(|(r, h)| {
                r.contains(x, y) && matches!(h, Hit::Bubble(_) | Hit::Media(_) | Hit::Link(_))
            })
    }

    /// The right-click menu of a message.
    fn context_menu(&mut self, x: i32, y: i32) -> bool {
        let Some(peer) = self.open else {
            return false;
        };
        let mut link = None;
        let mut id = None;
        for (r, h) in self.hits.iter().rev() {
            if !r.contains(x, y) {
                continue;
            }
            match h {
                Hit::Link(u) if link.is_none() => link = Some(u.clone()),
                Hit::Media(i) | Hit::Bubble(i) if id.is_none() => id = Some(*i),
                _ => {}
            }
        }
        let Some(id) = id else {
            return false;
        };
        let s = self.shared.borrow();
        let Some(m) = s
            .history
            .get(&peer)
            .and_then(|h| h.messages.iter().find(|m| m.id == id))
        else {
            return false;
        };
        let mut items: Vec<(&'static str, Action)> = Vec::new();
        if let Some(l) = link {
            items.push(("Open link", Action::Open(l.clone())));
            items.push(("Copy link", Action::Copy(l)));
        }
        if !m.text.is_empty() {
            items.push(("Copy text", Action::Copy(m.text.clone())));
        }
        if (m.photo.is_some() || m.file.is_some()) && id != 0 {
            items.push(("Save to Downloads", Action::Save(id)));
        }
        drop(s);
        if items.is_empty() {
            return false;
        }
        self.context = Some(Context {
            x,
            y,
            items,
            hover: None,
        });
        true
    }

    fn context_click(&mut self, x: i32, y: i32) {
        let Some(ctx) = self.context.take() else {
            return;
        };
        let i = widgets::menu_item_at(ctx.rect(), &ctx.list(), x, y).or(ctx.hover);
        let Some((_, action)) = i.and_then(|i| ctx.items.get(i)) else {
            return;
        };
        match action.clone() {
            Action::Copy(t) => {
                widgets::copy(&t);
                self.say("Copied");
            }
            Action::Open(l) => self.follow(&l),
            Action::Save(id) => {
                if let Some(peer) = self.open {
                    self.command(Cmd::Download(peer, id, false));
                    self.say("Saving to Downloads...");
                }
            }
        }
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
            MouseKind::Down { right: true } => {
                self.menu = None;
                let had = self.context.take().is_some();
                if self.stage() == Stage::Ready && x > LIST_W {
                    return self.context_menu(x, y) || had;
                }
                return had;
            }
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
                        Some(Button::Emoji) => {
                            self.picker = !self.picker;
                            self.focus = Focus::Input;
                        }
                        Some(Button::Join) => self.click_hit(Hit::Membership(true)),
                        _ => {}
                    }
                }
                return pressed.is_some();
            }
            _ => return false,
        }
        if self.context.is_some() {
            self.context_click(x, y);
            return true;
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
                if self.note.is_some() {
                    self.note = None;
                }
                if let Some(b) = self.button_at(x, y) {
                    self.pressed = Some(b);
                    return true;
                }
                if let Some(hit) = self.hit_at(x, y) {
                    let keep = matches!(hit, Hit::Emoji(_) | Hit::EmojiTab(_) | Hit::Blank);
                    if !keep {
                        self.picker = false;
                    }
                    self.click_hit(hit);
                    return true;
                }
                self.picker = false;
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
                    if let Some(&row) = self.rows().get(i) {
                        self.click_row(row);
                    }
                    return true;
                }
                if Self::input_rect().contains(x, y) && self.open.is_some() {
                    self.focus = Focus::Input;
                    let ir = Self::input_rect();
                    self.input.cursor = self.input_place(ir, x);
                    self.input.anchor = self.input.cursor;
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

    /// Where the input box is scrolled to, so the caret stays in view.
    fn input_shift(&self, ir: Rect) -> i32 {
        let before: String = self.input.text[..self.input.cursor.min(self.input.text.len())]
            .iter()
            .collect();
        let cw = rich::width(&UI, &before);
        (cw - (ir.w - 12)).max(0)
    }

    /// The place in the input text nearest to `x`.
    fn input_place(&self, ir: Rect, x: i32) -> usize {
        let x0 = ir.x + 4 - self.input_shift(ir);
        let mut s = String::new();
        let mut prev = 0;
        for (i, &c) in self.input.text.iter().enumerate() {
            s.push(c);
            let w = rich::width(&UI, &s);
            if x < x0 + (prev + w) / 2 {
                return i;
            }
            prev = w;
        }
        self.input.text.len()
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
                let outside = self.outside();
                if self.profile && self.open.is_some() && Self::profile_rect().contains(x, y) {
                    return None;
                }
                if self.picker && Self::picker_rect().contains(x, y) {
                    None
                } else if outside && self.join_rect().contains(x, y) {
                    Some(Button::Join)
                } else if outside {
                    Self::menu_button().contains(x, y).then_some(Button::Menu)
                } else if self.open.is_some() && Self::send_rect().contains(x, y) {
                    Some(Button::Send)
                } else if self.open.is_some() && Self::attach_rect().contains(x, y) {
                    Some(Button::Attach)
                } else if self.open.is_some() && Self::emoji_rect().contains(x, y) {
                    Some(Button::Emoji)
                } else if Self::menu_button().contains(x, y) {
                    Some(Button::Menu)
                } else {
                    None
                }
            }
            Stage::Starting => None,
        }
    }

    /// The open chat is a channel or group we are not in: it shows a
    /// Join button instead of the box to write in.
    fn outside(&self) -> bool {
        let Some(peer) = self.open else {
            return false;
        };
        self.shared.borrow().chat(peer).is_some_and(|c| c.left)
    }

    pub fn on_wheel(&mut self, delta: i32) -> bool {
        if let Some(d) = &mut self.dialog {
            return d.on_wheel(delta, full());
        }
        if self.stage() != Stage::Ready {
            return false;
        }
        if self.picker
            && self
                .hover_hit
                .as_ref()
                .is_some_and(|h| matches!(h, Hit::Emoji(_) | Hit::EmojiTab(_) | Hit::Blank))
        {
            let n = emoji::get()
                .list
                .iter()
                .filter(|e| e.1 == self.picker_tab)
                .count() as i32;
            let rows = (n + EMOJI_COLS - 1) / EMOJI_COLS;
            let max = ((rows - 7) * EMOJI_CELL).max(0);
            self.picker_scroll = (self.picker_scroll + delta * 2 * EMOJI_CELL).clamp(0, max);
            return true;
        }
        if self.hover_row.is_some() || self.open.is_none() {
            let n = self.rows().len() as i32;
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
        let hit = if self.stage() == Stage::Ready {
            self.hit_at(x, y)
        } else {
            None
        };
        let mut changed =
            row != self.hover_row || button != self.hover_button || hit != self.hover_hit;
        // keep the list scrollable by the wheel when the mouse is on it
        if row.is_none() && x < LIST_W && y > TOP_H && self.hover_row.is_none() {
            changed = hit != self.hover_hit;
        }
        self.hover_row = row.or(if x < LIST_W && y > TOP_H {
            Some(usize::MAX)
        } else {
            None
        });
        self.hover_button = button;
        self.hover_hit = hit;
        if let Some(menu) = self.menu {
            let items = Self::menu_items();
            let h = widgets::menu_item_at(Self::menu_rect(), &items, x, y);
            if h != menu {
                self.menu = Some(h);
                changed = true;
            }
        }
        if let Some(ctx) = &mut self.context {
            let h = widgets::menu_item_at(ctx.rect(), &ctx.list(), x, y);
            if h != ctx.hover {
                ctx.hover = h;
                changed = true;
            }
        }
        changed
    }

    // ---- drawing ------------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, focused: bool, caret: bool) {
        self.drawn = self.shared.borrow().version;
        self.hits.clear();
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
            Stage::Starting => {
                // the step it is on, so a stuck connection says where
                let mut l = alloc::vec![String::from("Connecting...")];
                if let Some(step) = tg::client::last_step() {
                    l.push(fit(&UI, &step, card.w - 40));
                }
                ("Telegram", l)
            }
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
            Some(peer) => {
                self.draw_chat(c, peer, caret);
                if self.profile {
                    self.draw_profile(c, peer);
                }
                if self.picker && !self.outside() {
                    self.draw_picker(c);
                }
            }
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
        if let Some((n, _)) = &self.note {
            let area = Self::chat_area();
            let n = fit(&UI, n, area.w - 60);
            draw_pill(c, area.x + area.w / 2, area.bottom() - 22, &n);
        }
        if let Some(hover) = self.menu {
            let items = Self::menu_items();
            widgets::draw_menu(c, Self::menu_rect(), &items, hover);
        }
        if let Some(ctx) = &self.context {
            widgets::draw_menu(c, ctx.rect(), &ctx.list(), ctx.hover);
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
            c.draw_text(sr.x + 14, sr.y + 9, "Search or @name", dim());
            if search_focused && caret {
                c.fill_rect(sr.x + 14, sr.y + 8, 1, 18, text());
            }
        } else {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_to(sr.inset(4));
            let s = self.search.string();
            let w = rich::width(&UI, &s);
            let x = sr.x + 14 - (w - (sr.w - 30)).max(0);
            rich::draw(&mut sub, &UI, x, sr.y + 9, &s, text());
            if search_focused && caret {
                let before: String = self.search.text[..self.search.cursor].iter().collect();
                sub.fill_rect(x + 1 + rich::width(&UI, &before), sr.y + 8, 1, 18, text());
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
        let rows = self.rows();
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
        let empty_found = Vec::new();
        let found = s.found.as_ref().map_or(&empty_found, |f| &f.1);
        for (pos, row) in rows.iter().enumerate().skip(self.list_top as usize) {
            if y >= CLIENT_H {
                break;
            }
            let chat = match *row {
                Row::Section => {
                    c.fill(
                        Rect::new(0, y, LIST_W, SECTION_H),
                        pick(rgb(0xf4, 0xf4, 0xf5), rgb(0x1e, 0x28, 0x33)),
                    );
                    c.draw_text(14, y + 7, "Global search", dim());
                    y += SECTION_H;
                    continue;
                }
                Row::Chat(i) => &s.chats[i],
                Row::Found(i) => &found[i],
            };
            let is_open = self.open == Some(chat.peer);
            let r = Rect::new(0, y, LIST_W, ROW_H);
            if is_open {
                c.fill(r, selected());
            } else if self.hover_row == Some(pos) {
                c.fill(r, hover());
            }
            draw_row(c, chat, y, is_open, matches!(row, Row::Found(_)), now, tz);
            y += ROW_H;
        }
        if rows.is_empty() {
            let msg = if s.chats.is_empty() {
                "Loading chats..."
            } else if s.found.as_ref().is_some_and(|f| f.0 == self.searched)
                || self.search.text.len() < 2
            {
                "No chats found"
            } else {
                "Searching..."
            };
            let w = UI.width(msg);
            c.draw_text((LIST_W - w) / 2, y + 40, msg, dim());
        }
    }

    fn draw_chat(&mut self, c: &mut Canvas, peer: Peer, caret: bool) {
        let s = self.shared.borrow();
        let chat = s.chat(peer).cloned();
        let (title, kind, username, left) = chat.as_ref().map_or(
            (String::new(), ChatKind::Private, String::new(), false),
            |c| (c.title.clone(), c.kind, c.username.clone(), c.left),
        );
        let read_out = chat.as_ref().map_or(0, |c| c.read_out);
        let tz = s.tz;

        // the header: a click opens the profile
        let head = Rect::new(LIST_W + 1, 0, CLIENT_W - LIST_W - 1, TOP_H);
        c.fill(head, panel());
        if self.hover_hit == Some(Hit::Header) {
            c.fill(head, hover());
        }
        rich::draw_fit(c, &UI_BOLD, head.x + 20, 10, &title, head.w - 40, text());
        let members = s.info.get(&peer).map_or(0, |i| i.members);
        let mut subtitle = String::from(match kind {
            ChatKind::Saved => "your cloud storage",
            ChatKind::Bot => "bot",
            ChatKind::Group => "group",
            ChatKind::Channel => "channel",
            ChatKind::Private => "private chat",
        });
        if members > 0 {
            let what = if kind == ChatKind::Channel {
                "subscribers"
            } else {
                "members"
            };
            subtitle = alloc::format!("{} {}", group_digits(members), what);
        }
        let mut sx = head.x + 20;
        if !username.is_empty() && kind != ChatKind::Saved {
            sx += c.draw_text(sx, 30, &alloc::format!("@{}", username), BLUE);
            sx += c.draw_text(sx, 30, "  \u{2022}  ", dim());
        }
        c.draw_text(sx, 30, &subtitle, dim());
        c.fill_rect(head.x, TOP_H - 1, head.w, 1, line());
        self.hits.push((head, Hit::Header));

        // the messages
        let mut area = Self::chat_area();
        if self.profile {
            area.w -= PROFILE_W;
        }
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
        let mut want = Vec::new();
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
            let mut ctx = Draw {
                s: &s,
                peer,
                read_out,
                tz,
                hits: &mut self.hits,
                want: &mut want,
                hover: &self.hover_hit,
            };
            for l in &laid {
                let y = base + l.y;
                if y + l.h < area.y || y > area.bottom() {
                    continue;
                }
                match &l.kind {
                    LaidKind::Date(d) => draw_pill(c, area.x + area.w / 2, y + 14, d),
                    LaidKind::Service(t) => draw_pill(c, area.x + area.w / 2, y + 14, t),
                    LaidKind::Bubble(b) => {
                        let m = &h.messages[b.index];
                        let x = if m.out {
                            area.right() - 16 - b.w
                        } else {
                            area.x + 16
                        };
                        draw_bubble(c, &mut ctx, m, b, x, y, l.h);
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
        if !want.is_empty() {
            let mut s = self.shared.borrow_mut();
            for id in want {
                if let alloc::collections::btree_map::Entry::Vacant(e) =
                    s.previews.entry((peer, id))
                {
                    e.insert(Preview::Loading);
                    s.commands.push_back(Cmd::Preview(peer, id));
                }
            }
        }

        // the box to write in, or a Join button
        let bar = Rect::new(
            LIST_W + 1,
            CLIENT_H - INPUT_H,
            CLIENT_W - LIST_W - 1,
            INPUT_H,
        );
        c.fill(bar, panel());
        c.fill_rect(bar.x, bar.y, bar.w, 1, line());
        if left {
            let jr = self.join_rect();
            if self.hover_button == Some(Button::Join) {
                c.fill_round(jr, 8, hover());
            }
            let label = if kind == ChatKind::Channel {
                "JOIN CHANNEL"
            } else {
                "JOIN GROUP"
            };
            c.text_centered_in(&UI_BOLD, jr, label, BLUE);
            return;
        }
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
                // the caret stays in view
                let x = ir.x + 4 - self.input_shift(ir);
                rich::draw(&mut sub, &UI, x, ir.y + 9, &self.input.string(), text());
                if focused && caret {
                    let before: String = self.input.text[..self.input.cursor].iter().collect();
                    let cw = rich::width(&UI, &before);
                    sub.fill_rect(x + cw, ir.y + 8, 1, 18, text());
                }
            }
        }
        let ar = Self::attach_rect();
        if self.hover_button == Some(Button::Attach) {
            c.fill_round(ar, 20, hover());
        }
        draw_clip(c, ar.x + 12, ar.y + 8, dim());
        let er = Self::emoji_rect();
        if self.hover_button == Some(Button::Emoji) || self.picker {
            c.fill_round(er, 20, hover());
        }
        if let Some((e, _)) = emoji::get().at(&['\u{1f642}']) {
            emoji::get().draw_in(c, e, er);
        }
        let sr = Self::send_rect();
        let active = !self.input.text.is_empty();
        let color = if active { BLUE } else { dim() };
        if self.hover_button == Some(Button::Send) {
            c.fill_round(sr, 20, hover());
        }
        draw_plane(c, sr.x + 8, sr.y + 10, 24, color);
    }

    /// The profile of the open chat, on the right: its @name and link to
    /// copy, what it is about, and Join or Leave.
    fn draw_profile(&mut self, c: &mut Canvas, peer: Peer) {
        let pr = Self::profile_rect();
        c.fill(pr, panel());
        c.fill_rect(pr.x, pr.y, 1, pr.h, line());
        self.hits.push((pr, Hit::Blank));
        let s = self.shared.borrow();
        let Some(chat) = s.chat(peer).cloned() else {
            return;
        };
        let info = s.info.get(&peer).cloned();
        drop(s);
        c.draw_text_in(&UI_BOLD, pr.x + 20, pr.y + 14, "Info", text());
        let close = Rect::new(pr.right() - 42, pr.y + 8, 32, 32);
        if self.hover_hit == Some(Hit::CloseProfile) {
            c.fill_round(close, 16, hover());
        }
        for d in 0..2 {
            c.line(
                close.x + 10 + d,
                close.y + 10,
                close.x + 21 + d,
                close.y + 21,
                dim(),
            );
            c.line(
                close.x + 21 + d,
                close.y + 10,
                close.x + 10 + d,
                close.y + 21,
                dim(),
            );
        }
        self.hits.push((close, Hit::CloseProfile));
        let cx = pr.x + pr.w / 2;
        let id = match peer {
            Peer::User(id) | Peer::Chat(id) | Peer::Channel(id) => id,
        };
        draw_avatar(c, cx, pr.y + 96, 44, &chat.title, id, chat.kind);
        let tw = rich::width(&UI_BOLD, &chat.title).min(pr.w - 40);
        rich::draw_fit(
            c,
            &UI_BOLD,
            cx - tw / 2,
            pr.y + 150,
            &chat.title,
            pr.w - 40,
            text(),
        );
        let members = info.as_ref().map_or(0, |i| i.members);
        let what = match chat.kind {
            ChatKind::Channel if members > 0 => {
                alloc::format!("{} subscribers", group_digits(members))
            }
            ChatKind::Group if members > 0 => alloc::format!("{} members", group_digits(members)),
            ChatKind::Channel => String::from("channel"),
            ChatKind::Group => String::from("group"),
            ChatKind::Bot => String::from("bot"),
            ChatKind::Saved => String::from("your cloud storage"),
            ChatKind::Private => String::from("user"),
        };
        let ww = UI.width(&what);
        c.draw_text(cx - ww / 2, pr.y + 172, &what, dim());

        let mut y = pr.y + 206;
        c.fill_rect(pr.x, y - 10, pr.w, 1, line());
        let mut rows: Vec<(&str, String, String)> = Vec::new();
        if !chat.username.is_empty() && chat.kind != ChatKind::Saved {
            let name = alloc::format!("@{}", chat.username);
            rows.push(("Username", name.clone(), name));
            let link = alloc::format!("t.me/{}", chat.username);
            rows.push(("Link", link.clone(), alloc::format!("https://{}", link)));
        }
        if let Some(i) = &info {
            if !i.phone.is_empty() {
                let phone = alloc::format!("+{}", i.phone.trim_start_matches('+'));
                rows.push(("Mobile", phone.clone(), phone));
            }
        }
        for (label, value, copy) in rows {
            let r = Rect::new(pr.x, y - 4, pr.w, 46);
            let hit = Hit::Copy(copy);
            if self.hover_hit.as_ref() == Some(&hit) {
                c.fill(r, hover());
                let cw = UI.width("Copy");
                c.draw_text(pr.right() - 20 - cw, y + 10, "Copy", BLUE);
            }
            c.draw_text(pr.x + 20, y, &fit(&UI, &value, pr.w - 90), text());
            c.draw_text(pr.x + 20, y + 19, label, dim());
            self.hits.push((r, hit));
            y += 50;
        }
        if let Some(about) = info
            .as_ref()
            .map(|i| i.about.clone())
            .filter(|a| !a.is_empty())
        {
            let lines = rich::wrap(&UI, &rich::pieces(&UI, &about, &[]), pr.w - 40);
            for l in lines.iter().take(8) {
                rich::draw_line(c, &UI, pr.x + 20, y, l, text(), BLUE);
                y += LINE_H;
            }
            let label = if chat.kind == ChatKind::Private || chat.kind == ChatKind::Bot {
                "Bio"
            } else {
                "Description"
            };
            c.draw_text(pr.x + 20, y, label, dim());
            y += 30;
        } else if info.is_none() {
            c.draw_text(pr.x + 20, y, "Loading...", dim());
            y += 30;
        }
        // Join or Leave, for channels and supergroups
        if matches!(peer, Peer::Channel(_)) {
            let r = Rect::new(pr.x + 20, y.max(pr.bottom() - 60), pr.w - 40, 40);
            let hit = Hit::Membership(chat.left);
            if self.hover_hit.as_ref() == Some(&hit) {
                c.fill_round(r, 8, hover());
            }
            let (label, color) = match (chat.left, chat.kind == ChatKind::Channel) {
                (true, true) => ("Join channel", BLUE),
                (true, false) => ("Join group", BLUE),
                (false, true) => ("Leave channel", theme::error()),
                (false, false) => ("Leave group", theme::error()),
            };
            c.text_centered_in(&UI_BOLD, r, label, color);
            self.hits.push((r, hit));
        }
    }

    /// The emoji picker over the chat, above the smiley button.
    fn draw_picker(&mut self, c: &mut Canvas) {
        let pr = Self::picker_rect();
        c.shadow(pr, 12, 6, 2, 60);
        c.fill_round(pr, 12, panel());
        c.outline_round(pr, 12, line());
        self.hits.push((pr, Hit::Blank));
        let e = emoji::get();
        let grid = Rect::new(pr.x + 8, pr.y + 8, EMOJI_COLS * EMOJI_CELL, 7 * EMOJI_CELL);
        {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_to(grid);
            let clip = sub.clip_rect();
            let tab = self.picker_tab;
            let ids = e
                .list
                .iter()
                .enumerate()
                .filter(|(_, x)| x.1 == tab)
                .map(|(i, _)| i as u16);
            for (k, id) in ids.enumerate() {
                let (col, row) = (k as i32 % EMOJI_COLS, k as i32 / EMOJI_COLS);
                let r = Rect::new(
                    grid.x + col * EMOJI_CELL,
                    grid.y + row * EMOJI_CELL - self.picker_scroll,
                    EMOJI_CELL,
                    EMOJI_CELL,
                );
                if r.bottom() < grid.y {
                    continue;
                }
                if r.y > grid.bottom() {
                    break;
                }
                let hit = Hit::Emoji(id);
                if self.hover_hit.as_ref() == Some(&hit) {
                    sub.fill_round(r.inset(1), 6, hover());
                }
                e.draw_in(&mut sub, id, r);
                let vis = r.intersect(&clip);
                if !vis.is_empty() {
                    self.hits.push((vis, hit));
                }
            }
        }
        // the groups' tabs
        let ty = pr.bottom() - EMOJI_TABS_H;
        c.fill_rect(pr.x, ty, pr.w, 1, line());
        let tw = (pr.w - 16) / GROUPS.len() as i32;
        for (i, (_, sample)) in GROUPS.iter().enumerate() {
            let r = Rect::new(pr.x + 8 + i as i32 * tw, ty + 4, tw, EMOJI_TABS_H - 8);
            let hit = Hit::EmojiTab(i as u8);
            if i as u8 == self.picker_tab {
                c.fill_round(r, 6, pick(rgb(0xe4, 0xee, 0xf6), rgb(0x2b, 0x52, 0x78)));
            } else if self.hover_hit.as_ref() == Some(&hit) {
                c.fill_round(r, 6, hover());
            }
            if let Some((id, _)) = e.at(&[*sample]) {
                e.draw_in(c, id, r);
            }
            self.hits.push((r, hit));
        }
    }
}

/// A chat in the list: avatar, name, the last message or @name, the
/// time and the unread count.
fn draw_row(c: &mut Canvas, chat: &Chat, y: i32, is_open: bool, found: bool, now: i64, tz: i64) {
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
    let date = if found || chat.date == 0 {
        String::new()
    } else {
        short_date(chat.date + tz, now)
    };
    let dw = UI.width(&date);
    c.draw_text(LIST_W - 12 - dw, y + 13, &date, sub_fg);
    let mut name_x = tx;
    if chat.kind == ChatKind::Channel || chat.kind == ChatKind::Group {
        draw_group_mark(c, tx, y + 14, fg, chat.kind == ChatKind::Channel);
        name_x += 20;
    }
    rich::draw_fit(
        c,
        &UI_BOLD,
        name_x,
        y + 12,
        &chat.title,
        LIST_W - 12 - dw - 8 - name_x,
        fg,
    );
    if found {
        let what = if chat.username.is_empty() {
            String::from(match chat.kind {
                ChatKind::Channel => "channel",
                ChatKind::Group => "group",
                ChatKind::Bot => "bot",
                _ => "user",
            })
        } else {
            alloc::format!("@{}", chat.username)
        };
        c.draw_text(tx, y + 38, &fit(&UI, &what, LIST_W - 12 - tx), sub_fg);
        return;
    }
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
    rich::draw_fit(c, &UI, px, y + 38, &chat.last, right - px, sub_fg);
}

/// Open a downloaded file with its app; files no app here reads are
/// shown in their folder.
fn open_file(path: &str) {
    let lower = path.to_ascii_lowercase();
    let readable = [
        ".txt", ".md", ".json", ".csv", ".log", ".html", ".htm", ".xml", ".rs", ".py", ".js", ".c",
        ".h", ".ini", ".cfg", ".conf",
    ];
    if super::picture::is_picture(path)
        || super::video::is_video(path)
        || readable.iter().any(|e| lower.ends_with(e))
    {
        super::request_file(path);
    } else {
        let dir = path.rsplit_once('/').map_or("/", |p| p.0);
        super::request_folder(dir);
    }
}

/// 12345 as "12 345".
fn group_digits(n: i64) -> String {
    let digits = alloc::format!("{}", n);
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// A file's size for people: "816 B", "12.4 KB", "3.1 MB".
fn size_text(n: i64) -> String {
    if n < 1024 {
        alloc::format!("{} B", n)
    } else if n < 1024 * 1024 {
        alloc::format!("{}.{} KB", n / 1024, n % 1024 * 10 / 1024)
    } else {
        let m = n * 10 / (1024 * 1024);
        alloc::format!("{}.{} MB", m / 10, m % 10)
    }
}

/// What a picture of `w` x `h` is drawn as in a message at most `max_w`
/// wide.
fn pic_size(w: i32, h: i32, max_w: i32) -> (i32, i32) {
    let (w, h) = if w <= 0 || h <= 0 { (4, 3) } else { (w, h) };
    let mut pw = max_w.min(w.max(220));
    let mut ph = (h as i64 * pw as i64 / w as i64) as i32;
    if ph > PIC_MAX_H {
        ph = PIC_MAX_H;
        pw = (w as i64 * ph as i64 / h as i64) as i32;
    }
    (pw.clamp(120, max_w), ph.max(60))
}

/// The picture a message shows (its size), whether it has a file row,
/// and the "[Sticker]" kind of line for anything else.
fn media_parts(m: &Message) -> (Option<(i32, i32)>, bool, Option<String>) {
    if let Some(p) = &m.photo {
        return (Some((p.w, p.h)), false, None);
    }
    if let Some(f) = &m.file {
        let picture = matches!(f.kind.as_str(), "Video" | "GIF" | "Video message")
            || (f.kind == "File" && f.mime.starts_with("image/") && f.mime != "image/webp");
        if f.kind == "Sticker" {
            return (None, false, Some(String::from("Sticker")));
        }
        if let (true, Some((_, w, h))) = (picture, &f.thumb) {
            return (Some((*w, *h)), false, None);
        }
        return (None, true, None);
    }
    (None, false, m.media.clone())
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
        let (pic, file_row, label) = media_parts(m);
        let mut text = String::new();
        let mut shift = 0;
        if let Some(l) = &label {
            text = alloc::format!("[{}]", l);
            if !m.text.is_empty() {
                text.push('\n');
            }
            shift = text.chars().count();
        }
        text.push_str(&m.text);
        let mut links = Vec::new();
        let mut ranges = Vec::new();
        for (a, b, url) in &m.links {
            ranges.push((a + shift, b + shift, links.len() as u16));
            links.push(url.clone());
        }
        let pic = pic.map(|(w, h)| pic_size(w, h, max_w - 8));
        let inner = match pic {
            Some((pw, _)) => pw + 8 - 2 * PAD_X,
            None => max_w - 2 * PAD_X,
        };
        let lines = if text.is_empty() {
            Vec::new()
        } else {
            rich::wrap(&UI, &rich::pieces(&UI, &text, &ranges), inner)
        };
        let name = (group && !m.out && m.from_id != last_from).then(|| clean(&m.from));
        let web = m.web.clone();
        let bare_pic = pic.is_some() && lines.is_empty() && web.is_none();
        let time_w = time_width(m);
        let widest = lines
            .iter()
            .map(|l| rich::line_width(&UI, l))
            .max()
            .unwrap_or(0);
        let last_w = lines.last().map_or(0, |l| rich::line_width(&UI, l));
        let time_below =
            !bare_pic && (web.is_some() || lines.is_empty() || last_w + 12 + time_w > inner);
        let mut w = widest.max(if time_below || bare_pic {
            time_w
        } else {
            last_w + 12 + time_w
        });
        if let Some(n) = &name {
            w = w.max(UI_BOLD.width(n).min(inner));
        }
        if let Some((site, title)) = &web {
            let ww = rich::width(&UI_BOLD, site).max(rich::width(&UI, title)) + 12;
            w = w.max(ww.min(inner));
        }
        if file_row {
            w = w.max(260.min(inner));
        }
        let w = match pic {
            Some((pw, _)) => pw + 8,
            None => w + 2 * PAD_X,
        };
        let mut h = if pic.is_some() && name.is_none() {
            4
        } else {
            PAD_Y
        };
        if name.is_some() {
            h += LINE_H;
        }
        if let Some((_, ph)) = pic {
            h += ph + if bare_pic { 4 } else { 6 };
        }
        if file_row {
            h += FILE_H;
        }
        h += lines.len() as i32 * LINE_H;
        if web.is_some() {
            h += WEB_H + 4;
        }
        if time_below {
            h += LINE_H - 4;
        }
        if !bare_pic {
            h += PAD_Y;
        }
        // messages in a row from the same person sit closer
        let gap = if m.from_id == last_from { 4 } else { 10 };
        last_from = m.from_id;
        out.push(Laid {
            y,
            h,
            kind: LaidKind::Bubble(Bubble {
                index: i,
                w,
                name,
                pic,
                file_row,
                label: label.is_some(),
                lines,
                links,
                web,
                time_below,
            }),
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

fn link_color(out: bool) -> Color {
    if out {
        pick(rgb(0x2e, 0x8b, 0xcc), rgb(0xa8, 0xd6, 0xff))
    } else {
        pick(rgb(0x16, 0x8a, 0xcd), rgb(0x71, 0xba, 0xfa))
    }
}

/// What drawing the messages needs besides the message.
struct Draw<'a> {
    s: &'a Shared,
    peer: Peer,
    read_out: i64,
    tz: i64,
    hits: &'a mut Vec<(Rect, Hit)>,
    /// Messages whose picture should be loaded.
    want: &'a mut Vec<i64>,
    hover: &'a Option<Hit>,
}

impl Draw<'_> {
    /// Remember where something can be clicked, as far as it is seen.
    fn hit(&mut self, c: &Canvas, r: Rect, hit: Hit) {
        let r = r.intersect(&c.clip_rect());
        if !r.is_empty() {
            self.hits.push((r, hit));
        }
    }
}

/// How a download is going, for the picture's corner or the file's row.
fn download_text(d: Option<&tg::Download>, size: i64) -> Option<String> {
    let d = d?;
    if let Some(e) = &d.failed {
        return Some(alloc::format!("Failed: {}", e));
    }
    if d.path.is_some() {
        return None;
    }
    let total = if d.total > 0 { d.total } else { size };
    Some(if total > 0 {
        alloc::format!(
            "{}% of {}",
            (d.done * 100 / total).clamp(0, 100),
            size_text(total)
        )
    } else {
        String::from("Downloading...")
    })
}

fn draw_bubble(c: &mut Canvas, d: &mut Draw, m: &Message, b: &Bubble, x: i32, y: i32, h: i32) {
    let w = b.w;
    let r = Rect::new(x, y, w, h);
    let face = if m.out { bubble_out() } else { bubble_in() };
    c.shadow(r, 10, 2, 1, 30);
    c.fill_round(r, 10, face);
    if m.id != 0 {
        d.hit(c, r, Hit::Bubble(m.id));
    }
    let tcolor = if m.out { time_out() } else { time_in() };
    let bare_pic = b.pic.is_some() && b.lines.is_empty() && b.web.is_none();
    let mut ty = y + if b.pic.is_some() && b.name.is_none() {
        4
    } else {
        PAD_Y
    };
    if let Some(n) = &b.name {
        let n = fit(&UI_BOLD, n, w - 2 * PAD_X);
        c.draw_text_in(&UI_BOLD, x + PAD_X, ty, &n, palette(m.from_id));
        ty += LINE_H;
    }
    let download = d.s.downloads.get(&(d.peer, m.id));
    // the picture
    if let Some((pw, ph)) = b.pic {
        let pr = Rect::new(x + 4, ty, pw, ph);
        let grey = pick(rgb(0xd8, 0xde, 0xe4), rgb(0x24, 0x31, 0x40));
        {
            let mut sub = c.sub(Rect::new(0, 0, c.width, c.height));
            sub.clip_round(pr, 8);
            match d.s.previews.get(&(d.peer, m.id)) {
                Some(Preview::Ready(img)) => {
                    let (iw, ih) = (img.width as i32, img.height as i32);
                    if iw >= pw && ih >= ph {
                        sub.blit_smooth(pr, &img.pixels, iw, ih);
                    } else {
                        sub.blit_scaled(pr, &img.pixels, iw, ih, 256);
                    }
                }
                Some(Preview::Loading) => {
                    sub.fill(pr, grey);
                }
                Some(Preview::Failed) => {
                    sub.fill(pr, grey);
                    let what = m.file.as_ref().map_or("Photo", |f| f.kind.as_str());
                    sub.text_centered(pr, what, dim());
                }
                None => {
                    sub.fill(pr, grey);
                    if m.id != 0 {
                        d.want.push(m.id);
                    }
                }
            }
        }
        let video = m
            .file
            .as_ref()
            .is_some_and(|f| matches!(f.kind.as_str(), "Video" | "GIF" | "Video message"));
        if video {
            // a play button in the middle
            let (cx, cy) = (pr.x + pw / 2, pr.y + ph / 2);
            c.fill_round_alpha(Rect::new(cx - 22, cy - 22, 44, 44), 22, rgb(0, 0, 0), 120);
            c.fill_polygon(
                &[(cx - 7, cy - 11), (cx + 12, cy), (cx - 7, cy + 11)],
                rgb(0xff, 0xff, 0xff),
            );
        }
        // what it is, or how its download goes, in the corner
        let size = m
            .file
            .as_ref()
            .map_or_else(|| m.photo.as_ref().map_or(0, |p| p.big_size), |f| f.size);
        let corner = download_text(download, size).or_else(|| {
            m.file
                .as_ref()
                .map(|f| alloc::format!("{} \u{2022} {}", f.kind, size_text(f.size)))
        });
        if let Some(t) = corner {
            let t = fit(&UI, &t, pw - 24);
            let cw = UI.width(&t) + 14;
            let cr = Rect::new(pr.x + 6, pr.y + 6, cw, 22);
            c.fill_round_alpha(cr, 11, rgb(0, 0, 0), 110);
            c.text_centered(cr, &t, rgb(0xff, 0xff, 0xff));
        }
        if d.hover.as_ref() == Some(&Hit::Media(m.id)) {
            c.fill_round_alpha(pr, 8, rgb(0xff, 0xff, 0xff), 30);
        }
        d.hit(c, pr, Hit::Media(m.id));
        ty += ph + if bare_pic { 4 } else { 6 };
    }
    // a file: an icon, its name, its size or how its download goes
    if let (true, Some(f)) = (b.file_row, &m.file) {
        let row = Rect::new(x + PAD_X - 4, ty, w - 2 * PAD_X + 8, FILE_H);
        let hovered = d.hover.as_ref() == Some(&Hit::Media(m.id));
        let circle = Rect::new(x + PAD_X, ty + 4, 40, 40);
        let blue = if m.out {
            pick(rgb(0x78, 0xc2, 0x72), rgb(0x4a, 0x95, 0xd6))
        } else {
            BLUE
        };
        c.fill_round(
            circle,
            20,
            if hovered {
                mix(blue, rgb(0, 0, 0), 20)
            } else {
                blue
            },
        );
        let white = rgb(0xff, 0xff, 0xff);
        let (ax, ay) = (circle.x + 20, circle.y + 11);
        let saved = download.is_some_and(|d| d.path.is_some());
        if saved {
            // a page with a folded corner
            c.fill_rect(ax - 7, ay, 14, 18, white);
            c.fill_polygon(&[(ax + 3, ay), (ax + 7, ay), (ax + 7, ay + 4)], blue);
        } else {
            // an arrow down
            c.fill_rect(ax - 1, ay, 3, 12, white);
            c.fill_polygon(
                &[(ax - 7, ay + 10), (ax + 8, ay + 10), (ax, ay + 18)],
                white,
            );
        }
        let tx = circle.right() + 10;
        let tw = row.right() - tx - 4;
        rich::draw_fit(c, &UI_BOLD, tx, ty + 5, &f.name, tw, text());
        let info = download_text(download, f.size).unwrap_or_else(|| {
            if saved {
                alloc::format!("{} \u{2022} saved, click to open", size_text(f.size))
            } else {
                alloc::format!("{} \u{2022} {}", size_text(f.size), f.kind)
            }
        });
        c.draw_text(tx, ty + 25, &fit(&UI, &info, tw), tcolor);
        d.hit(c, row, Hit::Media(m.id));
        ty += FILE_H;
    }
    // the text
    let body = if m.failed { theme::error() } else { text() };
    let lc = link_color(m.out);
    for (i, l) in b.lines.iter().enumerate() {
        // the [Sticker] line of a message with something attached
        let color = if i == 0 && b.label {
            if m.out {
                time_out()
            } else {
                BLUE
            }
        } else {
            body
        };
        rich::draw_line(c, &UI, x + PAD_X, ty, l, color, lc);
        for (px, p) in l {
            if let Some(k) = p.link {
                if let Some(url) = b.links.get(k as usize) {
                    let hit = Hit::Link(url.clone());
                    d.hit(c, Rect::new(x + PAD_X + px, ty, p.w, LINE_H), hit);
                }
            }
        }
        ty += LINE_H;
    }
    // the link preview: a bar, the site, the title
    if let Some((site, title)) = &b.web {
        let wy = ty + 2;
        c.fill_round(Rect::new(x + PAD_X, wy, 3, WEB_H - 4), 1, lc);
        let tw = w - 2 * PAD_X - 12;
        rich::draw_fit(c, &UI_BOLD, x + PAD_X + 10, wy + 2, site, tw, lc);
        rich::draw_fit(c, &UI, x + PAD_X + 10, wy + LINE_H + 2, title, tw, text());
        ty += WEB_H + 4;
    }
    // the time, and ticks for our messages
    let mut time = String::new();
    if m.edited {
        time.push_str("edited ");
    }
    time.push_str(&clock(m.date + d.tz));
    let tw = UI.width(&time) + if m.out { 20 } else { 0 };
    let tx = x + w - PAD_X - tw;
    let (tyy, tcolor) = if bare_pic {
        // over the picture, on a dark pill
        let ty2 = ty - 4 - 26;
        let pr = Rect::new(tx - 8, ty2 - 2, tw + 12, 22);
        c.fill_round_alpha(pr, 11, rgb(0, 0, 0), 110);
        (ty2, rgb(0xff, 0xff, 0xff))
    } else if b.time_below {
        (ty - 2, tcolor)
    } else {
        (ty - LINE_H, tcolor)
    };
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
            if m.id <= d.read_out {
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
