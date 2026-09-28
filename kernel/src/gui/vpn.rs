//! VPN: the window of the VPN client, in the style of Happ. Paste a
//! subscription address or a vless://, trojan:// or ss:// link, pick a
//! server (the list shows each one's delay), and press the big button.
//! While it is on, every program's connections go through the server.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::text::{HEADING, UI, UI_BOLD};
use super::widgets::{self, FieldEvent, TextField};
use super::{theme, MouseEvent, MouseKind};
use crate::fiber::Fiber;
use crate::interrupts;
use crate::keyboard::Key;
use crate::vpn::{self, Group, Saved, Server};

pub const CLIENT_W: i32 = 880;
pub const CLIENT_H: i32 = 640;

const HEAD_H: i32 = 150;
const INPUT_Y: i32 = HEAD_H + 16;
const TOOLS_Y: i32 = HEAD_H + 64;
const LIST_Y: i32 = HEAD_H + 108;
const STATUS_H: i32 = 30;
const GROUP_H: i32 = 54;
const ROW_H: i32 = 44;

/// Something in the window that can be clicked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Power,
    Field,
    Paste,
    Add,
    UpdateAll,
    TestAll,
    Server(usize, usize),
    Update(usize),
    Delete(usize),
}

/// A line of the list: a subscription's heading or one of its servers.
#[derive(Clone, Copy)]
enum Line {
    Group(usize),
    Server(usize, usize),
}

type Shared<T> = Rc<RefCell<T>>;

enum Work {
    /// Connecting to the server with this link.
    Connect(String, Shared<Option<Result<u32, String>>>),
    /// Delay tests, as they finish.
    Test(Shared<Vec<(String, Result<u32, String>)>>),
    /// Subscriptions downloaded.
    Fetch(Shared<Vec<Result<Group, String>>>),
}

struct Job {
    fiber: Fiber,
    work: Work,
}

pub struct Vpn {
    saved: Saved,
    user: String,
    field: TextField,
    field_focused: bool,
    /// Delay test results by link.
    pings: BTreeMap<String, Result<u32, String>>,
    jobs: Vec<Job>,
    draining: Vec<Fiber>,
    status: String,
    scroll: i32,
    thumb_grab: Option<i32>,
    hover: Option<Hit>,
    /// The second the header's clock and traffic were drawn for.
    shown_second: u64,
    was_on: bool,
}

fn client() -> Rect {
    Rect::new(0, 0, CLIENT_W, CLIENT_H)
}

fn power_rect() -> Rect {
    Rect::new(36, (HEAD_H - 92) / 2, 92, 92)
}

fn add_rect() -> Rect {
    Rect::new(CLIENT_W - 20 - 90, INPUT_Y, 90, 36)
}

fn paste_rect() -> Rect {
    Rect::new(add_rect().x - 8 - 170, INPUT_Y, 170, 36)
}

fn field_rect() -> Rect {
    Rect::new(20, INPUT_Y, paste_rect().x - 8 - 20, 36)
}

fn update_all_rect() -> Rect {
    Rect::new(20, TOOLS_Y, 196, 32)
}

fn test_all_rect() -> Rect {
    Rect::new(update_all_rect().right() + 8, TOOLS_Y, 150, 32)
}

fn list_rect() -> Rect {
    Rect::new(0, LIST_Y, CLIENT_W, CLIENT_H - LIST_Y - STATUS_H)
}

fn track() -> Rect {
    let l = list_rect();
    Rect::new(CLIENT_W - 16, l.y + 6, 10, l.h - 12)
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

/// A Unix time as dd.mm.yyyy.
fn date_text(t: i64) -> String {
    let (y, m, d) = crate::rtc::civil_from_days(t.div_euclid(86400));
    format!("{:02}.{:02}.{}", d, m, y)
}

fn clock_text(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

impl Vpn {
    pub fn new() -> Self {
        Self {
            saved: Saved {
                groups: Vec::new(),
                selected: String::new(),
            },
            user: String::new(),
            field: TextField::new(""),
            field_focused: true,
            pings: BTreeMap::new(),
            jobs: Vec::new(),
            draining: Vec::new(),
            status: String::new(),
            scroll: 0,
            thumb_grab: None,
            hover: None,
            shown_second: 0,
            was_on: false,
        }
    }

    /// The window opened: read the saved servers (again, if someone
    /// else signed in meanwhile).
    pub fn start(&mut self) {
        crate::net::init();
        let user = crate::users::current_name()
            .map(|n| String::from(n.as_str()))
            .unwrap_or_default();
        if user != self.user {
            self.user = user;
            self.saved = vpn::load();
            self.pings.clear();
            self.scroll = 0;
        }
    }

    fn save(&mut self) {
        if let Err(e) = vpn::save(&self.saved) {
            self.status = format!("Could not save the servers: {}", e);
        }
    }

    fn servers(&self) -> impl Iterator<Item = &Server> {
        self.saved.groups.iter().flat_map(|g| g.servers.iter())
    }

    /// The chosen server, or the first one that can be used.
    fn selected(&self) -> Option<Server> {
        self.servers()
            .find(|s| s.link == self.saved.selected)
            .or_else(|| self.servers().find(|s| s.unsupported().is_none()))
            .cloned()
    }

    fn connecting(&self) -> Option<&str> {
        self.jobs.iter().find_map(|j| match &j.work {
            Work::Connect(link, _) => Some(link.as_str()),
            _ => None,
        })
    }

    fn fetching(&self) -> bool {
        self.jobs.iter().any(|j| matches!(j.work, Work::Fetch(_)))
    }

    fn testing(&self) -> bool {
        self.jobs.iter().any(|j| matches!(j.work, Work::Test(_)))
    }

    /// Whether work is running, so the desktop keeps ticking.
    pub fn busy(&self) -> bool {
        !self.jobs.is_empty() || !self.draining.is_empty() || vpn::is_on() != self.was_on
    }

    // ---- actions --------------------------------------------------------------

    fn connect(&mut self, server: Server) {
        if let Some(why) = server.unsupported() {
            self.status = format!("{}: {}.", server.name, why);
            return;
        }
        self.cancel_connect();
        self.saved.selected = server.link.clone();
        self.save();
        self.status = format!("Connecting to {}...", server.name);
        let out: Shared<Option<Result<u32, String>>> = Rc::new(RefCell::new(None));
        let o = out.clone();
        let link = server.link.clone();
        let fiber = Fiber::new(move || {
            *o.borrow_mut() = Some(vpn::connect(&server));
        });
        self.jobs.push(Job {
            fiber,
            work: Work::Connect(link, out),
        });
    }

    fn cancel_connect(&mut self) {
        let mut i = 0;
        while i < self.jobs.len() {
            if matches!(self.jobs[i].work, Work::Connect(..)) {
                let mut job = self.jobs.remove(i);
                job.fiber.cancel();
                self.draining.push(job.fiber);
            } else {
                i += 1;
            }
        }
    }

    fn power(&mut self) {
        if self.connecting().is_some() {
            self.cancel_connect();
            vpn::disconnect();
            self.status = String::from("Stopped.");
        } else if vpn::is_on() {
            vpn::disconnect();
            self.status = String::from("The VPN is off: programs connect directly.");
        } else {
            match self.selected() {
                Some(s) => self.connect(s),
                None => {
                    self.status = String::from(
                        "Add a subscription or a server link first: paste it in the box above.",
                    )
                }
            }
        }
    }

    fn test_all(&mut self) {
        if self.testing() {
            return;
        }
        let servers: Vec<Server> = self.servers().filter(|s| s.unsupported().is_none()).cloned().collect();
        if servers.is_empty() {
            self.status = String::from("No servers to test.");
            return;
        }
        for s in &servers {
            self.pings.remove(&s.link);
        }
        self.status = format!("Testing {} servers...", servers.len());
        let out: Shared<Vec<(String, Result<u32, String>)>> = Rc::new(RefCell::new(Vec::new()));
        let o = out.clone();
        let fiber = Fiber::new(move || {
            for s in servers {
                if crate::fiber::cancelled() {
                    break;
                }
                let r = vpn::test(&s);
                o.borrow_mut().push((s.link.clone(), r));
            }
        });
        self.jobs.push(Job {
            fiber,
            work: Work::Test(out),
        });
    }

    fn fetch(&mut self, urls: Vec<String>) {
        if self.fetching() || urls.is_empty() {
            return;
        }
        self.status = if urls.len() == 1 {
            String::from("Downloading the subscription...")
        } else {
            format!("Updating {} subscriptions...", urls.len())
        };
        let out: Shared<Vec<Result<Group, String>>> = Rc::new(RefCell::new(Vec::new()));
        let o = out.clone();
        let fiber = Fiber::new(move || {
            for u in urls {
                let r = vpn::fetch_subscription(&u);
                o.borrow_mut().push(r);
            }
        });
        self.jobs.push(Job {
            fiber,
            work: Work::Fetch(out),
        });
    }

    fn update_all(&mut self) {
        let urls: Vec<String> = self
            .saved
            .groups
            .iter()
            .filter(|g| !g.url.is_empty())
            .map(|g| g.url.clone())
            .collect();
        if urls.is_empty() {
            self.status = String::from("There are no subscriptions to update.");
        }
        self.fetch(urls);
    }

    /// Add what was typed or pasted: a subscription address, or links.
    fn add(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.status = String::from(
                "Paste a subscription address (https://...) or a vless://, trojan:// or ss:// link.",
            );
            return;
        }
        let first = text.lines().next().unwrap_or("").trim();
        if first.starts_with("http://") || first.starts_with("https://") || first.starts_with("happ://add/") {
            self.fetch(alloc::vec![String::from(first)]);
            self.field.set("");
            return;
        }
        let servers = vpn::link::parse_list(text);
        if servers.is_empty() {
            self.status = String::from("That is not a link RyzikOS knows: use vless://, trojan://, ss:// or a subscription address.");
            return;
        }
        let n = servers.len();
        vpn::add_group(
            &mut self.saved,
            Group {
                url: String::new(),
                title: String::new(),
                usage: None,
                servers,
            },
        );
        if self.saved.selected.is_empty() {
            if let Some(s) = self.selected() {
                self.saved.selected = s.link;
            }
        }
        self.save();
        self.field.set("");
        self.status = format!("Added {} server{}.", n, if n == 1 { "" } else { "s" });
    }

    fn delete_group(&mut self, g: usize) {
        if g >= self.saved.groups.len() {
            return;
        }
        let group = self.saved.groups.remove(g);
        let on_it = vpn::route().is_some_and(|r| group.servers.iter().any(|s| s.link == r.link));
        if on_it {
            vpn::disconnect();
        }
        self.save();
        self.status = format!(
            "Removed {}.",
            if group.url.is_empty() { "the servers added by hand" } else { group.title.as_str() }
        );
        self.scroll_to(self.scroll);
    }

    /// Run the work a little. True when the window must be drawn again.
    pub fn tick(&mut self) -> bool {
        self.draining.retain_mut(|f| !f.resume());
        let mut changed = false;
        let mut i = 0;
        while i < self.jobs.len() {
            let done = self.jobs[i].fiber.resume();
            // delay results show as they come
            if let Work::Test(out) = &self.jobs[i].work {
                for (link, r) in out.borrow_mut().drain(..) {
                    self.pings.insert(link, r);
                    changed = true;
                }
            }
            if !done {
                i += 1;
                continue;
            }
            let job = self.jobs.remove(i);
            changed = true;
            match job.work {
                Work::Connect(link, out) => {
                    self.status = match out.borrow_mut().take() {
                        Some(Ok(ms)) => {
                            let name = vpn::route().map(|s| s.name).unwrap_or_default();
                            if let Some(s) = vpn::route() {
                                self.pings.insert(s.link, Ok(ms));
                            }
                            format!("Connected to {} ({} ms). All programs now go through it.", name, ms)
                        }
                        Some(Err(e)) => {
                            self.pings.insert(link, Err(e.clone()));
                            format!("Could not connect: {}", e)
                        }
                        None => String::from("Stopped."),
                    };
                }
                Work::Test(_) => {
                    let ok = self.pings.values().filter(|r| r.is_ok()).count();
                    self.status = format!("Tested: {} of {} servers answer.", ok, self.pings.len());
                }
                Work::Fetch(out) => {
                    let results: Vec<_> = out.borrow_mut().drain(..).collect();
                    let mut added = 0;
                    let mut errors = Vec::new();
                    for r in results {
                        match r {
                            Ok(g) => {
                                added += g.servers.len();
                                vpn::add_group(&mut self.saved, g);
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                    if self.saved.selected.is_empty() {
                        if let Some(s) = self.selected() {
                            self.saved.selected = s.link;
                        }
                    }
                    self.save();
                    self.status = match errors.first() {
                        Some(e) => format!("Could not update: {}", e),
                        None => format!("The subscription has {} servers. Press the button to connect.", added),
                    };
                    if errors.is_empty() {
                        self.test_all();
                    }
                }
            }
        }
        // the header shows the time connected and the traffic
        let on = vpn::is_on();
        if on != self.was_on {
            self.was_on = on;
            changed = true;
        }
        let second = interrupts::ticks() / interrupts::TIMER_HZ;
        if on && second != self.shown_second {
            self.shown_second = second;
            changed = true;
        }
        changed
    }

    // ---- the list ----------------------------------------------------------------

    fn lines(&self) -> Vec<Line> {
        let mut out = Vec::new();
        for (g, group) in self.saved.groups.iter().enumerate() {
            out.push(Line::Group(g));
            for s in 0..group.servers.len() {
                out.push(Line::Server(g, s));
            }
        }
        out
    }

    fn line_height(line: Line) -> i32 {
        match line {
            Line::Group(_) => GROUP_H,
            Line::Server(..) => ROW_H,
        }
    }

    fn total(&self) -> i32 {
        self.lines().into_iter().map(Self::line_height).sum::<i32>() + 12
    }

    fn max_scroll(&self) -> i32 {
        (self.total() - list_rect().h).max(0)
    }

    fn scroll_to(&mut self, to: i32) -> bool {
        let old = self.scroll;
        self.scroll = to.clamp(0, self.max_scroll());
        old != self.scroll
    }

    /// Each line with where it is on screen.
    fn placed(&self) -> Vec<(Line, Rect)> {
        let list = list_rect();
        let mut y = list.y + 4 - self.scroll;
        let mut out = Vec::new();
        for line in self.lines() {
            let h = Self::line_height(line);
            out.push((line, Rect::new(16, y, CLIENT_W - 40, h)));
            y += h;
        }
        out
    }

    fn group_buttons(&self, g: usize, r: Rect) -> (Option<Rect>, Rect) {
        let delete = Rect::new(r.right() - 90, r.y + 14, 90, 30);
        let update = (!self.saved.groups[g].url.is_empty()).then(|| Rect::new(delete.x - 8 - 100, r.y + 14, 100, 30));
        (update, delete)
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if power_rect().contains(x, y) {
            return Some(Hit::Power);
        }
        let buttons = [
            (Hit::Field, field_rect()),
            (Hit::Paste, paste_rect()),
            (Hit::Add, add_rect()),
            (Hit::UpdateAll, update_all_rect()),
            (Hit::TestAll, test_all_rect()),
        ];
        if let Some((h, _)) = buttons.iter().find(|(_, r)| r.contains(x, y)) {
            return Some(*h);
        }
        if !list_rect().contains(x, y) || x >= track().x - 4 {
            return None;
        }
        for (line, r) in self.placed() {
            if !r.contains(x, y) {
                continue;
            }
            return match line {
                Line::Group(g) => {
                    let (update, delete) = self.group_buttons(g, r);
                    if delete.contains(x, y) {
                        Some(Hit::Delete(g))
                    } else if update.is_some_and(|u| u.contains(x, y)) {
                        Some(Hit::Update(g))
                    } else {
                        None
                    }
                }
                Line::Server(g, s) => Some(Hit::Server(g, s)),
            };
        }
        None
    }

    // ---- input ---------------------------------------------------------------------

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
            let to = if ev.y < t.y { self.scroll - view } else { self.scroll + view };
            return self.scroll_to(to);
        }
        let hit = self.hit(ev.x, ev.y);
        self.field_focused = hit == Some(Hit::Field);
        match hit {
            Some(Hit::Power) => self.power(),
            Some(Hit::Field) => self.field.click(field_rect(), ev.x),
            Some(Hit::Paste) => {
                let text = widgets::paste();
                self.add(&text);
            }
            Some(Hit::Add) => {
                let text = self.field.string();
                self.add(&text);
            }
            Some(Hit::UpdateAll) => self.update_all(),
            Some(Hit::TestAll) => self.test_all(),
            Some(Hit::Update(g)) => {
                let url = self.saved.groups[g].url.clone();
                self.fetch(alloc::vec![url]);
            }
            Some(Hit::Delete(g)) => self.delete_group(g),
            Some(Hit::Server(g, s)) => {
                let server = self.saved.groups[g].servers[s].clone();
                let was = self.saved.selected.clone();
                self.saved.selected = server.link.clone();
                // a click on the chosen server (or any, while on) connects
                if vpn::is_on() || self.connecting().is_some() || was == server.link {
                    if vpn::route().map(|r| r.link) != Some(server.link.clone()) {
                        self.connect(server);
                    }
                } else {
                    self.save();
                    self.status = format!("{} is chosen. Press the button to connect.", server.name);
                }
            }
            None => {}
        }
        true
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
            Key::Function(5) => {
                self.update_all();
                return true;
            }
            Key::Ctrl('t') => {
                self.test_all();
                return true;
            }
            _ => {}
        }
        if self.field_focused {
            return match self.field.on_key(key) {
                FieldEvent::Enter => {
                    let text = self.field.string();
                    self.add(&text);
                    true
                }
                FieldEvent::Escape => {
                    self.field_focused = false;
                    true
                }
                FieldEvent::Changed => true,
                FieldEvent::None => self.list_key(key),
            };
        }
        match key {
            Key::Ctrl('v') => {
                let text = widgets::paste();
                self.add(&text);
                true
            }
            Key::Enter | Key::Char(' ') => {
                self.power();
                true
            }
            Key::Up | Key::Down => {
                // choose the server above or below
                let all: Vec<String> = self.servers().map(|s| s.link.clone()).collect();
                if all.is_empty() {
                    return false;
                }
                let at = all.iter().position(|l| *l == self.saved.selected);
                let next = match (at, key) {
                    (None, _) => 0,
                    (Some(i), Key::Up) => i.saturating_sub(1),
                    (Some(i), _) => (i + 1).min(all.len() - 1),
                };
                self.saved.selected = all[next].clone();
                self.save();
                true
            }
            Key::Char('\t') => {
                self.field_focused = true;
                true
            }
            _ => self.list_key(key),
        }
    }

    fn list_key(&mut self, key: Key) -> bool {
        match key {
            Key::Down => self.on_wheel(1),
            Key::PageDown => self.on_wheel(5),
            Key::Up => self.on_wheel(-1),
            Key::PageUp => self.on_wheel(-5),
            _ => false,
        }
    }

    // ---- drawing ------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, caret: bool) {
        c.fill(client(), theme::light());
        self.draw_head(c);
        let field = field_rect();
        self.field.draw(c, field, self.field_focused, caret);
        if self.field.text.is_empty() {
            c.draw_text(
                field.x + 10,
                field.y + (field.h - UI.line_height) / 2,
                &fit("Subscription address or vless://, trojan://, ss:// link", field.w - 20),
                theme::text_dim(),
            );
        }
        let hot = |h: Hit| self.hover == Some(h);
        theme::button(c, paste_rect(), "Paste from clipboard", hot(Hit::Paste));
        theme::accent_button(c, add_rect(), "Add", hot(Hit::Add));
        let fetching = self.fetching();
        theme::button(c, update_all_rect(), if fetching { "Updating..." } else { "Update subscriptions" }, hot(Hit::UpdateAll));
        theme::button(c, test_all_rect(), if self.testing() { "Testing..." } else { "Test delays" }, hot(Hit::TestAll));
        let n = self.servers().count();
        let count = format!("{} server{}", n, if n == 1 { "" } else { "s" });
        c.draw_text(CLIENT_W - 24 - UI.width(&count), TOOLS_Y + 6, &count, theme::text_dim());
        self.draw_list(c);
        // the status line
        let st = Rect::new(0, CLIENT_H - STATUS_H, CLIENT_W, STATUS_H);
        c.fill(st, theme::face());
        c.fill_rect(0, st.y, CLIENT_W, 1, theme::stroke());
        c.draw_text(14, st.y + 6, &fit(&self.status, CLIENT_W - 28), theme::text());
    }

    fn draw_head(&self, c: &mut Canvas) {
        let head = Rect::new(0, 0, CLIENT_W, HEAD_H);
        let route = vpn::route();
        let connecting = self.connecting().is_some();
        let (top, bottom) = if connecting {
            (rgb(0xf0, 0xa8, 0x30), rgb(0xd0, 0x70, 0x10))
        } else if route.is_some() {
            (rgb(0x2c, 0xc4, 0x8c), rgb(0x0e, 0x8a, 0x6a))
        } else {
            (rgb(0x4a, 0x55, 0x6c), rgb(0x28, 0x30, 0x42))
        };
        c.vertical_gradient(head, top, bottom);
        // the power button: a white disc with the power sign
        let p = power_rect();
        let lit = self.hover == Some(Hit::Power);
        c.fill_round(p.inset(-6), p.w / 2 + 6, mix(top, 0xffffff, if lit { 90 } else { 60 }));
        c.fill_round(p, p.w / 2, 0xffffff);
        let ink = if route.is_some() || connecting { bottom } else { rgb(0x4a, 0x55, 0x6c) };
        let (cx, cy) = (p.x + p.w / 2, p.y + p.h / 2);
        let ring = Rect::new(cx - 24, cy - 24, 48, 48);
        for k in 0..5 {
            c.outline_round(ring.inset(k), 24 - k, ink);
        }
        // the gap at the top of the ring, and the bar through it
        c.fill_rect(cx - 9, cy - 26, 18, 20, 0xffffff);
        c.fill_round(Rect::new(cx - 3, cy - 30, 6, 30), 3, ink);

        let tx = p.right() + 36;
        let (title, sub) = match (&route, self.connecting()) {
            (_, Some(link)) => {
                let name = self.servers().find(|s| s.link == link).map(|s| s.name.clone()).unwrap_or_default();
                (String::from("Connecting..."), name)
            }
            (Some(s), None) => (String::from("Connected"), format!("{}  ·  {}", s.name, s.protocol())),
            (None, None) => (
                String::from("Not connected"),
                match self.selected() {
                    Some(s) => format!("{}  ·  {}", s.name, s.protocol()),
                    None => String::from("No servers yet"),
                },
            ),
        };
        c.draw_text_in(&HEADING, tx, 30, &title, 0xffffff);
        c.draw_text_in(&UI_BOLD, tx, 72, &fit(&sub, CLIENT_W - tx - 24), rgb(0xf0, 0xf4, 0xff));
        let line = if route.is_some() && !connecting {
            let (up, down) = vpn::traffic();
            format!(
                "{}   ·   sent {}   ·   received {}",
                clock_text(vpn::uptime()),
                vpn::size_text(up),
                vpn::size_text(down)
            )
        } else {
            String::from("Press the button to send all programs through the VPN server")
        };
        c.draw_text(tx, 102, &fit(&line, CLIENT_W - tx - 24), rgb(0xe0, 0xe8, 0xf4));
    }

    fn draw_list(&self, c: &mut Canvas) {
        let list = list_rect();
        c.fill_rect(0, list.y - 1, CLIENT_W, 1, theme::stroke());
        let mut s = c.sub(Rect::new(0, 0, c.width, c.height));
        s.clip_to(list);
        if self.saved.groups.is_empty() {
            let lines = [
                "No servers yet.",
                "Paste the subscription address your VPN service gave you (it starts with https://)",
                "or a vless://, trojan:// or ss:// link above, and press Add.",
            ];
            for (i, l) in lines.iter().enumerate() {
                let f = if i == 0 { &UI_BOLD } else { &UI };
                let w = f.width(l);
                s.draw_text_in(f, (CLIENT_W - w) / 2, list.y + 60 + i as i32 * 28, l, theme::text_dim());
            }
        }
        let route_link = vpn::route().map(|r| r.link);
        let connecting = self.connecting().map(String::from);
        for (line, r) in self.placed() {
            if r.bottom() < list.y || r.y > list.bottom() {
                continue;
            }
            match line {
                Line::Group(g) => self.draw_group(&mut s, g, r),
                Line::Server(g, i) => {
                    let server = &self.saved.groups[g].servers[i];
                    let on = route_link.as_deref() == Some(server.link.as_str());
                    let busy = connecting.as_deref() == Some(server.link.as_str());
                    self.draw_server(&mut s, g, i, server, r, on, busy);
                }
            }
        }
        if self.max_scroll() > 0 {
            let t = track();
            c.fill_round(t, 5, mix(theme::stroke(), theme::light(), 140));
            let thumb = widgets::thumb(t, true, self.total(), list.h, self.scroll).inset(2);
            let color = if self.thumb_grab.is_some() { theme::accent() } else { theme::thumb() };
            c.fill_round(thumb, 3, color);
        }
    }

    fn draw_group(&self, c: &mut Canvas, g: usize, r: Rect) {
        let group = &self.saved.groups[g];
        let title = if group.url.is_empty() {
            String::from("Added by hand")
        } else if group.title.is_empty() {
            group.url.clone()
        } else {
            group.title.clone()
        };
        c.draw_text_in(&UI_BOLD, r.x + 4, r.y + 12, &fit(&title, r.w - 260), theme::text());
        let info = match group.usage {
            Some(u) => {
                let mut t = if u.total > 0 {
                    format!("Used {} of {}", vpn::size_text(u.used), vpn::size_text(u.total))
                } else {
                    format!("Used {}", vpn::size_text(u.used))
                };
                if u.expire > 0 {
                    t.push_str(&format!("   ·   until {}", date_text(u.expire)));
                }
                t
            }
            None if group.url.is_empty() => format!("{} servers", group.servers.len()),
            None => fit(&group.url, r.w - 260),
        };
        c.draw_text(r.x + 4, r.y + 32, &info, theme::text_dim());
        let (update, delete) = self.group_buttons(g, r);
        if let Some(u) = update {
            theme::button(c, u, "Update", self.hover == Some(Hit::Update(g)));
        }
        let face = if self.hover == Some(Hit::Delete(g)) { theme::hover() } else { theme::light() };
        c.fill_round(delete, 15, face);
        c.outline_round(delete, 15, theme::stroke());
        c.text_centered(delete, "Delete", theme::error());
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_server(&self, c: &mut Canvas, g: usize, i: usize, server: &Server, r: Rect, on: bool, busy: bool) {
        let row = Rect::new(r.x, r.y + 2, r.w, r.h - 4);
        let chosen = server.link == self.saved.selected;
        let unusable = server.unsupported();
        let face = if on {
            mix(rgb(0x2c, 0xc4, 0x8c), theme::light(), 200)
        } else if chosen {
            mix(theme::accent(), theme::light(), 215)
        } else if self.hover == Some(Hit::Server(g, i)) {
            theme::row_hover()
        } else {
            theme::raised()
        };
        c.fill_round(row, 8, face);
        if chosen || on {
            let bar = if on { rgb(0x0e, 0x9a, 0x6e) } else { theme::accent() };
            c.fill_round(Rect::new(row.x, row.y + 6, 4, row.h - 12), 2, bar);
        }
        let ty = row.y + (row.h - UI.line_height) / 2;
        let ink = if unusable.is_some() { theme::text_dim() } else { theme::text() };
        let font = if chosen { &UI_BOLD } else { &UI };
        c.draw_text_in(font, row.x + 18, ty, &fit(&server.name, row.w - 330), ink);
        // the protocol, as a chip
        let proto = server.protocol();
        let pw = UI.width(&proto) + 18;
        let chip = Rect::new(row.right() - 130 - pw, row.y + (row.h - 22) / 2, pw, 22);
        c.fill_round(chip, 11, mix(theme::stroke(), theme::light(), 90));
        c.text_centered(chip, &proto, theme::text_dim());
        // the delay, or why not
        let (text, color) = if busy {
            (String::from("connecting"), rgb(0xd0, 0x80, 0x10))
        } else if unusable.is_some() {
            (String::from("not supported"), theme::text_dim())
        } else {
            match self.pings.get(&server.link) {
                Some(Ok(ms)) => (
                    format!("{} ms", ms),
                    if *ms < 300 {
                        rgb(0x1e, 0xa0, 0x5a)
                    } else if *ms < 1000 {
                        rgb(0xc9, 0x8a, 0x00)
                    } else {
                        theme::error()
                    },
                ),
                Some(Err(_)) => (String::from("no answer"), theme::error()),
                None if self.testing() => (String::from("..."), theme::text_dim()),
                None => (String::new(), theme::text_dim()),
            }
        };
        let w = UI.width(&text);
        c.draw_text(row.right() - 16 - w, ty, &text, color);
    }
}
