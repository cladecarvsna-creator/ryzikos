//! EverBrowser: tabs along the top, a toolbar with Back, Reload (Stop
//! while loading), Home and the address bar, the page below it and a
//! status bar. Pages come from `crate::web`.
//!
//! Pages download, run their scripts and lay out on fibers (see
//! `crate::fiber`), a slice at a time, so the desktop keeps running while
//! a big page loads. Images download on up to four fibers per tab.

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::icons;
use super::text::UI;
use super::theme;
use super::webfont;
use super::App;
use super::{MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::keyboard::Key;
use crate::sync::IrqMutex;
use crate::web::dom::NodeId;
use crate::web::image::Image;
use crate::web::layout::{self, Control, Item};
use crate::web::url::Url;
use crate::web::{self, Nav, Page};
use crate::{interrupts, net};

pub const CLIENT_W: i32 = 1500;
pub const CLIENT_H: i32 = 900;
/// A program's own window: only the page, no tabs, toolbar or status bar.
pub const PROGRAM_W: i32 = 960;
pub const PROGRAM_H: i32 = 720;

const TABS_H: i32 = 40;
const TOOLBAR_H: i32 = 48;
/// Where the toolbar starts, and the page below it.
const BAR_Y: i32 = TABS_H;
const PAGE_Y: i32 = TABS_H + TOOLBAR_H;
const STATUS_H: i32 = 26;
const SCROLLBAR_W: i32 = 12;
const BUTTON: i32 = 34;

const MAX_TABS: usize = 12;
/// A new tab needs at least this much free memory; big pages take tens
/// of megabytes.
const MIN_FREE_FOR_TAB: usize = 40 * 1024 * 1024;
const TAB_MAX_W: i32 = 240;
/// Image downloads running at once in one tab.
const IMAGE_FIBERS: usize = 4;
/// While images keep arriving, lay the page out again at most this often.
const RELAYOUT_TICKS: u64 = interrupts::TIMER_HZ / 2;
/// Loading animations advance this often.
const ANIM_TICKS: u64 = interrupts::TIMER_HZ / 20;
/// Background work gets this long per pass of the desktop loop.
const WORK_TICKS: u64 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Page,
    Address,
    Field(NodeId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pressed {
    None,
    Back,
    Reload,
    Home,
    Go,
    /// Dragging the scroll bar thumb, grabbed this far from its top.
    Thumb(i32),
}

/// Parts of the tab strip, for pressing and hovering.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StripPart {
    Tab(usize),
    Close(usize),
    New,
}

/// A page downloading in the background.
struct Load {
    fiber: Fiber,
    out: Rc<RefCell<Option<Page>>>,
    nav: Nav,
    started: u64,
}

/// A queue of images to download, shared with the fibers doing it.
type ImageQueue = Rc<RefCell<VecDeque<(String, Option<Url>)>>>;
type ImagesDone = Rc<RefCell<Vec<(String, Image)>>>;

struct Tab {
    id: u32,
    /// In a program's own window: no toolbar, the page is everything.
    bare: bool,
    page: Page,
    scroll: i32,
    history: Vec<Nav>,
    current: Option<Nav>,
    /// Where to go next; the load starts on the next tick.
    pending: Option<Nav>,
    load: Option<Load>,
    image_fibers: Vec<Fiber>,
    image_queue: ImageQueue,
    images_done: ImagesDone,
    /// Scripts or images changed the page, and it needs laying out again.
    layout_due: bool,
    /// Layouts that nobody waits for run no sooner than this, so a big
    /// page that keeps changing does not take all the time.
    layout_ok_at: u64,
    address: String,
    /// Cursor in the address bar or a field, in characters.
    cursor: usize,
    /// The whole text is selected; typing replaces it.
    select_all: bool,
    focus: Focus,
    /// The text of the focused form field.
    field_text: String,
    pressed: Pressed,
    status: String,
    /// Where the link under the mouse goes.
    hover: Option<String>,
    /// A link asked for a new tab.
    open_in_new_tab: Option<Nav>,
    /// Stop (or Escape) was pressed while loading.
    stop_requested: bool,
}

/// What the page area of the window shows now, so drawing can skip the
/// page when nothing changed, or only move it when it scrolled.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Drawn {
    buffer: (usize, usize),
    tab: u32,
    generation: u64,
    scroll: i32,
    focus: Focus,
    field: u64,
}

pub struct Browser {
    /// A program's own window rather than the browser.
    bare: bool,
    tabs: Vec<Tab>,
    active: usize,
    strip_pressed: Option<StripPart>,
    strip_hover: Option<StripPart>,
    /// Cancelled fibers, resumed until they finish.
    draining: Vec<Fiber>,
    next_id: u32,
    net_started: bool,
    /// Loading animation frame.
    spin: u32,
    next_anim: u64,
    /// Which tab gets background work first, taking turns.
    turn: usize,
    drawn: Cell<Option<Drawn>>,
}

/// An address typed in the shell, for the browser to open.
static REQUESTED: IrqMutex<Option<String>> = IrqMutex::new(None);

pub fn request_address(address: &str) {
    *REQUESTED.lock() = Some(address.to_string());
}

/// Files and folders pages asked the desktop to open (`ryzikos:` links).
static DESKTOP_REQUESTS: IrqMutex<Vec<String>> = IrqMutex::new(Vec::new());

/// What a page asked the desktop for: `open:<path>` or `folder:<path>`.
pub fn take_desktop_request() -> Option<String> {
    let mut q = DESKTOP_REQUESTS.lock();
    if q.is_empty() {
        None
    } else {
        Some(q.remove(0))
    }
}

/// Whether a file is a web page or a downloaded program, for the browser.
pub fn is_page(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".html", ".htm", web::PROGRAM_EXT]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// An anti-aliased line `width` pixels thick, for toolbar icons.
fn stroke(c: &mut Canvas, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color) {
    let (minx, maxx) = (x0.min(x1) - width, x0.max(x1) + width);
    let (miny, maxy) = (y0.min(y1) - width, y0.max(y1) + width);
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = (dx * dx + dy * dy).max(0.0001);
    for py in miny as i32..=maxy as i32 + 1 {
        for px in minx as i32..=maxx as i32 + 1 {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let t = (((fx - x0) * dx + (fy - y0) * dy) / len2).clamp(0.0, 1.0);
            let (ex, ey) = (x0 + t * dx - fx, y0 + t * dy - fy);
            let d = sqrt(ex * ex + ey * ey);
            let cover = (width / 2.0 + 0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                c.blend_at(px, py, color, (cover * 256.0) as i32);
            }
        }
    }
}

/// The arrow in the corner of a desktop shortcut's icon.
pub fn shortcut_arrow(c: &mut Canvas, r: Rect) {
    let (x, y) = (r.x as f32, r.y as f32);
    let blue = 0x1a73e8;
    stroke(c, x + 4.5, y + 11.5, x + 11.0, y + 5.0, 2.0, blue);
    stroke(c, x + 6.5, y + 4.5, x + 11.5, y + 4.5, 2.0, blue);
    stroke(c, x + 11.5, y + 4.5, x + 11.5, y + 9.5, 2.0, blue);
}

fn sqrt(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v / 2.0 } else { 1.0 };
    for _ in 0..8 {
        x = 0.5 * (x + v / x);
    }
    x
}

fn back_rect() -> Rect {
    Rect::new(8, BAR_Y + 7, BUTTON, BUTTON)
}
fn reload_rect() -> Rect {
    Rect::new(8 + BUTTON + 4, BAR_Y + 7, BUTTON, BUTTON)
}
fn home_rect() -> Rect {
    Rect::new(8 + 2 * (BUTTON + 4), BAR_Y + 7, BUTTON, BUTTON)
}
fn address_rect() -> Rect {
    let x = 8 + 3 * (BUTTON + 4) + 6;
    Rect::new(x, BAR_Y + 7, CLIENT_W - x - 8 - 64 - 8, BUTTON)
}
fn go_rect() -> Rect {
    Rect::new(CLIENT_W - 8 - 64, BAR_Y + 7, 64, BUTTON)
}
fn content_rect(bare: bool) -> Rect {
    if bare {
        return Rect::new(0, 0, PROGRAM_W, PROGRAM_H);
    }
    Rect::new(
        0,
        PAGE_Y,
        CLIENT_W - SCROLLBAR_W,
        CLIENT_H - PAGE_Y - STATUS_H,
    )
}
fn scrollbar_rect(bare: bool) -> Rect {
    if bare {
        // no scroll bar: the wheel and the keys still scroll
        return Rect::new(PROGRAM_W, 0, 0, 0);
    }
    Rect::new(
        CLIENT_W - SCROLLBAR_W,
        PAGE_Y,
        SCROLLBAR_W,
        CLIENT_H - PAGE_Y - STATUS_H,
    )
}

/// The tabs in the strip, left to right.
fn tab_rects(n: usize) -> Vec<Rect> {
    let room = CLIENT_W - 16 - 44;
    let w = (room / n.max(1) as i32).clamp(56, TAB_MAX_W);
    (0..n)
        .map(|i| Rect::new(8 + i as i32 * w, 6, w, TABS_H - 6))
        .collect()
}

/// A tab's close button.
fn close_rect(tab: Rect) -> Rect {
    Rect::new(tab.right() - 30, tab.y + (tab.h - 22) / 2, 22, 22)
}

/// The "+" button after the last tab.
fn new_tab_rect(n: usize) -> Rect {
    let x = tab_rects(n).last().map_or(8, |r| r.right()) + 4;
    Rect::new(x, 9, 30, 28)
}

/// The page's viewport: the content area.
fn viewport(bare: bool) -> (i32, i32) {
    (content_rect(bare).w, content_rect(bare).h)
}

/// A web colour (0xAARRGGBB) as a desktop colour and its opacity (0 to 256).
fn web_color(c: u32) -> (Color, i32) {
    let a = (c >> 24) as i32;
    (c & 0xff_ffff, if a >= 255 { 256 } else { a })
}

fn hash_text(s: &str, extra: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ extra;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

impl Browser {
    pub fn new() -> Self {
        Browser {
            bare: false,
            tabs: alloc::vec![Tab::new(1, None, false)],
            active: 0,
            strip_pressed: None,
            strip_hover: None,
            draining: Vec::new(),
            next_id: 2,
            net_started: false,
            spin: 0,
            next_anim: 0,
            turn: 0,
            drawn: Cell::new(None),
        }
    }

    /// A window for running one program, without the browser's tabs,
    /// toolbar and status bar.
    pub fn program() -> Self {
        let mut b = Browser::new();
        b.bare = true;
        b.tabs = alloc::vec![Tab::new(1, None, true)];
        b
    }

    /// Show a page or run a program from the disk.
    pub fn open_file(&mut self, path: &str) {
        if self.bare {
            // a new program starts clean: no way back to the last one
            let mut tab = Tab::new(self.next_id, None, true);
            self.next_id += 1;
            self.tabs[0].stop(&mut self.draining);
            self.tabs[0].reset_images(&mut self.draining);
            tab.navigate(Nav::Special(alloc::format!("file:{}", path)));
            tab.current = None;
            self.tabs[0] = tab;
            self.drawn.set(None);
            return;
        }
        self.tab().navigate(Nav::Special(alloc::format!("file:{}", path)));
    }

    /// The program's name, for the title bar: the page's title, or the
    /// file name while it loads.
    pub fn program_title(&self) -> String {
        let tab = &self.tabs[0];
        let t = tab.page.title();
        let t = t.trim();
        if !tab.loading() && !t.is_empty() {
            return t.to_string();
        }
        let name = crate::fs::file_name(tab.address.trim_start_matches("file:"));
        String::from(name.strip_suffix(web::PROGRAM_EXT).unwrap_or(name))
    }

    /// Stop the program when its window closes.
    pub fn close_program(&mut self) {
        self.tabs[0].stop(&mut self.draining);
        self.tabs[0].reset_images(&mut self.draining);
        self.tabs[0] = Tab::new(self.next_id, None, true);
        self.next_id += 1;
        self.drawn.set(None);
    }

    /// Start the network card when the browser first opens.
    pub fn start(&mut self) {
        if !self.net_started && !self.bare {
            self.net_started = true;
            let status =
                match net::init() {
                    Some(mac) => alloc::format!(
                    "Network card {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, getting an address...",
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
                ),
                    None => String::from(net::NO_CARD),
                };
            self.tabs[self.active].status = status;
        }
    }

    /// Whether pages are loading: the desktop should not sleep then.
    pub fn busy(&self) -> bool {
        !self.draining.is_empty()
            || self
                .tabs
                .iter()
                .any(|t| t.load.is_some() || t.pending.is_some() || !t.image_fibers.is_empty())
    }

    fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    /// Open a tab after the current one and switch to it.
    fn new_tab(&mut self, nav: Option<Nav>) {
        if self.tabs.len() >= MAX_TABS || crate::heap::free_bytes() < MIN_FREE_FOR_TAB {
            self.tab().status = String::from("No room for another tab: close one first");
            // no room: go there in this tab instead
            if let Some(nav) = nav {
                self.tab().navigate(nav);
            }
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let focus_address = nav.is_none();
        let mut tab = Tab::new(id, nav, self.bare);
        if focus_address {
            tab.focus_address();
        }
        self.active += 1;
        self.tabs.insert(self.active, tab);
        crate::serial::write_str("\nbrowser: new tab\n");
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs.remove(i);
        tab.stop(&mut self.draining);
        tab.reset_images(&mut self.draining);
        if self.tabs.is_empty() {
            // the last tab closed: start over with the home page
            let id = self.next_id;
            self.next_id += 1;
            self.tabs.push(Tab::new(id, None, self.bare));
        }
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        crate::serial::write_str("\nbrowser: closed a tab\n");
    }

    fn switch_to(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
        }
    }

    /// Called every pass of the desktop loop. Returns true to redraw.
    pub fn tick(&mut self) -> bool {
        net::poll();
        let mut redraw = false;
        let requested = if self.bare { None } else { REQUESTED.lock().take() };
        if let Some(address) = requested {
            if web::is_special(&address) {
                self.tab().navigate(Nav::Special(String::from(address.trim())));
                redraw = true;
            } else if let Some(u) = web::address_to_url(&address) {
                self.tab().navigate(Nav::Get(u));
                redraw = true;
            }
        }
        let deadline = interrupts::ticks() + WORK_TICKS;
        // stopped work first: it ends quickly and frees its memory
        self.draining.retain_mut(|f| !f.resume());
        let n = self.tabs.len();
        self.turn = (self.turn + 1) % n;
        for k in 0..n {
            let i = (self.turn + k) % n;
            let changed = self.tabs[i].tick(deadline, &mut self.draining);
            redraw |= changed && i == self.active;
            if let Some(nav) = self.tabs[i].open_in_new_tab.take() {
                if self.bare {
                    // a program has one page: links go there
                    self.tabs[i].navigate(nav);
                    redraw = true;
                    continue;
                }
                self.active = i;
                self.new_tab(Some(nav));
                redraw = true;
            }
        }
        // the spinning loading icons
        let now = interrupts::ticks();
        if now >= self.next_anim && self.tabs.iter().any(|t| t.loading()) {
            self.next_anim = now + ANIM_TICKS;
            self.spin = self.spin.wrapping_add(1);
            redraw = true;
        }
        let tab = self.tab();
        if tab.status.starts_with("Network card") && net::configured() {
            tab.status = alloc::format!("Online, address {}", net::address().unwrap_or_default());
            redraw = true;
        }
        redraw
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        if self.bare {
            let r = self.tab().on_key(key);
            if let Some(nav) = self.tab().open_in_new_tab.take() {
                self.tab().navigate(nav);
            }
            return r;
        }
        match key {
            Key::Ctrl('t') => {
                self.new_tab(None);
                true
            }
            Key::Ctrl('w') => {
                self.close_tab(self.active);
                true
            }
            Key::Ctrl('\t') => {
                let n = self.tabs.len();
                let next = if crate::keyboard::shift_held() {
                    (self.active + n - 1) % n
                } else {
                    (self.active + 1) % n
                };
                self.switch_to(next);
                true
            }
            Key::Ctrl(d @ '1'..='9') => {
                let i = if d == '9' {
                    self.tabs.len() - 1
                } else {
                    d as usize - '1' as usize
                };
                self.switch_to(i);
                true
            }
            _ => {
                let r = self.tab().on_key(key);
                self.take_new_tab();
                r
            }
        }
    }

    fn take_new_tab(&mut self) {
        if let Some(nav) = self.tab().open_in_new_tab.take() {
            if self.bare {
                self.tab().navigate(nav);
                return;
            }
            self.new_tab(Some(nav));
        }
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        self.tab().scroll_by(clicks * 60)
    }

    fn strip_at(&self, x: i32, y: i32) -> Option<StripPart> {
        if y >= TABS_H || self.bare {
            return None;
        }
        let rects = tab_rects(self.tabs.len());
        for (i, r) in rects.iter().enumerate() {
            if r.contains(x, y) {
                let closable = r.w >= 100 || i == self.active;
                if closable && close_rect(*r).contains(x, y) {
                    return Some(StripPart::Close(i));
                }
                return Some(StripPart::Tab(i));
            }
        }
        if self.tabs.len() < MAX_TABS && new_tab_rect(self.tabs.len()).contains(x, y) {
            return Some(StripPart::New);
        }
        None
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Down { right: false } if ev.y < TABS_H && !self.bare => {
                let part = self.strip_at(ev.x, ev.y);
                if let Some(StripPart::Tab(i)) = part {
                    self.switch_to(i);
                }
                self.strip_pressed = part;
                true
            }
            MouseKind::Up if self.strip_pressed.is_some() => {
                let pressed = self.strip_pressed.take();
                if pressed == self.strip_at(ev.x, ev.y) {
                    match pressed {
                        Some(StripPart::Close(i)) => self.close_tab(i),
                        Some(StripPart::New) => self.new_tab(None),
                        _ => {}
                    }
                }
                true
            }
            _ => {
                let r = self.tab().on_mouse(ev);
                self.take_new_tab();
                r
            }
        }
    }

    /// The mouse moved over the window without a button held.
    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let part = self.strip_at(x, y);
        let strip = part != self.strip_hover;
        self.strip_hover = part;
        self.tab().on_hover(x, y) | strip
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas) {
        let tab = &self.tabs[self.active];
        if self.bare {
            self.draw_page_area(c, tab);
            return;
        }
        self.draw_tabs(c);
        tab.draw_toolbar(c, self.spin);
        self.draw_page_area(c, tab);
        tab.draw_scrollbar(c);
        tab.draw_status(c);
    }

    /// Draw the page, or only what scrolling uncovered, or nothing when
    /// it looks the same as last time.
    fn draw_page_area(&self, c: &mut Canvas, tab: &Tab) {
        let area = content_rect(self.bare);
        let now = Drawn {
            buffer: c.buffer_id(),
            tab: tab.id,
            generation: tab.page.generation,
            scroll: tab.scroll,
            focus: tab.focus,
            field: if matches!(tab.focus, Focus::Field(_)) {
                hash_text(&tab.field_text, tab.cursor as u64)
            } else {
                0
            },
        };
        let before = self.drawn.replace(Some(now));
        let view = Rect::new(0, 0, area.w, area.h);
        let band = match before {
            Some(b) if b == now => return,
            Some(b)
                if Drawn {
                    scroll: now.scroll,
                    ..b
                } == now
                    && (now.scroll - b.scroll).abs() < area.h / 2 =>
            {
                let dy = now.scroll - b.scroll;
                c.sub(area).shift_up(view, dy);
                if dy > 0 {
                    Rect::new(0, area.h - dy, area.w, dy)
                } else {
                    Rect::new(0, 0, area.w, -dy)
                }
            }
            _ => view,
        };
        tab.draw_page(c, band);
    }

    fn draw_tabs(&self, c: &mut Canvas) {
        let strip = mix(theme::FACE, theme::SHADOW, 70);
        c.fill(Rect::new(0, 0, CLIENT_W, TABS_H), strip);
        let rects = tab_rects(self.tabs.len());
        for (i, r) in rects.iter().enumerate() {
            let tab = &self.tabs[i];
            let active = i == self.active;
            let hover =
                matches!(self.strip_hover, Some(StripPart::Tab(h) | StripPart::Close(h)) if h == i);
            if active {
                // joined to the toolbar below, rounded on top
                c.fill_round(Rect::new(r.x, r.y, r.w, r.h + 10), 8, theme::FACE);
            } else {
                if hover {
                    c.fill_round(r.inset(2), 7, mix(strip, theme::FACE, 150));
                }
                // a thin line between tabs that are not next to the active one
                if i + 1 < rects.len() && i + 1 != self.active {
                    c.fill_rect(r.right() - 1, r.y + 9, 1, r.h - 16, theme::SHADOW);
                }
            }
            // icon: spinning while loading
            let (ix, iy) = (r.x + 12, r.y + (r.h - 16) / 2);
            if tab.loading() {
                draw_spinner(
                    c,
                    ix as f32 + 8.0,
                    iy as f32 + 8.0,
                    self.spin,
                    theme::ACCENT,
                );
            } else {
                icons::get().draw_small(c, App::Browser, ix, iy);
            }
            let closable = r.w >= 100 || active;
            let text_right = if closable {
                close_rect(*r).x - 4
            } else {
                r.right() - 8
            };
            let title = tab.title();
            let ty = r.y + (r.h - UI.line_height) / 2;
            let mut t = c.sub(Rect::new(ix + 22, r.y, (text_right - ix - 22).max(0), r.h));
            let color = if active {
                theme::TEXT
            } else {
                mix(theme::TEXT, theme::TEXT_DIM, 140)
            };
            t.draw_text(0, ty - r.y, &title, color);
            if closable {
                let cr = close_rect(*r);
                let pressed = self.strip_pressed == Some(StripPart::Close(i));
                if pressed || self.strip_hover == Some(StripPart::Close(i)) {
                    c.fill_round(
                        cr,
                        4,
                        mix(strip, theme::SHADOW, if pressed { 200 } else { 110 }),
                    );
                }
                let (cx, cy) = (cr.x as f32 + 11.0, cr.y as f32 + 11.0);
                stroke(c, cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0, 1.4, theme::TEXT);
                stroke(c, cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0, 1.4, theme::TEXT);
            }
        }
        if self.tabs.len() < MAX_TABS {
            let r = new_tab_rect(self.tabs.len());
            let pressed = self.strip_pressed == Some(StripPart::New);
            if pressed || self.strip_hover == Some(StripPart::New) {
                c.fill_round(
                    r,
                    6,
                    mix(strip, theme::SHADOW, if pressed { 200 } else { 110 }),
                );
            }
            let (cx, cy) = (r.x as f32 + 15.0, r.y as f32 + 14.0);
            stroke(c, cx - 6.0, cy, cx + 6.0, cy, 1.6, theme::TEXT);
            stroke(c, cx, cy - 6.0, cx, cy + 6.0, 1.6, theme::TEXT);
        }
    }
}

/// A turning arc, the loading sign.
fn draw_spinner(c: &mut Canvas, cx: f32, cy: f32, frame: u32, color: Color) {
    let start = frame as f32 * 0.45;
    let steps = 10;
    let mut prev = None;
    for i in 0..=steps {
        let a = start + 4.2 * i as f32 / steps as f32;
        let p = (cx + 6.0 * cos(a), cy + 6.0 * sin(a));
        if let Some((px, py)) = prev {
            stroke(c, px, py, p.0, p.1, 2.0, color);
        }
        prev = Some(p);
    }
}

impl Tab {
    /// A tab going to `nav`, or showing the home page.
    fn new(id: u32, nav: Option<Nav>, bare: bool) -> Tab {
        let mut tab = Tab {
            id,
            bare,
            page: web::home(viewport(bare)),
            scroll: 0,
            history: Vec::new(),
            current: Some(Nav::Home),
            pending: None,
            load: None,
            image_fibers: Vec::new(),
            image_queue: Rc::new(RefCell::new(VecDeque::new())),
            images_done: Rc::new(RefCell::new(Vec::new())),
            layout_due: false,
            layout_ok_at: 0,
            address: String::new(),
            cursor: 0,
            select_all: false,
            focus: if bare { Focus::Page } else { Focus::Address },
            field_text: String::new(),
            pressed: Pressed::None,
            status: String::new(),
            hover: None,
            open_in_new_tab: None,
            stop_requested: false,
        };
        if let Some(nav) = nav {
            tab.navigate(nav);
        }
        tab
    }

    fn loading(&self) -> bool {
        self.load.is_some() || self.pending.is_some()
    }

    fn title(&self) -> String {
        if self.loading() {
            return String::from("Loading...");
        }
        let t = self.page.title();
        let t = t.trim();
        if !t.is_empty() {
            return t.to_string();
        }
        match &self.page.url {
            Some(u) => u.host.clone(),
            None => String::from("New tab"),
        }
    }

    /// Background work for this tab. Returns true if the page changed.
    fn tick(&mut self, deadline: u64, draining: &mut Vec<Fiber>) -> bool {
        let mut changed = false;
        if core::mem::take(&mut self.stop_requested) {
            self.stop(draining);
            self.status = String::from("Stopped");
            self.address = self
                .page
                .url
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_default();
            changed = true;
        }
        if let Some(nav) = self.pending.take() {
            self.start_load(nav, draining);
            changed = true;
        }
        if interrupts::ticks() < deadline {
            if let Some(load) = &mut self.load {
                if load.fiber.resume() {
                    let load = self.load.take().unwrap();
                    let page = load.out.borrow_mut().take();
                    if let Some(page) = page {
                        self.finish_load(load.nav, page, load.started, draining);
                    }
                    changed = true;
                }
            }
        }
        if self.page.run_timers(false) {
            // what timers change shows at the next layout
            self.script_actions();
            self.layout_due = true;
        }
        changed |= self.pump_images(deadline);
        if self.layout_due && interrupts::ticks() >= self.layout_ok_at {
            changed |= self.relayout();
        }
        changed
    }

    /// Lay the page out again if it changed. Returns true if it did.
    fn relayout(&mut self) -> bool {
        self.layout_due = false;
        if !self.page.update() {
            return false;
        }
        // wait four times as long as it took before the next one
        let cost = self.page.layout_ms.max(0) as u64 * interrupts::TIMER_HZ / 1000;
        self.layout_ok_at = interrupts::ticks() + (4 * cost).max(RELAYOUT_TICKS);
        self.scroll = self.scroll.clamp(0, self.max_scroll());
        true
    }

    fn start_load(&mut self, nav: Nav, draining: &mut Vec<Fiber>) {
        self.stop(draining);
        let out: Rc<RefCell<Option<Page>>> = Rc::new(RefCell::new(None));
        let (o, n) = (out.clone(), nav.clone());
        let vp = viewport(self.bare);
        let fiber = Fiber::new(move || {
            let page = match &n {
                Nav::Home => web::home(vp),
                Nav::Get(u) => web::load(u, None, vp, true),
                Nav::Post(u, body) => web::load(u, Some(body), vp, true),
                Nav::Special(a) => web::special(a, vp),
            };
            *o.borrow_mut() = Some(page);
        });
        self.load = Some(Load {
            fiber,
            out,
            nav,
            started: interrupts::ticks(),
        });
    }

    /// Stop loading, keeping the page that is showing.
    fn stop(&mut self, draining: &mut Vec<Fiber>) {
        if let Some(mut load) = self.load.take() {
            load.fiber.cancel();
            draining.push(load.fiber);
        }
        self.pending = None;
    }

    /// Forget the images of the page that is going away.
    fn reset_images(&mut self, draining: &mut Vec<Fiber>) {
        for mut f in self.image_fibers.drain(..) {
            f.cancel();
            draining.push(f);
        }
        self.image_queue.borrow_mut().clear();
        self.image_queue = Rc::new(RefCell::new(VecDeque::new()));
        self.images_done = Rc::new(RefCell::new(Vec::new()));
        self.layout_due = false;
    }

    fn finish_load(&mut self, nav: Nav, page: Page, started: u64, draining: &mut Vec<Fiber>) {
        if let Some(prev) = self.current.take() {
            if self.history.len() >= 64 {
                self.history.remove(0);
            }
            self.history.push(prev);
        }
        // a redirect or a POST leaves us at a plain address
        self.current = Some(match (&nav, &page.url) {
            (Nav::Home, _) => Nav::Home,
            (Nav::Special(a), _) => Nav::Special(a.clone()),
            (_, Some(u)) => Nav::Get(u.clone()),
            _ => nav.clone(),
        });
        self.reset_images(draining);
        self.set_page(page);
        if let Nav::Special(a) = &nav {
            self.address = a.clone();
        }
        let secs = (interrupts::ticks() - started) as f32 / interrupts::TIMER_HZ as f32;
        self.status = alloc::format!("Done in {}.{} s", secs as u32, (secs * 10.0) as u32 % 10);
        let mut line = crate::StackString::<64>::new();
        let _ = core::fmt::write(
            &mut line,
            format_args!(
                "\nbrowser: showing page, {} MiB free\n",
                crate::heap::free_bytes() >> 20
            ),
        );
        crate::serial::write_str(line.as_str());
        // a script may have moved on straight away
        self.after_script();
    }

    /// Hand new images to the download fibers and take in what arrived.
    fn pump_images(&mut self, deadline: u64) -> bool {
        if self.load.is_none() {
            let new = self.page.take_image_queue();
            if !new.is_empty() {
                self.image_queue.borrow_mut().extend(new);
            }
        }
        let waiting = self.image_queue.borrow().len();
        while self.image_fibers.len() < IMAGE_FIBERS && self.image_fibers.len() < waiting {
            let (queue, done) = (self.image_queue.clone(), self.images_done.clone());
            self.image_fibers.push(Fiber::new(move || loop {
                let next = queue.borrow_mut().pop_front();
                let Some((src, url)) = next else {
                    break;
                };
                let img = web::page::fetch_image(&src, url.as_ref());
                done.borrow_mut().push((src, img));
            }));
        }
        for f in self.image_fibers.iter_mut() {
            if interrupts::ticks() >= deadline {
                break;
            }
            f.resume();
        }
        self.image_fibers.retain(|f| !f.done());
        let arrived = core::mem::take(&mut *self.images_done.borrow_mut());
        let changed = !arrived.is_empty();
        for (src, img) in arrived {
            self.layout_due |= self.page.add_image(src, img);
        }
        changed
    }

    /// Act on what a script asked for, and show what it changed.
    fn after_script(&mut self) {
        self.script_actions();
        self.relayout();
    }

    /// Act on what a script asked for: going somewhere, back, scrolling.
    fn script_actions(&mut self) {
        if let Some(nav) = self.page.st.nav.take() {
            self.navigate(nav);
        }
        if core::mem::take(&mut self.page.st.back) {
            self.back();
        }
        if let Some(n) = self.page.st.scroll_to.take() {
            if let Some(y) = self.page.element_top(n) {
                self.scroll = y.clamp(0, self.max_scroll());
            }
        }
        // the address follows history.pushState
        if self.focus != Focus::Address && !self.loading() {
            if let Some(u) = &self.page.url {
                self.address = u.to_string();
            }
        }
    }

    fn navigate(&mut self, nav: Nav) {
        if let Nav::Special(a) = &nav {
            if let Some(req) = a.strip_prefix("ryzikos:") {
                self.desktop_request(req);
                return;
            }
        }
        self.status = match &nav {
            Nav::Home => String::from("Opening the home page..."),
            Nav::Get(u) | Nav::Post(u, _) => alloc::format!("Loading {} ...", u),
            Nav::Special(a) => alloc::format!("Opening {} ...", a),
        };
        if let Nav::Special(a) = &nav {
            self.address = a.clone();
        }
        if let Nav::Get(u) | Nav::Post(u, _) = &nav {
            self.address = u.to_string();
        }
        self.pending = Some(nav);
        self.focus = Focus::Page;
    }

    /// A `ryzikos:` link: open a file or folder on the desktop, or install
    /// a program from the disc.
    fn desktop_request(&mut self, req: &str) {
        if let Some(from) = req.strip_prefix("install:") {
            let dir = web::programs_folder();
            let _ = crate::fs::create_dir(&dir);
            let to = crate::fs::join(&dir, crate::fs::file_name(from));
            self.status = match crate::fs::read(from).and_then(|d| crate::fs::write(&to, &d)) {
                Ok(()) => {
                    let name = crate::fs::file_name(from);
                    web::add_shortcut(name.trim_end_matches(web::PROGRAM_EXT), &to);
                    alloc::format!("Installed {}", name)
                }
                Err(e) => String::from(e.message()),
            };
            self.pending = Some(Nav::Special(String::from("about:programs")));
            return;
        }
        let known = req.starts_with("open:") || req.starts_with("folder:");
        if known {
            DESKTOP_REQUESTS.lock().push(String::from(req));
        }
    }

    fn set_page(&mut self, page: Page) {
        self.address = match &page.url {
            Some(u) => u.to_string(),
            None => String::new(),
        };
        self.page = page;
        self.scroll = 0;
        self.hover = None;
        self.select_all = false;
    }

    fn back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.current = None; // do not push the page we leave
            self.navigate(prev);
        }
    }

    fn reload(&mut self) {
        if let Some(cur) = self.current.clone() {
            self.current = None;
            let cur = match cur {
                Nav::Post(u, _) => Nav::Get(u),
                n => n,
            };
            self.navigate(cur);
        }
    }

    fn max_scroll(&self) -> i32 {
        (self.page.layout.height - content_rect(self.bare).h).max(0)
    }

    fn scroll_by(&mut self, dy: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + dy).clamp(0, self.max_scroll());
        self.scroll != old
    }

    // ---- editing the address bar and form fields --------------------------

    fn edit_text(&mut self) -> Option<&mut String> {
        match self.focus {
            Focus::Address => Some(&mut self.address),
            Focus::Field(_) => Some(&mut self.field_text),
            Focus::Page => None,
        }
    }

    fn edit_key(&mut self, key: Key) -> bool {
        let mut cursor = self.cursor;
        let select_all = core::mem::replace(&mut self.select_all, false);
        let Some(text) = self.edit_text() else {
            return false;
        };
        let len = text.chars().count();
        cursor = cursor.min(len);
        let byte = |t: &String, i: usize| t.char_indices().nth(i).map_or(t.len(), |(b, _)| b);
        match key {
            Key::Char(c) if c != '\t' => {
                if select_all {
                    text.clear();
                    cursor = 0;
                }
                if text.len() < 1024 {
                    let b = byte(text, cursor);
                    text.insert(b, c);
                    cursor += 1;
                }
            }
            Key::Backspace => {
                if select_all {
                    text.clear();
                    cursor = 0;
                } else if cursor > 0 {
                    let b = byte(text, cursor - 1);
                    text.remove(b);
                    cursor -= 1;
                }
            }
            Key::Delete => {
                if select_all {
                    text.clear();
                    cursor = 0;
                } else if cursor < len {
                    let b = byte(text, cursor);
                    text.remove(b);
                }
            }
            Key::Left => cursor = cursor.saturating_sub(1),
            Key::Right => cursor = (cursor + 1).min(len),
            Key::Home => cursor = 0,
            Key::End => cursor = len,
            Key::Ctrl('a') => {
                self.select_all = true;
                self.cursor = len;
                return true;
            }
            // there is no partial selection here: copy and cut take the
            // whole text
            Key::Ctrl('c') | Key::Ctrl('x') => {
                if !text.is_empty() {
                    super::widgets::copy(text);
                }
                if matches!(key, Key::Ctrl('x')) {
                    text.clear();
                    cursor = 0;
                }
            }
            Key::Ctrl('v') => {
                let clip = super::widgets::paste();
                let clip: String = clip.chars().filter(|c| !c.is_control()).collect();
                if select_all {
                    text.clear();
                    cursor = 0;
                }
                if text.len() + clip.len() <= 4096 {
                    let b = byte(text, cursor);
                    text.insert_str(b, &clip);
                    cursor += clip.chars().count();
                }
            }
            Key::Enter => {
                match self.focus {
                    Focus::Address => {
                        let text = self.address.trim().to_string();
                        if text.is_empty() || text.eq_ignore_ascii_case(web::HOME) {
                            self.navigate(Nav::Home);
                        } else if web::is_special(&text) {
                            self.navigate(Nav::Special(text));
                        } else if let Some(u) = web::address_to_url(&text) {
                            self.navigate(Nav::Get(u));
                        }
                    }
                    Focus::Field(n) => {
                        if let Some(form) = self.page.form_of(n) {
                            let nav = self.page.submit(form, None);
                            if let Some(nav) = nav {
                                self.navigate(nav);
                            }
                        }
                        self.after_script();
                    }
                    Focus::Page => {}
                }
                return true;
            }
            Key::Escape => {
                if self.focus == Focus::Address {
                    self.address = self
                        .page
                        .url
                        .as_ref()
                        .map(|u| u.to_string())
                        .unwrap_or_default();
                }
                self.focus = Focus::Page;
                return true;
            }
            _ => return false,
        }
        self.cursor = cursor;
        if let Focus::Field(n) = self.focus {
            let value = self.field_text.clone();
            self.page.set_field(n, &value);
            self.after_script();
        }
        true
    }

    fn on_key(&mut self, key: Key) -> bool {
        if self.focus != Focus::Page {
            return self.edit_key(key);
        }
        let mut letter = [0u8; 4];
        let name = match key {
            Key::Up => "ArrowUp",
            Key::Down => "ArrowDown",
            Key::Left => "ArrowLeft",
            Key::Right => "ArrowRight",
            Key::Enter => "Enter",
            Key::Escape => "Escape",
            // letters, digits and space reach the page's scripts too
            Key::Char(c) if c.is_alphanumeric() || c == ' ' => c.encode_utf8(&mut letter),
            _ => "",
        };
        if !name.is_empty() {
            let prevented = !self.page.key_event(None, name);
            self.after_script();
            if prevented {
                return true;
            }
        }
        let page = content_rect(self.bare).h - 40;
        match key {
            Key::Up => self.scroll_by(-40),
            Key::Down => self.scroll_by(40),
            Key::PageUp => self.scroll_by(-page),
            Key::PageDown | Key::Char(' ') => self.scroll_by(page),
            Key::Home => self.scroll_by(-self.scroll),
            Key::End => self.scroll_by(self.max_scroll()),
            Key::Backspace if !self.bare => {
                self.back();
                true
            }
            Key::Ctrl('l') if !self.bare => {
                self.focus_address();
                true
            }
            Key::Ctrl('r') | Key::Function(5) => {
                self.reload();
                true
            }
            Key::Escape if self.loading() => {
                self.stop_requested = true;
                true
            }
            _ => false,
        }
    }

    /// Where the address bar shows its text (right of the padlock).
    fn address_text_rect(&self) -> Rect {
        let r = address_rect();
        let lock = self.page.url.as_ref().is_some_and(|u| u.https) && self.focus != Focus::Address;
        let x = r.x + if lock { 32 } else { 14 };
        Rect::new(x, r.y, r.right() - 14 - x, r.h)
    }

    fn focus_address(&mut self) {
        self.focus = Focus::Address;
        self.select_all = true;
        self.cursor = self.address.chars().count();
    }

    fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Down { right: false } => self.press(ev.x, ev.y),
            MouseKind::Down { right: true } => false,
            MouseKind::Move => {
                if let Pressed::Thumb(grab) = self.pressed {
                    let track = scrollbar_rect(self.bare);
                    let (_, thumb_h) = self.thumb();
                    let room = (track.h - thumb_h).max(1);
                    let top = ev.y - grab - track.y;
                    let scroll = top * self.max_scroll() / room;
                    let old = self.scroll;
                    self.scroll = scroll.clamp(0, self.max_scroll());
                    return old != self.scroll;
                }
                false
            }
            MouseKind::Up => {
                let pressed = core::mem::replace(&mut self.pressed, Pressed::None);
                let inside = |r: Rect| r.contains(ev.x, ev.y);
                match pressed {
                    Pressed::Back if inside(back_rect()) => self.back(),
                    Pressed::Reload if inside(reload_rect()) => {
                        if self.loading() {
                            self.stop_requested = true;
                        } else {
                            self.reload();
                        }
                    }
                    Pressed::Home if inside(home_rect()) => self.navigate(Nav::Home),
                    Pressed::Go if inside(go_rect()) => {
                        self.focus = Focus::Address;
                        self.edit_key(Key::Enter);
                    }
                    _ => {}
                }
                pressed != Pressed::None
            }
        }
    }

    /// The mouse moved over the window without a button held.
    fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let area = content_rect(self.bare);
        let mut hover = None;
        if area.contains(x, y) {
            let (px, py) = (x - area.x, y - area.y + self.scroll);
            if let Some(a) = self
                .page
                .element_at(px, py)
                .and_then(|n| self.page.link_of(n))
            {
                hover = self.page.link_target(a);
            }
        }
        if hover != self.hover {
            self.hover = hover;
            return true;
        }
        false
    }

    /// Top and height of the scroll bar thumb.
    fn thumb(&self) -> (i32, i32) {
        let track = scrollbar_rect(self.bare);
        let total = self.page.layout.height;
        let view = content_rect(self.bare).h;
        if total <= view {
            return (track.y, track.h);
        }
        let h = (track.h * view / total).max(30);
        let top = track.y + (track.h - h) * self.scroll / self.max_scroll().max(1);
        (top, h)
    }

    fn press(&mut self, x: i32, y: i32) -> bool {
        let bare = self.bare;
        let hit = |r: Rect| r.contains(x, y);
        let tool = |r: Rect| !bare && r.contains(x, y);
        if tool(back_rect()) {
            self.pressed = Pressed::Back;
            return true;
        }
        if tool(reload_rect()) {
            self.pressed = Pressed::Reload;
            return true;
        }
        if tool(home_rect()) {
            self.pressed = Pressed::Home;
            return true;
        }
        if tool(go_rect()) {
            self.pressed = Pressed::Go;
            return true;
        }
        if tool(address_rect()) {
            if self.focus == Focus::Address && !self.select_all {
                // place the cursor where clicked
                self.cursor = click_cursor(self.address_text_rect(), &self.address, self.cursor, x);
            } else {
                self.focus_address();
            }
            return true;
        }
        if hit(scrollbar_rect(self.bare)) {
            let (top, h) = self.thumb();
            if y >= top && y < top + h {
                self.pressed = Pressed::Thumb(y - top);
            } else {
                let page = content_rect(self.bare).h - 40;
                self.scroll_by(if y < top { -page } else { page });
            }
            return true;
        }
        if hit(content_rect(self.bare)) {
            let area = content_rect(self.bare);
            let (px, py) = (x - area.x, y - area.y + self.scroll);
            self.focus = Focus::Page;
            let Some(node) = self.page.element_at(px, py) else {
                return true;
            };
            // a text field takes the focus
            let tag = self.page.dom.tag(node).to_string();
            let ty = self
                .page
                .dom
                .attr(node, "type")
                .unwrap_or("text")
                .to_ascii_lowercase();
            let is_field = tag == "textarea"
                || tag == "input"
                    && !matches!(
                        ty.as_str(),
                        "submit"
                            | "button"
                            | "reset"
                            | "checkbox"
                            | "radio"
                            | "image"
                            | "hidden"
                            | "file"
                    );
            self.page.dispatch(node, "mousedown", px, py);
            self.page.dispatch(node, "mouseup", px, py);
            if is_field {
                self.focus = Focus::Field(node);
                self.field_text = self.page.field_value(node);
                self.cursor = self.field_text.chars().count();
                self.select_all = false;
                self.page.dispatch(node, "focus", px, py);
            }
            if self.page.dispatch(node, "click", px, py) {
                let new_tab = self.page.opens_new_tab(node);
                if let Some(nav) = self.page.default_action(node) {
                    if new_tab {
                        self.open_in_new_tab = Some(nav);
                    } else {
                        self.navigate(nav);
                    }
                }
            }
            self.after_script();
            return true;
        }
        false
    }

    // ---- drawing -----------------------------------------------------------

    fn draw_toolbar(&self, c: &mut Canvas, spin: u32) {
        let bar = Rect::new(0, BAR_Y, CLIENT_W, TOOLBAR_H);
        c.fill(bar, theme::FACE);
        c.fill_rect(0, BAR_Y + TOOLBAR_H - 1, CLIENT_W, 1, theme::STROKE);

        let icon_button = |c: &mut Canvas, r: Rect, pressed: bool, enabled: bool| {
            if pressed {
                c.fill_round(
                    r,
                    theme::CONTROL_RADIUS,
                    mix(theme::FACE, theme::SHADOW, 90),
                );
            }
            if enabled {
                theme::TEXT
            } else {
                mix(theme::TEXT_DIM, theme::FACE, 110)
            }
        };

        // back: an arrow
        let r = back_rect();
        let col = icon_button(
            c,
            r,
            self.pressed == Pressed::Back,
            !self.history.is_empty(),
        );
        let (cx, cy) = (r.x as f32 + 17.0, r.y as f32 + 17.0);
        stroke(c, cx - 7.0, cy, cx + 7.0, cy, 2.0, col);
        stroke(c, cx - 7.0, cy, cx - 1.0, cy - 6.0, 2.0, col);
        stroke(c, cx - 7.0, cy, cx - 1.0, cy + 6.0, 2.0, col);

        // reload: an almost closed circle with an arrow head; a cross
        // (stop) while loading
        let r = reload_rect();
        let col = icon_button(c, r, self.pressed == Pressed::Reload, true);
        let (cx, cy) = (r.x as f32 + 17.0, r.y as f32 + 17.0);
        if self.loading() {
            stroke(c, cx - 6.0, cy - 6.0, cx + 6.0, cy + 6.0, 2.0, col);
            stroke(c, cx - 6.0, cy + 6.0, cx + 6.0, cy - 6.0, 2.0, col);
        } else {
            let steps = 20;
            let mut prev = None;
            for i in 0..=steps {
                let a = 0.9 + 5.0 * i as f32 / steps as f32;
                let p = (cx + 7.0 * cos(a), cy - 7.0 * sin(a));
                if let Some((px, py)) = prev {
                    stroke(c, px, py, p.0, p.1, 2.0, col);
                }
                prev = Some(p);
            }
            let (ax, ay) = (cx + 7.0 * cos(0.9), cy - 7.0 * sin(0.9));
            stroke(c, ax, ay, ax - 5.0, ay - 1.0, 2.0, col);
            stroke(c, ax, ay, ax + 1.0, ay - 5.5, 2.0, col);
        }

        // home: a house
        let r = home_rect();
        let col = icon_button(c, r, self.pressed == Pressed::Home, true);
        let (cx, cy) = (r.x as f32 + 17.0, r.y as f32 + 17.0);
        stroke(c, cx - 8.0, cy - 1.0, cx, cy - 8.0, 2.0, col);
        stroke(c, cx, cy - 8.0, cx + 8.0, cy - 1.0, 2.0, col);
        stroke(c, cx - 5.5, cy - 3.0, cx - 5.5, cy + 7.0, 2.0, col);
        stroke(c, cx + 5.5, cy - 3.0, cx + 5.5, cy + 7.0, 2.0, col);
        stroke(c, cx - 5.5, cy + 7.0, cx + 5.5, cy + 7.0, 2.0, col);

        // address bar
        let r = address_rect();
        let focused = self.focus == Focus::Address;
        c.fill_round(r, r.h / 2, theme::LIGHT);
        c.outline_round(
            r,
            r.h / 2,
            if focused {
                theme::ACCENT
            } else {
                theme::STROKE
            },
        );
        if focused {
            c.outline_round(r.inset(1), r.h / 2 - 1, theme::ACCENT);
        }
        let https = self.page.url.as_ref().is_some_and(|u| u.https) && !focused;
        if https {
            // a small padlock
            let (lx, ly) = (r.x + 14, r.y + 10);
            c.fill_round(Rect::new(lx, ly + 6, 11, 9), 2, rgb(0x2e, 0x7d, 0x32));
            c.outline_round(Rect::new(lx + 2, ly, 7, 12), 3, rgb(0x2e, 0x7d, 0x32));
            c.outline_round(Rect::new(lx + 3, ly + 1, 5, 10), 2, rgb(0x2e, 0x7d, 0x32));
        }
        draw_edit(
            c,
            self.address_text_rect(),
            &self.address,
            "Search or type a web address",
            self.cursor,
            focused,
            self.select_all,
        );

        let loading = self.loading();
        theme::accent_button(
            c,
            go_rect(),
            if loading { "..." } else { "Go" },
            self.pressed == Pressed::Go,
        );

        // a strip running along the bottom of the toolbar while loading
        if loading {
            let track = Rect::new(0, BAR_Y + TOOLBAR_H - 3, CLIENT_W, 3);
            let w = CLIENT_W / 4;
            let x = (spin as i32 * 24) % (CLIENT_W + w) - w;
            let mut s = c.sub(track);
            s.fill(Rect::new(x, 0, w, 3), theme::ACCENT);
        }
    }

    /// Draw the part `band` of the page area (in its own coordinates).
    fn draw_page(&self, c: &mut Canvas, band: Rect) {
        let area = content_rect(self.bare);
        let mut page = c.sub(area);
        page.clip_to(band);
        let (bg, _) = web_color(self.page.layout.canvas);
        page.fill(band, bg);
        let dy = -self.scroll;
        let view = Rect::new(0, 0, area.w, area.h).intersect(&band);
        let mut clips: Vec<Rect> = alloc::vec![view];
        for item in &self.page.layout.items {
            let clip = *clips.last().unwrap();
            let r = |r: &layout::Rect| Rect::new(r.x, r.y + dy, r.w, r.h);
            match item {
                Item::Clip(cr) => {
                    clips.push(clip.intersect(&r(cr)));
                    continue;
                }
                Item::Unclip => {
                    if clips.len() > 1 {
                        clips.pop();
                    }
                    continue;
                }
                Item::None => continue,
                _ => {}
            }
            if clip.is_empty() {
                continue;
            }
            // skip what is off screen
            let bounds = match item {
                Item::Fill { r: b, .. }
                | Item::Border { r: b, .. }
                | Item::Image { r: b, .. }
                | Item::Control { r: b, .. } => r(b),
                Item::Text {
                    x,
                    baseline,
                    w,
                    size,
                    ..
                } => Rect::new(
                    *x - 2,
                    baseline + dy - (*size as i32) * 2,
                    w + 4,
                    (*size as i32) * 3,
                ),
                _ => continue,
            };
            if bounds.intersect(&clip).is_empty() {
                continue;
            }
            let mut sub = page.sub(Rect::new(0, 0, area.w, area.h));
            sub.clip_to(clip);
            self.draw_item(&mut sub, item, dy);
        }
    }

    fn draw_item(&self, c: &mut Canvas, item: &Item, dy: i32) {
        match item {
            Item::Fill { r, color, radius } => {
                let rr = Rect::new(r.x, r.y + dy, r.w, r.h);
                let (col, a) = web_color(*color);
                if *radius > 0 || a < 256 {
                    c.fill_round_alpha(rr, *radius, col, a);
                } else {
                    c.fill(rr, col);
                }
            }
            Item::Border {
                r,
                widths,
                colors,
                radius,
            } => {
                let rr = Rect::new(r.x, r.y + dy, r.w, r.h);
                let uniform = widths.iter().all(|&w| w == widths[0])
                    && colors.iter().all(|&c| c == colors[0]);
                if *radius > 1 && uniform && widths[0] <= 3 {
                    let (col, _) = web_color(colors[0]);
                    for i in 0..widths[0] {
                        c.outline_round(rr.inset(i), (*radius - i).max(0), col);
                    }
                    return;
                }
                let sides = [
                    Rect::new(rr.x, rr.y, rr.w, widths[0]),
                    Rect::new(rr.right() - widths[1], rr.y, widths[1], rr.h),
                    Rect::new(rr.x, rr.bottom() - widths[2], rr.w, widths[2]),
                    Rect::new(rr.x, rr.y, widths[3], rr.h),
                ];
                for (i, s) in sides.iter().enumerate() {
                    if widths[i] > 0 && colors[i] >> 24 != 0 {
                        let (col, a) = web_color(colors[i]);
                        c.fill_round_alpha(*s, 0, col, a);
                    }
                }
            }
            Item::Text {
                x,
                baseline,
                w,
                text,
                face,
                size,
                color,
                underline,
                strike,
            } => {
                let (col, _) = web_color(*color);
                let f = webfont::Face {
                    bold: face.bold,
                    italic: face.italic,
                    mono: face.mono,
                };
                let by = baseline + dy;
                webfont::draw(c, f, *size, *x, by, text, col);
                let thick = (*size as i32 / 14).max(1);
                if *underline {
                    c.fill_rect(*x, by + (*size as i32) / 8, *w, thick, col);
                }
                if *strike {
                    c.fill_rect(*x, by - (*size as i32) * 3 / 10, *w, thick, col);
                }
            }
            Item::Image { r, src, cover } => {
                if let Some(img) = self.page.images.get(src) {
                    draw_image(c, img, Rect::new(r.x, r.y + dy, r.w, r.h), *cover);
                }
            }
            Item::Control { r, node, kind } => {
                let rr = Rect::new(r.x, r.y + dy, r.w, r.h);
                self.draw_control(c, rr, *node, *kind);
            }
            _ => {}
        }
    }

    fn draw_control(&self, c: &mut Canvas, r: Rect, node: NodeId, kind: Control) {
        let dom = &self.page.dom;
        match kind {
            Control::Checkbox | Control::Radio => {
                let checked = dom.attr(node, "checked").is_some();
                let bx = Rect::new(r.x, r.y, r.w.max(13), r.h.max(13));
                let radius = if kind == Control::Radio { bx.w / 2 } else { 3 };
                if checked {
                    c.fill_round(bx, radius, theme::ACCENT);
                    if kind == Control::Radio {
                        c.fill_round(bx.inset(4), (bx.w - 8) / 2, rgb(255, 255, 255));
                    } else {
                        let (x0, y0) = (bx.x as f32, bx.y as f32);
                        stroke(
                            c,
                            x0 + 3.0,
                            y0 + 7.0,
                            x0 + 5.5,
                            y0 + 10.0,
                            2.0,
                            rgb(255, 255, 255),
                        );
                        stroke(
                            c,
                            x0 + 5.5,
                            y0 + 10.0,
                            x0 + 10.5,
                            y0 + 3.5,
                            2.0,
                            rgb(255, 255, 255),
                        );
                    }
                } else {
                    c.fill_round(bx, radius, rgb(255, 255, 255));
                    c.outline_round(bx, radius, rgb(0x76, 0x76, 0x76));
                }
            }
            Control::Select => {
                let text = self.page.field_value(node);
                let label = dom
                    .descendants(node)
                    .into_iter()
                    .filter(|&o| dom.tag(o) == "option")
                    .find(|&o| {
                        dom.attr(o, "selected").is_some()
                            || dom.attr(o, "value").unwrap_or("") == text
                    })
                    .map(|o| web::text::collapse(&dom.text_content(o)))
                    .unwrap_or(text);
                let ty = r.y + (r.h - UI.line_height) / 2;
                let mut inner = c.sub(Rect::new(r.x, r.y, (r.w - 18).max(0), r.h));
                inner.draw_text(0, ty - r.y, &label, theme::TEXT);
                let (ax, ay) = ((r.right() - 10) as f32, (r.y + r.h / 2) as f32);
                stroke(c, ax - 4.0, ay - 2.0, ax, ay + 2.0, 1.5, theme::TEXT);
                stroke(c, ax, ay + 2.0, ax + 4.0, ay - 2.0, 1.5, theme::TEXT);
            }
            Control::Text | Control::Password | Control::TextArea => {
                let focused = self.focus == Focus::Field(node);
                let value = if focused {
                    self.field_text.clone()
                } else {
                    self.page.field_value(node)
                };
                let shown = if kind == Control::Password {
                    value.chars().map(|_| '•').collect()
                } else if kind == Control::TextArea {
                    value.replace('\n', " ")
                } else {
                    value
                };
                let placeholder = dom.attr(node, "placeholder").unwrap_or("");
                let line = if kind == Control::TextArea {
                    Rect::new(r.x, r.y, r.w, UI.line_height + 4)
                } else {
                    r
                };
                draw_edit(c, line, &shown, placeholder, self.cursor, focused, false);
            }
        }
    }

    fn draw_scrollbar(&self, c: &mut Canvas) {
        let track = scrollbar_rect(self.bare);
        c.fill(track, rgb(0xf6, 0xf6, 0xf6));
        c.fill_rect(track.x, track.y, 1, track.h, rgb(0xe6, 0xe6, 0xe6));
        if self.max_scroll() > 0 {
            let (top, h) = self.thumb();
            let active = matches!(self.pressed, Pressed::Thumb(_));
            let color = if active {
                rgb(0x80, 0x80, 0x84)
            } else {
                rgb(0xb8, 0xb8, 0xbc)
            };
            c.fill_round(
                Rect::new(track.x + 3, top + 2, track.w - 5, h - 4),
                3,
                color,
            );
        }
    }

    fn draw_status(&self, c: &mut Canvas) {
        let bar = Rect::new(0, CLIENT_H - STATUS_H, CLIENT_W, STATUS_H);
        c.fill(bar, theme::FACE);
        c.fill_rect(0, bar.y, CLIENT_W, 1, theme::STROKE);
        let ty = bar.y + (STATUS_H - UI.line_height) / 2;
        let loading = self.loading();
        let x = 10;
        let status = match (&self.hover, loading) {
            (Some(link), false) => link.as_str(),
            _ => self.status.as_str(),
        };
        let mut images = String::new();
        let waiting = self.image_queue.borrow().len() + self.image_fibers.len();
        if waiting > 0 && !loading {
            images = alloc::format!("Loading images: {} left", waiting);
        }
        let right_w = UI.width(&images);
        c.draw_text(CLIENT_W - right_w - 14, ty, &images, theme::TEXT_DIM);
        let mut left = c.sub(Rect::new(x, bar.y, CLIENT_W - x - right_w - 40, STATUS_H));
        left.draw_text(0, ty - bar.y, status, theme::TEXT_DIM);
    }
}

/// Draw an image scaled into `r`; with `cover`, scaled to cover it and cut.
fn draw_image(c: &mut Canvas, img: &web::image::Image, r: Rect, cover: bool) {
    if img.width == 0 || r.w <= 0 || r.h <= 0 {
        return;
    }
    let (iw, ih) = (img.width as i64, img.height as i64);
    // source pixels per destination pixel, in 1/1024
    let (sx, sy, ox, oy) = if cover {
        let scale = ((iw * 1024) / r.w as i64)
            .min((ih * 1024) / r.h as i64)
            .max(1);
        let ox = (iw * 1024 - scale * r.w as i64) / 2;
        let oy = (ih * 1024 - scale * r.h as i64) / 2;
        (scale, scale, ox.max(0), oy.max(0))
    } else {
        ((iw * 1024) / r.w as i64, (ih * 1024) / r.h as i64, 0, 0)
    };
    let step = ((sx.max(sy) + 512) / 1024).max(1) as usize;
    let visible = r.intersect(&c.clip_rect());
    for py in visible.y..visible.bottom() {
        let src_y = ((oy + (py - r.y) as i64 * sy) / 1024).clamp(0, ih - 1) as usize;
        for px in visible.x..visible.right() {
            let src_x = ((ox + (px - r.x) as i64 * sx) / 1024).clamp(0, iw - 1) as usize;
            let p = img.sample(src_x, src_y, step);
            let a = (p >> 24) as i32;
            if a == 255 {
                c.pixel(px, py, p & 0xff_ffff);
            } else if a > 0 {
                c.blend_at(px, py, p & 0xff_ffff, a);
            }
        }
    }
}

/// Cumulative advance before each character of `text`, in 1/16 pixels.
fn advances(text: &str) -> Vec<i32> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut pen = 0;
    out.push(0);
    for ch in text.chars() {
        pen += UI.advance16(ch) as i32;
        out.push(pen);
    }
    out
}

/// The first character a text box shows, so that the cursor is visible.
fn first_visible(adv: &[i32], cursor: usize, width: i32) -> usize {
    let mut start = 0;
    while start < cursor && adv[cursor] - adv[start] > (width - 4) * 16 {
        start += 1;
    }
    start
}

/// A one-line text box: `r` is the area for the text.
fn draw_edit(
    c: &mut Canvas,
    r: Rect,
    text: &str,
    placeholder: &str,
    cursor: usize,
    focused: bool,
    select_all: bool,
) {
    let mut inner = c.sub(r);
    let ty = (r.h - UI.line_height) / 2;
    if text.is_empty() {
        if !focused {
            inner.draw_text(0, ty, placeholder, rgb(0x80, 0x86, 0x8c));
        } else {
            inner.fill_rect(0, ty, 1, UI.line_height, theme::TEXT);
        }
        return;
    }
    let adv = advances(text);
    let cursor = cursor.min(adv.len() - 1);
    let start = if focused {
        first_visible(&adv, cursor, r.w)
    } else {
        0
    };
    let shown: String = text.chars().skip(start).collect();
    if focused && select_all {
        let w = ((adv[adv.len() - 1] - adv[start]) / 16).min(r.w);
        inner.fill_rect(
            0,
            ty,
            w,
            UI.line_height,
            mix(theme::ACCENT, theme::LIGHT, 60),
        );
    }
    inner.draw_text(0, ty, &shown, theme::TEXT);
    if focused && !select_all {
        let x = (adv[cursor] - adv[start] + 8) / 16;
        inner.fill_rect(x, ty, 1, UI.line_height, theme::TEXT);
    }
}

/// Where a click at `x` puts the cursor in a text box.
fn click_cursor(r: Rect, text: &str, cursor: usize, x: i32) -> usize {
    let adv = advances(text);
    let cursor = cursor.min(adv.len() - 1);
    let start = first_visible(&adv, cursor, r.w);
    let target = (x - r.x) * 16 + adv[start];
    (start..adv.len())
        .min_by_key(|&i| (adv[i] - target).abs())
        .unwrap_or(0)
}

fn sin(x: f32) -> f32 {
    // reduce to [-pi, pi], then a Taylor series
    let pi = core::f32::consts::PI;
    let mut x = x % (2.0 * pi);
    if x > pi {
        x -= 2.0 * pi;
    } else if x < -pi {
        x += 2.0 * pi;
    }
    let x2 = x * x;
    x * (1.0
        - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0 * (1.0 - x2 / 72.0 * (1.0 - x2 / 110.0)))))
}

fn cos(x: f32) -> f32 {
    sin(x + core::f32::consts::FRAC_PI_2)
}
