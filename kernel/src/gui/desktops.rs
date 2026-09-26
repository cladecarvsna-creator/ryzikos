//! Virtual desktops and Task View, as in Windows 11.
//!
//! Every window lives on one desktop, and only the current desktop's
//! windows are shown (and get taskbar buttons unless pinned). Win+Ctrl+
//! Left and Right switch desktops with a slide, Win+Ctrl+D makes a new
//! one and Win+Ctrl+F4 closes the current one, moving its windows next
//! door.
//!
//! Task View (the taskbar button or Win+Tab) shows the windows of a
//! desktop as small live pictures, and the desktops in a row below.
//! Clicking a picture goes to that window; dragging it onto a desktop
//! moves the window there. Pointing at a desktop shows its windows.
//!
//! Alt+Tab shows the open windows in a row while Alt is held; each Tab
//! moves on, and letting go of Alt picks one.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use super::anim::{self, Tween, ONE};
use super::canvas::{rgb, Canvas, Rect};
use super::popup::{Builder, Cmd};
use super::text::UI_BOLD;
use super::theme;
use super::{fade_row, put_pixel, App, Desktop, APPS, BLACK, MAX_W, TASKBAR_H};
use crate::keyboard::{self, Key};
use crate::serial;

pub const MAX_DESKTOPS: usize = 8;
/// The row of desktops at the bottom of Task View.
const STRIP_H: i32 = 196;
pub const TILE_W: i32 = 208;
pub const TILE_H: i32 = 117;
const TITLE: i32 = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TvHit {
    Window(App),
    CloseWindow(App),
    Desk(usize),
    CloseDesk(usize),
    NewDesk,
}

#[derive(Clone, Copy)]
pub struct TvDrag {
    app: App,
    /// Where in the picture it was grabbed.
    grab: (i32, i32),
    size: (i32, i32),
    start: (i32, i32),
    moved: bool,
}

pub struct TaskView {
    pub open: bool,
    /// The desktop whose windows are shown.
    pub shown: usize,
    pub hover: Option<TvHit>,
    pub drag: Option<TvDrag>,
    /// The window picked with the arrow keys.
    pub selected: Option<App>,
}

impl TaskView {
    pub fn new() -> Self {
        Self {
            open: false,
            shown: 0,
            hover: None,
            drag: None,
            selected: None,
        }
    }
}

/// Alt+Tab: the windows, most recently used first, and the chosen one.
pub struct Switcher {
    pub apps: Vec<App>,
    pub selected: usize,
}

/// The name of desktop `i`.
pub fn desk_name(i: usize) -> crate::StackString<16> {
    let mut s = crate::StackString::new();
    let _ = write!(s, "Desktop {}", i + 1);
    s
}

impl Desktop<'_> {
    // ---- desktops ----------------------------------------------------------

    /// Hide the windows of other desktops and show this one's.
    pub(super) fn update_away(&mut self) {
        for app in APPS {
            let w = &mut self.windows[app.index()];
            let away = w.desk != self.current_desk;
            if away && w.anim.is_some() {
                // no animating on a desktop that can't be seen
                w.anim = None;
                if !w.open {
                    self.remove_from_order(app);
                }
            }
            self.windows[app.index()].away = away;
        }
    }

    pub(super) fn switch_desktop(&mut self, to: usize) {
        if to >= self.desk_count || to == self.current_desk {
            return;
        }
        self.close_menu();
        self.close_panel();
        self.close_search();
        self.popup = None;
        let slide = if self.tv.open {
            self.close_task_view(false);
            0
        } else if to > self.current_desk {
            1
        } else {
            -1
        };
        if slide != 0 {
            self.take_snapshot();
        }
        self.current_desk = to;
        self.update_away();
        self.focus_top();
        if self.focused.is_some_and(|a| self.windows[a.index()].away) {
            self.focused = None;
        }
        if slide != 0 {
            self.slide = Some((Tween::new(0, ONE, anim::ms(280)), slide));
        }
        self.peeked.clear();
        self.damage(self.screen());
        serial::write_str("desktops: switched to ");
        serial::write_str(desk_name(to).as_str());
        serial::write_str("\n");
    }

    /// Add a desktop at the end. Returns it, or None when there are
    /// already as many as can be.
    pub(super) fn new_desktop(&mut self) -> Option<usize> {
        if self.desk_count >= MAX_DESKTOPS {
            return None;
        }
        self.desk_count += 1;
        let d = self.desk_count - 1;
        serial::write_str("desktops: created ");
        serial::write_str(desk_name(d).as_str());
        serial::write_str("\n");
        self.damage(self.screen());
        Some(d)
    }

    /// Close desktop `d`; its windows go to the one on its left (or the
    /// right, for the first).
    pub(super) fn close_desktop(&mut self, d: usize) {
        if self.desk_count <= 1 || d >= self.desk_count {
            return;
        }
        let into = if d == 0 { 0 } else { d - 1 };
        for w in &mut self.windows {
            if w.desk == d {
                w.desk = into;
            } else if w.desk > d {
                w.desk -= 1;
            }
        }
        if self.current_desk > d || (self.current_desk == d && d > 0) {
            self.current_desk -= 1;
        }
        self.desk_count -= 1;
        self.tv.shown = self.current_desk;
        self.update_away();
        self.focus_top();
        self.damage(self.screen());
        serial::write_str("desktops: closed a desktop\n");
    }

    /// Put a window on desktop `d`, or a new one after the last.
    pub(super) fn move_to_desktop(&mut self, app: App, d: Option<usize>) {
        let Some(d) = d.or_else(|| self.new_desktop()) else {
            return;
        };
        let w = self.windows[app.index()];
        if !w.open || w.desk == d {
            return;
        }
        if w.visible() {
            self.damage(w.bounds());
        }
        self.windows[app.index()].desk = d;
        self.update_away();
        if self.focused == Some(app) {
            self.focus_top();
            if self.focused == Some(app) {
                self.focused = None;
            }
        }
        self.damage_taskbar();
        if self.tv.open {
            self.damage(self.screen());
        }
    }

    /// Open windows of desktop `d`, the top one first.
    pub(super) fn desk_windows(&self, d: usize) -> Vec<App> {
        self.order[..self.order_len]
            .iter()
            .rev()
            .copied()
            .filter(|a| {
                let w = &self.windows[a.index()];
                w.open && w.desk == d
            })
            .collect()
    }

    /// The desktop moving away on the left or right while the next one
    /// comes in: `p` of the way (ONE is done), `dir` 1 when going right.
    pub(super) fn present_slide(&self, p: i32, dir: i32) {
        let fb = &self.fb;
        let (w, h) = (self.width as usize, self.height as usize);
        let native = fb.bytes_per_pixel == 4
            && (fb.red.position, fb.green.position, fb.blue.position) == (16, 8, 0);
        let shift = (w as i64 * anim::ease_in_out(p) as i64 / ONE as i64) as usize;
        let dim = self.dim();
        let mut row = [0u32; MAX_W];
        let desk_rows = h - TASKBAR_H as usize;
        for y in 0..h {
            let (back, snap) = (&self.back[y * w..][..w], &self.snapshot[y * w..][..w]);
            if y >= desk_rows {
                row[..w].copy_from_slice(back);
            } else if dir > 0 {
                // the old one leaves to the left, the new one follows
                row[..w - shift].copy_from_slice(&snap[shift..]);
                row[w - shift..w].copy_from_slice(&back[..shift]);
            } else {
                row[..shift].copy_from_slice(&back[w - shift..]);
                row[shift..w].copy_from_slice(&snap[..w - shift]);
            }
            if dim > 0 {
                let plain = row;
                fade_row(&mut row[..w], &plain[..w], &BLACK[..w], dim);
            }
            if native {
                unsafe {
                    let dst = fb.base.add(y * fb.pitch) as *mut u32;
                    core::ptr::copy_nonoverlapping(row.as_ptr(), dst, w);
                }
            } else {
                for (x, &px) in row[..w].iter().enumerate() {
                    put_pixel(fb, x, y, px);
                }
            }
        }
    }

    // ---- Task View ---------------------------------------------------------

    pub(super) fn toggle_task_view(&mut self) {
        if self.tv.open {
            self.close_task_view(true);
        } else {
            self.open_task_view();
        }
    }

    fn open_task_view(&mut self) {
        self.close_menu();
        self.close_panel();
        self.close_search();
        self.popup = None;
        self.start_crossfade();
        self.tv.open = true;
        self.tv.shown = self.current_desk;
        self.tv.hover = None;
        self.tv.drag = None;
        self.tv.selected = None;
        self.damage(self.screen());
        serial::write_str("desktops: task view\n");
    }

    pub(super) fn close_task_view(&mut self, fade: bool) {
        if !self.tv.open {
            return;
        }
        if fade {
            self.start_crossfade();
        }
        self.tv.open = false;
        self.tv.drag = None;
        self.damage(self.screen());
    }

    /// Where the window pictures go.
    fn tv_area(&self) -> Rect {
        let top = 56;
        Rect::new(
            48,
            top,
            self.width - 96,
            self.height - TASKBAR_H - STRIP_H - top - 16,
        )
    }

    /// The pictures of desktop `d`'s windows: each window and the frame
    /// of its picture (its title goes above).
    pub(super) fn tv_layout(&self, d: usize) -> Vec<(App, Rect)> {
        let apps = self.desk_windows(d);
        let n = apps.len();
        if n == 0 {
            return Vec::new();
        }
        let area = self.tv_area();
        let gap = 32;
        // the column count that lets the pictures be biggest
        let mut best = (1, 0i64);
        for cols in 1..=n {
            let rows = n.div_ceil(cols);
            let cw = (area.w - (cols as i32 - 1) * gap) / cols as i32;
            let ch = (area.h - (rows as i32 - 1) * gap) / rows as i32 - TITLE;
            let scale = apps
                .iter()
                .map(|a| {
                    let r = self.windows[a.index()].rect;
                    (cw as i64 * 1000 / r.w as i64).min(ch as i64 * 1000 / r.h as i64)
                })
                .min()
                .unwrap_or(0)
                .min(600);
            if scale > best.1 {
                best = (cols, scale);
            }
        }
        let (cols, scale) = best;
        let rows = n.div_ceil(cols);
        let cell_h = apps
            .iter()
            .map(|a| self.windows[a.index()].rect.h as i64 * scale / 1000)
            .max()
            .unwrap_or(0) as i32
            + TITLE;
        let total_h = rows as i32 * cell_h + (rows as i32 - 1) * gap;
        let mut y = area.y + (area.h - total_h) / 2;
        let mut out = Vec::new();
        for row in apps.chunks(cols) {
            let sizes: Vec<(i32, i32)> = row
                .iter()
                .map(|a| {
                    let r = self.windows[a.index()].rect;
                    (
                        (r.w as i64 * scale / 1000) as i32,
                        (r.h as i64 * scale / 1000) as i32,
                    )
                })
                .collect();
            let row_w: i32 = sizes.iter().map(|s| s.0).sum::<i32>() + (row.len() as i32 - 1) * gap;
            let mut x = area.x + (area.w - row_w) / 2;
            for (a, (w, h)) in row.iter().zip(sizes) {
                let top = y + TITLE + (cell_h - TITLE - h) / 2;
                out.push((*a, Rect::new(x, top, w, h)));
                x += w + gap;
            }
            y += cell_h + gap;
        }
        out
    }

    /// The desktops in a row, then "New desktop".
    pub(super) fn desk_tiles(&self) -> Vec<(TvHit, Rect)> {
        let n = self.desk_count as i32 + i32::from(self.desk_count < MAX_DESKTOPS);
        let gap = 28;
        let total = n * TILE_W + (n - 1) * gap;
        let x0 = (self.width - total) / 2;
        let y = self.height - TASKBAR_H - STRIP_H + 30;
        (0..n)
            .map(|i| {
                let r = Rect::new(x0 + i * (TILE_W + gap), y, TILE_W, TILE_H);
                let hit = if (i as usize) < self.desk_count {
                    TvHit::Desk(i as usize)
                } else {
                    TvHit::NewDesk
                };
                (hit, r)
            })
            .collect()
    }

    fn close_box(frame: Rect) -> Rect {
        Rect::new(frame.right() - 28, frame.y - TITLE + 2, 28, 28)
    }

    fn tv_hit(&self, x: i32, y: i32) -> Option<TvHit> {
        for (hit, r) in self.desk_tiles() {
            if let TvHit::Desk(d) = hit {
                let close = Rect::new(r.right() - 26, r.y + 2, 24, 24);
                if self.desk_count > 1 && close.contains(x, y) {
                    return Some(TvHit::CloseDesk(d));
                }
            }
            if r.contains(x, y) {
                return Some(hit);
            }
        }
        for (app, r) in self.tv_layout(self.tv.shown) {
            if Self::close_box(r).contains(x, y) {
                return Some(TvHit::CloseWindow(app));
            }
            let whole = Rect::new(r.x, r.y - TITLE, r.w, r.h + TITLE);
            if whole.contains(x, y) {
                return Some(TvHit::Window(app));
            }
        }
        None
    }

    pub(super) fn tv_hover(&mut self, x: i32, y: i32) {
        let hit = self.tv_hit(x, y);
        if hit != self.tv.hover {
            self.tv.hover = hit;
            // pointing at a desktop shows its windows
            if let Some(TvHit::Desk(d)) = hit {
                self.tv.shown = d;
            }
            self.damage(self.screen());
        }
    }

    pub(super) fn tv_press(&mut self, x: i32, y: i32, right: bool) {
        let hit = self.tv_hit(x, y);
        if right {
            if let Some(TvHit::Window(app)) = hit {
                let mut b = Builder::default();
                for d in (0..self.desk_count).filter(|&d| d != self.windows[app.index()].desk) {
                    let mut label = String::from("Move to ");
                    label.push_str(desk_name(d).as_str());
                    b = b.item(&label, Cmd::MoveTo(app, d));
                }
                if self.desk_count < MAX_DESKTOPS {
                    b = b.item("Move to new desktop", Cmd::MoveToNew(app));
                }
                let menu = b
                    .sep()
                    .item("Close", Cmd::Close(app))
                    .at(x, y, false, self.screen());
                self.show_popup(menu);
            }
            return;
        }
        match hit {
            Some(TvHit::Window(app)) => {
                let r = self
                    .tv_layout(self.tv.shown)
                    .into_iter()
                    .find(|(a, _)| *a == app)
                    .map(|(_, r)| r)
                    .unwrap_or_default();
                self.tv.drag = Some(TvDrag {
                    app,
                    grab: (x - r.x, y - r.y),
                    size: (r.w, r.h),
                    start: (x, y),
                    moved: false,
                });
            }
            Some(TvHit::CloseWindow(app)) => {
                self.close(app);
                self.damage(self.screen());
            }
            Some(TvHit::Desk(d)) => {
                self.close_task_view(true);
                self.switch_desktop(d);
            }
            Some(TvHit::CloseDesk(d)) => self.close_desktop(d),
            Some(TvHit::NewDesk) => {
                if let Some(d) = self.new_desktop() {
                    self.tv.shown = d;
                }
            }
            None => self.close_task_view(true),
        }
    }

    pub(super) fn tv_move(&mut self, x: i32, y: i32) {
        let Some(mut drag) = self.tv.drag else {
            return;
        };
        if !drag.moved && (x - drag.start.0).abs() + (y - drag.start.1).abs() < 6 {
            return;
        }
        drag.moved = true;
        self.tv.drag = Some(drag);
        let over = self.tv_hit(x, y);
        self.tv.hover = over.filter(|h| matches!(h, TvHit::Desk(_) | TvHit::NewDesk));
        self.damage(self.screen());
    }

    pub(super) fn tv_release(&mut self) {
        let Some(drag) = self.tv.drag.take() else {
            return;
        };
        if !drag.moved {
            // a click: go to the window
            let d = self.windows[drag.app.index()].desk;
            self.close_task_view(true);
            if d != self.current_desk {
                self.switch_desktop(d);
            }
            self.open(drag.app);
            return;
        }
        match self.tv_hit(self.mouse_x, self.mouse_y) {
            Some(TvHit::Desk(d) | TvHit::CloseDesk(d)) => self.move_to_desktop(drag.app, Some(d)),
            Some(TvHit::NewDesk) => self.move_to_desktop(drag.app, None),
            _ => {}
        }
        self.damage(self.screen());
    }

    pub(super) fn tv_key(&mut self, key: Key) {
        let apps = self.desk_windows(self.tv.shown);
        let at = self
            .tv
            .selected
            .and_then(|s| apps.iter().position(|&a| a == s));
        match key {
            Key::Escape | Key::Super => self.close_task_view(true),
            Key::Right | Key::Down | Key::Char('\t') if !apps.is_empty() => {
                let i = at.map_or(0, |i| (i + 1) % apps.len());
                self.tv.selected = Some(apps[i]);
            }
            Key::Left | Key::Up if !apps.is_empty() => {
                let i = at.map_or(0, |i| (i + apps.len() - 1) % apps.len());
                self.tv.selected = Some(apps[i]);
            }
            Key::Enter => {
                let pick = self.tv.selected.or(apps.first().copied());
                let shown = self.tv.shown;
                self.close_task_view(true);
                if shown != self.current_desk {
                    self.switch_desktop(shown);
                }
                if let Some(app) = pick {
                    self.open(app);
                }
            }
            Key::Delete => {
                if let Some(app) = self.tv.selected {
                    self.close(app);
                }
            }
            _ => return,
        }
        self.damage(self.screen());
    }

    /// Draw window `app` shrunk into `frame`, with rounded corners.
    fn draw_thumb(&self, c: &mut Canvas, scratch: &mut [u32], app: App, frame: Rect) {
        let r = self.windows[app.index()].rect;
        if !c.visible(frame.inset(-12)) || (r.w * r.h) as usize > scratch.len() {
            return;
        }
        c.shadow(frame, 8, 12, 3, 110);
        {
            let mut side = Canvas::new(scratch, r.w as usize, r.h as usize);
            self.draw_window_body(&mut side, app, Rect::new(0, 0, r.w, r.h));
        }
        let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
        m.clip_round(frame, 8);
        m.blit_smooth(frame, scratch, r.w, r.h);
    }

    pub(super) fn draw_task_view(&self, c: &mut Canvas, scratch: &mut [u32]) {
        let desk = Rect::new(0, 0, self.width, self.height - TASKBAR_H);
        c.blit(
            0,
            0,
            self.width,
            self.height,
            self.login.backdrop(),
            self.width as usize,
        );
        let dragging = self.tv.drag.filter(|d| d.moved);
        let layout = self.tv_layout(self.tv.shown);
        if layout.is_empty() {
            let r = Rect::new(
                0,
                self.tv_area().y + self.tv_area().h / 2 - 12,
                self.width,
                24,
            );
            c.text_centered_in(&UI_BOLD, r, "No windows open on this desktop", 0xffffff);
        }
        for &(app, frame) in &layout {
            if dragging.is_some_and(|d| d.app == app) {
                continue;
            }
            let lit = matches!(self.tv.hover, Some(TvHit::Window(a) | TvHit::CloseWindow(a)) if a == app)
                || self.tv.selected == Some(app);
            let whole = Rect::new(
                frame.x - 6,
                frame.y - TITLE - 4,
                frame.w + 12,
                frame.h + TITLE + 10,
            );
            if !c.visible(whole.inset(-16)) {
                continue;
            }
            if lit {
                c.fill_round_alpha(whole, 10, 0xffffff, 46);
                c.outline_round_alpha(whole, 10, 0xffffff, 150);
            }
            self.icons
                .draw_small(c, app, frame.x + 2, frame.y - TITLE + 8);
            let title = super::search::fit(&self.window_title(app), frame.w - 64);
            c.draw_text(frame.x + 26, frame.y - TITLE + 7, &title, 0xffffff);
            if lit {
                let b = Self::close_box(frame);
                let on = self.tv.hover == Some(TvHit::CloseWindow(app));
                if on {
                    c.fill_round(b, 4, rgb(0xc4, 0x2b, 0x1c));
                }
                let (x, y) = (b.x + 9, b.y + 9);
                c.line(x, y, x + 9, y + 9, 0xffffff);
                c.line(x + 9, y, x, y + 9, 0xffffff);
            }
            self.draw_thumb(c, scratch, app, frame);
        }

        // the desktops
        let strip = Rect::new(0, self.height - TASKBAR_H - STRIP_H, self.width, STRIP_H);
        c.fill_round_alpha(strip, 0, rgb(0x20, 0x28, 0x3c), 110);
        for (hit, r) in self.desk_tiles() {
            if !c.visible(r.inset(-8).union(&r.offset(0, 40))) {
                continue;
            }
            let lit = self.tv.hover == Some(hit)
                || matches!((hit, self.tv.hover), (TvHit::Desk(d), Some(TvHit::CloseDesk(e))) if d == e);
            match hit {
                TvHit::Desk(d) => {
                    self.draw_mini_desk(c, r, d);
                    let current = d == self.current_desk;
                    if current || lit || d == self.tv.shown {
                        let color = if current {
                            theme::accent_light()
                        } else {
                            0xffffff
                        };
                        c.outline_round(r.inset(-3), 8, color);
                        c.outline_round(r.inset(-2), 7, color);
                    }
                    let label = Rect::new(r.x, r.bottom() + 8, r.w, 22);
                    c.text_centered_in(&UI_BOLD, label, desk_name(d).as_str(), 0xffffff);
                    if lit && self.desk_count > 1 {
                        let b = Rect::new(r.right() - 26, r.y + 2, 24, 24);
                        let face = if self.tv.hover == Some(TvHit::CloseDesk(d)) {
                            rgb(0xc4, 0x2b, 0x1c)
                        } else {
                            rgb(0x30, 0x34, 0x40)
                        };
                        c.fill_round(b, 4, face);
                        let (x, y) = (b.x + 8, b.y + 8);
                        c.line(x, y, x + 8, y + 8, 0xffffff);
                        c.line(x + 8, y, x, y + 8, 0xffffff);
                    }
                }
                _ => {
                    let face = if lit { 140 } else { 70 };
                    c.fill_round_alpha(r, 8, 0xffffff, face);
                    c.outline_round_alpha(r, 8, 0xffffff, 120);
                    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
                    c.fill_rect(cx - 12, cy - 1, 24, 3, 0xffffff);
                    c.fill_rect(cx - 1, cy - 12, 3, 24, 0xffffff);
                    let label = Rect::new(r.x, r.bottom() + 8, r.w, 22);
                    c.text_centered_in(&UI_BOLD, label, "New desktop", 0xffffff);
                }
            }
        }

        // the picture being dragged, under the pointer
        if let Some(d) = dragging {
            let (w, h) = (d.size.0 * 2 / 5, d.size.1 * 2 / 5);
            let frame = Rect::new(
                self.mouse_x - d.grab.0 * 2 / 5,
                self.mouse_y - d.grab.1 * 2 / 5,
                w,
                h,
            );
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_to(desk);
            self.draw_thumb(&mut m, scratch, d.app, frame);
        }
    }

    /// A desktop's small picture: the wallpaper and its windows as boxes.
    fn draw_mini_desk(&self, c: &mut Canvas, r: Rect, d: usize) {
        {
            let mut m = c.sub(Rect::new(0, 0, c.width, c.height));
            m.clip_round(r, 6);
            m.blit(r.x, r.y, TILE_W, TILE_H, &self.wall_thumb, TILE_W as usize);
            let (sx, sy) = (TILE_W as i64, self.width as i64);
            let scale = |v: i32| (v as i64 * sx / sy) as i32;
            for app in self.desk_windows(d).into_iter().rev() {
                let w = self.windows[app.index()];
                if w.minimized {
                    continue;
                }
                let f = Rect::new(
                    r.x + scale(w.rect.x),
                    r.y + scale(w.rect.y),
                    scale(w.rect.w).max(6),
                    scale(w.rect.h).max(4),
                );
                m.fill_round(f, 2, rgb(0xf6, 0xf7, 0xfa));
                m.fill_rect(f.x, f.y, f.w, 3, rgb(0xdc, 0xe0, 0xea));
                m.outline_round(f, 2, rgb(0x70, 0x78, 0x88));
                if f.w > 24 && f.h > 22 {
                    self.icons
                        .draw_small(&mut m, app, f.x + f.w / 2 - 8, f.y + f.h / 2 - 6);
                }
            }
        }
        c.outline_round_alpha(r, 6, 0xffffff, 60);
    }

    // ---- Alt+Tab -----------------------------------------------------------

    /// Alt+Tab pressed: show the switcher, or move on in it.
    pub(super) fn alt_tab(&mut self) {
        if let Some(s) = &mut self.switcher {
            let n = s.apps.len();
            s.selected = if keyboard::shift_held() {
                (s.selected + n - 1) % n
            } else {
                (s.selected + 1) % n
            };
        } else {
            let apps = self.desk_windows(self.current_desk);
            if apps.is_empty() {
                return;
            }
            self.close_menu();
            self.close_search();
            self.close_panel();
            self.close_task_view(false);
            let selected = usize::from(apps.len() > 1);
            self.switcher = Some(Switcher { apps, selected });
        }
        self.damage(self.switcher_rect().inset(-24));
    }

    /// Alt let go: go to the chosen window.
    pub(super) fn alt_up(&mut self) {
        let Some(s) = self.switcher.take() else {
            return;
        };
        self.damage(self.switcher_rect_for(s.apps.len()).inset(-24));
        if let Some(&app) = s.apps.get(s.selected) {
            self.open(app);
        }
    }

    pub(super) fn cancel_switcher(&mut self) {
        if let Some(s) = self.switcher.take() {
            self.damage(self.switcher_rect_for(s.apps.len()).inset(-24));
        }
    }

    fn switcher_cell(&self, n: usize) -> (i32, i32) {
        let w = ((self.width - 160) / n.max(1) as i32).min(280);
        (w, w * 5 / 8 + TITLE + 16)
    }

    fn switcher_rect_for(&self, n: usize) -> Rect {
        let (cw, ch) = self.switcher_cell(n);
        let w = cw * n as i32 + 32;
        let h = ch + 32;
        Rect::new(
            (self.width - w) / 2,
            (self.height - TASKBAR_H - h) / 2,
            w,
            h,
        )
    }

    fn switcher_rect(&self) -> Rect {
        let n = self.switcher.as_ref().map_or(1, |s| s.apps.len());
        self.switcher_rect_for(n)
    }

    pub(super) fn draw_switcher(&self, c: &mut Canvas, scratch: &mut [u32]) {
        let Some(s) = &self.switcher else {
            return;
        };
        let p = self.switcher_rect();
        if !c.visible(p.inset(-24)) {
            return;
        }
        c.shadow(p, 10, 20, 4, 150);
        c.fill_round_alpha(p, 10, rgb(0x24, 0x28, 0x34), 236);
        c.outline_round_alpha(p, 10, 0xffffff, 50);
        let (cw, ch) = self.switcher_cell(s.apps.len());
        for (i, &app) in s.apps.iter().enumerate() {
            let cell = Rect::new(p.x + 16 + i as i32 * cw, p.y + 16, cw, ch);
            if i == s.selected {
                c.outline_round(cell.inset(2), 8, 0xffffff);
                c.outline_round(cell.inset(3), 7, 0xffffff);
            }
            let inner = cell.inset(12);
            self.icons.draw_small(c, app, inner.x, inner.y + 4);
            let title = super::search::fit(&self.window_title(app), inner.w - 24);
            c.draw_text(inner.x + 24, inner.y + 3, &title, 0xffffff);
            // the picture, as big as fits under the title
            let r = self.windows[app.index()].rect;
            let area = Rect::new(inner.x, inner.y + TITLE - 4, inner.w, inner.h - TITLE + 4);
            let scale = (area.w * 1000 / r.w).min(area.h * 1000 / r.h);
            let (w, h) = (r.w * scale / 1000, r.h * scale / 1000);
            let frame = Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h);
            self.draw_thumb(c, scratch, app, frame);
        }
    }

    // ---- crossfades --------------------------------------------------------

    /// Fade from what the screen shows now to what it shows next.
    pub(super) fn start_crossfade(&mut self) {
        self.take_snapshot();
        self.crossfade = Some(Tween::new(0, ONE, anim::ms(160)));
    }

    /// Mix for the crossfade: 256 is all old screen.
    pub(super) fn crossfade_alpha(t: Tween) -> u32 {
        (ONE - t.value()) as u32
    }
}
