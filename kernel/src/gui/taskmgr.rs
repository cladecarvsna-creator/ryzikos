//! Task Manager: the open apps with their state and memory, a button to
//! end one or switch to it, and graphs of how busy the processor is and
//! how much of the kernel's memory is in use. Ctrl+Shift+Esc opens it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::text::{HEADING, UI_BOLD};
use super::{theme, App, MouseEvent, MouseKind};
use crate::interrupts;

pub const CLIENT_W: i32 = 860;
pub const CLIENT_H: i32 = 600;

/// The window's size now; it opens at CLIENT_W x CLIENT_H.
fn cw() -> i32 {
    super::client_w(super::App::TaskManager)
}
fn ch() -> i32 {
    super::client_h(super::App::TaskManager)
}

const SIDE_W: i32 = 190;
const ROW_H: i32 = 34;
const HEADER_Y: i32 = 70;
const LIST_Y: i32 = HEADER_Y + 30;
/// Samples in a graph: one a second, a minute in all.
const HISTORY: usize = 60;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Apps,
    Performance,
}

/// An open window, as the desktop describes it.
#[derive(Clone)]
pub struct Row {
    pub app: App,
    pub name: String,
    pub state: &'static str,
    /// The window's picture, in bytes.
    pub memory: u64,
}

/// Something the kernel runs besides the windows.
struct Service {
    name: &'static str,
    state: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    EndTask,
    SwitchTo,
}

pub struct TaskManager {
    page: Page,
    rows: Vec<Row>,
    selected: Option<App>,
    pressed: Option<Button>,
    /// Processor load in percent, oldest first.
    cpu: Vec<u32>,
    /// Kernel heap in use, in percent.
    memory: Vec<u32>,
    last_sample: (u64, u64),
}

fn nav_rect(i: i32) -> Rect {
    Rect::new(10, 64 + i * 44, SIDE_W - 20, 38)
}

fn button_rect(b: Button) -> Rect {
    let w = 130;
    let right = cw() - 20;
    match b {
        Button::EndTask => Rect::new(right - w, ch() - 52, w, 34),
        Button::SwitchTo => Rect::new(right - 2 * w - 10, ch() - 52, w, 34),
    }
}

fn row_rect(i: usize) -> Rect {
    Rect::new(SIDE_W + 12, LIST_Y + i as i32 * ROW_H, cw() - SIDE_W - 32, ROW_H)
}

pub fn size_text(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{}.{} GB", bytes >> 30, (bytes % (1 << 30)) * 10 >> 30)
    } else if bytes >= 1024 * 1024 {
        format!("{}.{} MB", bytes >> 20, (bytes % (1 << 20)) * 10 >> 20)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn uptime_text() -> String {
    let secs = interrupts::ticks() / interrupts::TIMER_HZ;
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn heap_percent() -> u32 {
    let total = crate::heap::total_bytes() as u64;
    let used = total - crate::heap::free_bytes() as u64;
    (used * 100 / total.max(1)) as u32
}

fn services() -> Vec<Service> {
    let net = if crate::net::configured() {
        format!("Online, {}", crate::net::address().unwrap_or_default())
    } else if crate::net::link().is_some() {
        String::from("Getting an address")
    } else {
        String::from("No network card")
    };
    let disk = match crate::fs::storage() {
        crate::fs::Storage::Disk => format!("{} drives", crate::fs::drives().len()),
        _ => String::from("Files in memory only"),
    };
    let heap_total = crate::heap::total_bytes() as u64;
    let heap_used = heap_total - crate::heap::free_bytes() as u64;
    alloc::vec![
        Service {
            name: "Kernel",
            state: format!("{} of {} used", size_text(heap_used), size_text(heap_total)),
        },
        Service { name: "Network", state: net },
        Service {
            name: "Sound",
            state: String::from(if crate::sound::available() { "ES1370" } else { "No sound card" }),
        },
        Service { name: "Disks", state: disk },
    ]
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            page: Page::Apps,
            rows: Vec::new(),
            selected: None,
            pressed: None,
            cpu: Vec::new(),
            memory: Vec::new(),
            last_sample: (0, 0),
        }
    }

    /// The desktop's list of open windows.
    pub fn set_rows(&mut self, rows: Vec<Row>) {
        if !rows.iter().any(|r| Some(r.app) == self.selected) {
            self.selected = None;
        }
        self.rows = rows;
    }

    /// Take a sample once a second. Returns true when there is a new one.
    pub fn tick(&mut self) -> bool {
        let now = interrupts::ticks();
        let idle = interrupts::idle_ticks();
        let (then, idle_then) = self.last_sample;
        if then != 0 && now - then < interrupts::TIMER_HZ {
            return false;
        }
        self.last_sample = (now, idle);
        if then == 0 {
            return false;
        }
        let elapsed = (now - then).max(1);
        let busy = elapsed.saturating_sub(idle - idle_then);
        push(&mut self.cpu, (busy * 100 / elapsed).min(100) as u32);
        push(&mut self.memory, heap_percent());
        true
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseKind::Down { right: false } => {
                for (i, page) in [Page::Apps, Page::Performance].into_iter().enumerate() {
                    if nav_rect(i as i32).contains(ev.x, ev.y) {
                        self.page = page;
                        return true;
                    }
                }
                if self.page != Page::Apps {
                    return false;
                }
                for b in [Button::EndTask, Button::SwitchTo] {
                    if button_rect(b).contains(ev.x, ev.y) && self.selected.is_some() {
                        self.pressed = Some(b);
                        return true;
                    }
                }
                let hit = (0..self.rows.len()).find(|&i| row_rect(i).contains(ev.x, ev.y));
                self.selected = hit.map(|i| self.rows[i].app);
                true
            }
            MouseKind::Up => {
                let Some(b) = self.pressed.take() else {
                    return false;
                };
                if let (true, Some(app)) = (button_rect(b).contains(ev.x, ev.y), self.selected) {
                    match b {
                        Button::EndTask => {
                            super::request_close(app);
                            self.selected = None;
                        }
                        Button::SwitchTo => {
                            super::request_open(app);
                        }
                    }
                }
                true
            }
            _ => false,
        }
    }

    // ---- drawing -----------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas) {
        c.fill_rect(0, 0, cw(), ch(), theme::light());
        c.fill_rect(0, 0, SIDE_W, ch(), theme::face());
        c.fill_rect(SIDE_W, 0, 1, ch(), theme::stroke());
        c.draw_text_in(&UI_BOLD, 22, 22, "Task Manager", theme::text());
        for (i, (page, label)) in [(Page::Apps, "Apps"), (Page::Performance, "Performance")]
            .into_iter()
            .enumerate()
        {
            let r = nav_rect(i as i32);
            if page == self.page {
                c.fill_round(r, 6, theme::selection());
                c.fill_round(Rect::new(r.x + 2, r.y + 10, 3, r.h - 20), 1, theme::accent());
            }
            c.draw_text(r.x + 18, r.y + (r.h - 17) / 2, label, theme::text());
        }
        match self.page {
            Page::Apps => self.draw_apps(c),
            Page::Performance => self.draw_performance(c),
        }
    }

    fn draw_apps(&self, c: &mut Canvas) {
        let x = SIDE_W + 24;
        c.draw_text_in(&HEADING, x, 20, "Apps", theme::text());
        let r = row_rect(0);
        let (state_x, mem_x) = (r.x + r.w - 330, r.x + r.w - 110);
        c.draw_text(x, HEADER_Y, "Name", theme::text_dim());
        c.draw_text(state_x, HEADER_Y, "Status", theme::text_dim());
        c.draw_text(mem_x, HEADER_Y, "Memory", theme::text_dim());
        c.fill_rect(r.x, LIST_Y - 6, r.w, 1, theme::stroke());
        if self.rows.is_empty() {
            c.draw_text(x, LIST_Y + 8, "No apps are open.", theme::text_dim());
        }
        for (i, row) in self.rows.iter().enumerate() {
            let r = row_rect(i);
            if Some(row.app) == self.selected {
                c.fill_round(r, 5, theme::selection());
            }
            let ty = r.y + (r.h - 17) / 2;
            super::icons::get().draw_medium(c, row.app, r.x + 6, r.y + (r.h - 24) / 2);
            let name = super::search::fit(&row.name, state_x - x - 50);
            c.draw_text(r.x + 40, ty, &name, theme::text());
            c.draw_text(state_x, ty, row.state, theme::text_dim());
            c.draw_text(mem_x, ty, &size_text(row.memory), theme::text());
        }

        // what runs in the kernel itself
        let top = LIST_Y + self.rows.len().max(1) as i32 * ROW_H + 28;
        c.draw_text_in(&UI_BOLD, x, top, "System", theme::text());
        for (i, s) in services().iter().enumerate() {
            let y = top + 30 + i as i32 * 26;
            if y > ch() - 80 {
                break;
            }
            c.draw_text(x + 16, y, s.name, theme::text());
            c.draw_text(state_x, y, &s.state, theme::text_dim());
        }

        let enabled = self.selected.is_some();
        for (b, label) in [(Button::SwitchTo, "Switch to"), (Button::EndTask, "End task")] {
            let r = button_rect(b);
            if enabled {
                theme::button(c, r, label, self.pressed == Some(b));
            } else {
                c.fill_round(r, theme::CONTROL_RADIUS, theme::face());
                c.outline_round(r, theme::CONTROL_RADIUS, theme::stroke());
                c.text_centered(r, label, mix(theme::text_dim(), theme::face(), 90));
            }
        }
    }

    fn draw_performance(&self, c: &mut Canvas) {
        let x = SIDE_W + 24;
        c.draw_text_in(&HEADING, x, 20, "Performance", theme::text());
        let w = cw() - x - 24;
        let cpu_now = self.cpu.last().copied().unwrap_or(0);
        let mem_now = heap_percent();
        let blue = rgb(0x1a, 0x73, 0xe8);
        let purple = rgb(0x8a, 0x3c, 0xd8);
        graph(c, Rect::new(x, 96, w, 150), &self.cpu, blue, "Processor", &format!("{}%", cpu_now));
        let heap_total = crate::heap::total_bytes() as u64;
        let heap_used = heap_total - crate::heap::free_bytes() as u64;
        graph(
            c,
            Rect::new(x, 300, w, 150),
            &self.memory,
            purple,
            "Memory",
            &format!("{} of {} ({}%)", size_text(heap_used), size_text(heap_total), mem_now),
        );
        let facts = [
            ("Up time", uptime_text()),
            ("Open apps", format!("{}", self.rows.len())),
            ("Processor", String::from("1 core, x86_64")),
        ];
        for (i, (k, v)) in facts.iter().enumerate() {
            let fx = x + i as i32 * (w / 3);
            c.draw_text(fx, 490, k, theme::text_dim());
            c.draw_text_in(&UI_BOLD, fx, 512, v, theme::text());
        }
        c.draw_text(
            x,
            ch() - 40,
            "Processor: time not spent waiting. Memory: the kernel's heap.",
            theme::text_dim(),
        );
    }
}

fn push(v: &mut Vec<u32>, value: u32) {
    if v.len() >= HISTORY {
        v.remove(0);
    }
    v.push(value);
}

/// A line graph of percentages over the last minute, newest on the right.
fn graph(c: &mut Canvas, r: Rect, values: &[u32], color: Color, title: &str, now: &str) {
    c.draw_text_in(&UI_BOLD, r.x, r.y - 26, title, theme::text());
    let nw = super::text::UI.width(now);
    c.draw_text(r.right() - nw, r.y - 26, now, theme::text_dim());
    c.fill(r, mix(color, theme::light(), 245));
    c.outline_round(r, 2, mix(color, theme::light(), 140));
    // grid lines every quarter
    for k in 1..4 {
        let y = r.y + r.h * k / 4;
        c.fill_rect(r.x + 1, y, r.w - 2, 1, mix(color, theme::light(), 225));
    }
    if values.is_empty() {
        c.text_centered(r, "Measuring...", theme::text_dim());
        return;
    }
    let step = (r.w - 2) as f32 / (HISTORY - 1) as f32;
    let start = HISTORY - values.len();
    let point = |i: usize, v: u32| {
        let px = r.x + 1 + ((start + i) as f32 * step) as i32;
        let py = r.bottom() - 2 - (v.min(100) as i32 * (r.h - 4) / 100);
        (px, py)
    };
    // the area under the line, then the line
    let mut poly: Vec<(i32, i32)> = values.iter().enumerate().map(|(i, &v)| point(i, v)).collect();
    let last_x = poly.last().map_or(r.x, |p| p.0);
    let first_x = poly[0].0;
    poly.push((last_x, r.bottom() - 1));
    poly.push((first_x, r.bottom() - 1));
    if values.len() > 1 {
        c.fill_polygon(&poly, mix(color, theme::light(), 190));
    }
    for i in 1..values.len() {
        let (a, b) = (point(i - 1, values[i - 1]), point(i, values[i]));
        c.line(a.0, a.1, b.0, b.1, color);
        c.line(a.0, a.1 - 1, b.0, b.1 - 1, color);
    }
}
