//! The taskbar, as in Windows 11: Start, the search box, Task View and
//! the app buttons in the middle; the ^ button for hidden icons, the
//! layout, network and volume, the clock and "Show desktop" at the right.
//!
//! Pinned apps always have a button; other apps get one while they are
//! open on the current desktop. A dot under a button shows the app is
//! open, a longer one that it has the keyboard. Right-click pins and
//! unpins; the pins are kept in the user's AppData folder on the disk.
//! Resting the mouse on a button shows what it is.

use alloc::string::String;
use alloc::vec::Vec;

use super::anim::ONE;
use super::canvas::{Canvas, Rect};
use super::icons::{Pic, MEDIUM};
use super::popup::{Builder, Cmd};
use super::search::{self, Search};
use super::text::UI;
use super::theme;
use super::tray::{self, Panel};
use super::{draw_start_logo, App, Desktop, Hover, APPS, SPREAD, TASKBAR_H};
use crate::{fs, interrupts, serial, users};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskItem {
    Start,
    Search,
    TaskView,
    App(App),
}

const BUTTON: i32 = 44;
const GAP: i32 = 4;
const SEARCH_W: i32 = 216;
/// How long the mouse rests on something before its tooltip shows.
const TIP_DELAY: u64 = interrupts::TIMER_HZ * 6 / 10;

/// Pinned for a new user.
pub const DEFAULT_PINS: [App; 5] = [
    App::Explorer,
    App::Browser,
    App::Terminal,
    App::Notepad,
    App::Settings,
];

/// The icons behind ^: the disk, Settings and About.
const HIDDEN: [&str; 3] = ["Local Disk (C:)", "Settings", "About EverOS"];

impl Desktop<'_> {
    // ---- buttons -----------------------------------------------------------

    /// Everything in the middle of the taskbar, from the left.
    pub(super) fn task_items(&self) -> Vec<TaskItem> {
        let mut out = alloc::vec![TaskItem::Start, TaskItem::Search, TaskItem::TaskView];
        out.extend(self.pins.iter().map(|&a| TaskItem::App(a)));
        // then open apps that are not pinned, in the order they opened
        let mut open: Vec<App> = APPS
            .into_iter()
            .filter(|a| {
                let w = &self.windows[a.index()];
                w.open && !w.away && !self.pins.contains(a)
            })
            .collect();
        open.sort_by_key(|a| self.windows[a.index()].opened);
        out.extend(open.into_iter().map(TaskItem::App));
        out
    }

    fn item_width(item: TaskItem) -> i32 {
        match item {
            TaskItem::Search => SEARCH_W,
            _ => BUTTON,
        }
    }

    /// The buttons and where they are, centred like Windows 11.
    pub(super) fn task_layout(&self) -> Vec<(TaskItem, Rect)> {
        let items = self.task_items();
        let total: i32 = items
            .iter()
            .map(|&i| Self::item_width(i) + GAP)
            .sum::<i32>()
            - GAP;
        let mut x = (self.width - total) / 2;
        let top = self.height - TASKBAR_H;
        items
            .into_iter()
            .map(|item| {
                let w = Self::item_width(item);
                let r = if item == TaskItem::Search {
                    Rect::new(x + 2, top + 7, w - 4, 34)
                } else {
                    Rect::new(x, top + 4, w, 40)
                };
                x += w + GAP;
                (item, r)
            })
            .collect()
    }

    pub(super) fn task_rect(&self, item: TaskItem) -> Option<Rect> {
        self.task_layout()
            .into_iter()
            .find(|&(i, _)| i == item)
            .map(|(_, r)| r)
    }

    pub(super) fn task_at(&self, x: i32, y: i32) -> Option<TaskItem> {
        self.task_layout()
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(i, _)| i)
    }

    /// Tray button `i` from the left: the layout, quick settings, the
    /// clock, and the ^ for hidden icons (left of the layout).
    pub(super) fn tray_rect(&self, i: usize) -> Rect {
        let top = self.height - TASKBAR_H + 4;
        match i {
            0 => Rect::new(self.width - 240, top, 52, 40),
            1 => Rect::new(self.width - 184, top, 72, 40),
            2 => Rect::new(self.width - 110, top, 100, 40),
            _ => Rect::new(self.width - 280, top, 36, 40),
        }
    }

    /// The thin "Show desktop" strip at the very right.
    pub(super) fn show_desktop_rect(&self) -> Rect {
        Rect::new(self.width - 8, self.height - TASKBAR_H, 8, TASKBAR_H)
    }

    pub(super) fn search_panel(&self) -> Rect {
        Search::panel(self.width, self.height, TASKBAR_H)
    }

    // ---- clicks ------------------------------------------------------------

    pub(super) fn taskbar_press(&mut self, x: i32, y: i32, right: bool) {
        if right {
            return self.taskbar_menu(x, y);
        }
        if self.show_desktop_rect().contains(x, y) {
            return self.toggle_show_desktop();
        }
        match (0..4).find(|&i| self.tray_rect(i).contains(x, y)) {
            Some(0) => {
                self.toggle_layout = true;
                return;
            }
            Some(1) => return self.open_panel(Panel::Quick),
            Some(2) => return self.open_panel(Panel::Calendar),
            Some(_) => return self.open_panel(Panel::Hidden),
            None => {}
        }
        match self.task_at(x, y) {
            Some(TaskItem::Start) => self.open_menu(),
            Some(TaskItem::Search) => self.open_search(),
            Some(TaskItem::TaskView) => self.toggle_task_view(),
            Some(TaskItem::App(app)) => {
                let w = self.windows[app.index()];
                if w.open && w.away {
                    // it is on another desktop: go there
                    self.switch_desktop(w.desk);
                    self.focus(app);
                } else if w.visible() && self.focused == Some(app) {
                    self.minimize(app);
                } else {
                    self.open(app);
                }
            }
            None => {}
        }
    }

    fn taskbar_menu(&mut self, x: i32, y: i32) {
        let top = self.height - TASKBAR_H - 8;
        let menu = match self.task_at(x, y) {
            Some(TaskItem::App(app)) => {
                let r = self.task_rect(TaskItem::App(app)).unwrap_or_default();
                let mut b = Builder::default().item(app.title(), Cmd::Open(app)).sep();
                b = if self.pins.contains(&app) {
                    b.item("Unpin from taskbar", Cmd::Unpin(app))
                } else {
                    b.item("Pin to taskbar", Cmd::Pin(app))
                };
                if self.windows[app.index()].open {
                    b = b.item("Close window", Cmd::Close(app));
                }
                let x = r.x + r.w / 2 - super::widgets::MENU_W / 2;
                b.at(x, top, true, self.screen())
            }
            Some(TaskItem::Start) => self.start_menu_popup(x, top),
            _ => Builder::default()
                .item("Search", Cmd::Search)
                .item("Task View", Cmd::TaskView)
                .item("Show desktop", Cmd::ShowDesktop)
                .sep()
                .item("Taskbar settings", Cmd::Open(App::Settings))
                .at(x - 20, top, true, self.screen()),
        };
        self.show_popup(menu);
    }

    /// The menu of Start's right-click and Win+X.
    pub(super) fn start_menu_popup(&self, x: i32, y: i32) -> super::popup::Popup {
        Builder::default()
            .item("Terminal", Cmd::Open(App::Terminal))
            .item("File Explorer", Cmd::Open(App::Explorer))
            .item("Settings", Cmd::Open(App::Settings))
            .keyed("Search", "Win+S", Cmd::Search)
            .keyed("Task View", "Win+Tab", Cmd::TaskView)
            .sep()
            .keyed("Lock", "Win+L", Cmd::Lock)
            .item("Sign out", Cmd::SignOut)
            .item("Restart", Cmd::Restart)
            .item("Shut down", Cmd::ShutDown)
            .sep()
            .keyed("Desktop", "Win+D", Cmd::ShowDesktop)
            .at(x, y, true, self.screen())
    }

    /// Minimise every window on this desktop, or bring back the ones
    /// this minimised.
    pub(super) fn toggle_show_desktop(&mut self) {
        let shown: Vec<App> = self.order[..self.order_len]
            .iter()
            .copied()
            .filter(|a| self.windows[a.index()].visible())
            .collect();
        if shown.is_empty() {
            let back = core::mem::take(&mut self.peeked);
            for app in back {
                let w = self.windows[app.index()];
                if w.open && w.minimized && !w.away {
                    self.open(app);
                }
            }
        } else {
            for &app in &shown {
                self.minimize(app);
            }
            self.peeked = shown;
        }
    }

    // ---- search ------------------------------------------------------------

    pub(super) fn open_search(&mut self) {
        if self.search.open {
            return self.close_search();
        }
        self.close_menu();
        self.close_panel();
        self.close_task_view(false);
        self.hide_tip();
        self.search.show();
        self.damage(self.search_panel().inset(-SPREAD));
        self.damage_taskbar();
    }

    pub(super) fn close_search(&mut self) {
        if self.search.open {
            self.search.open = false;
            self.search.query.clear();
            self.damage(self.search_panel().inset(-SPREAD));
            self.damage_taskbar();
        }
    }

    /// Apps shown before anything is typed.
    pub(super) fn top_apps(&self) -> Vec<App> {
        let mut out: Vec<App> = self.start.recent().collect();
        for app in self.pins.iter().copied().chain(APPS) {
            if !out.contains(&app) {
                out.push(app);
            }
        }
        out.truncate(6);
        out
    }

    pub(super) fn search_action(&mut self, action: search::Action) {
        match action {
            search::Action::None => return,
            search::Action::Redraw => {}
            search::Action::Close => self.close_search(),
            search::Action::Open(hit) => {
                self.close_search();
                match hit {
                    search::Hit::App(app) => self.open(app),
                    search::Hit::Path { path, dir: true } => self.show_folder(&path),
                    search::Hit::Path { path, .. } => self.open_file(&path),
                }
                serial::write_str("search: opened a result\n");
            }
            search::Action::Location(path) => {
                self.close_search();
                self.explorer.reveal(&path);
                self.open(App::Explorer);
                self.app_changed(App::Explorer);
            }
            search::Action::Pin(app) => self.pin(app),
            search::Action::Unpin(app) => self.unpin(app),
        }
        self.damage(self.search_panel());
        self.damage_taskbar();
    }

    // ---- pins --------------------------------------------------------------

    fn pins_file() -> Option<String> {
        let name = users::current_name()?;
        Some(fs::join(&fs::app_data(name.as_str()), "taskbar.txt"))
    }

    /// Read the signed-in user's pins, or start with the usual ones.
    pub(super) fn load_pins(&mut self) {
        self.pins = DEFAULT_PINS.to_vec();
        let Some(path) = Self::pins_file() else {
            return;
        };
        if let Ok(data) = fs::read(&path) {
            let text = String::from_utf8_lossy(&data);
            self.pins = text.lines().filter_map(|l| App::from_key(l.trim())).fold(
                Vec::new(),
                |mut v, a| {
                    if !v.contains(&a) {
                        v.push(a);
                    }
                    v
                },
            );
        }
    }

    fn save_pins(&self) {
        let Some(path) = Self::pins_file() else {
            return;
        };
        let mut text = String::new();
        for app in &self.pins {
            text.push_str(app.key());
            text.push('\n');
        }
        if fs::write(&path, text.as_bytes()).is_ok() {
            serial::write_str("taskbar: pins saved\n");
        }
    }

    pub(super) fn pin(&mut self, app: App) {
        if !self.pins.contains(&app) {
            // after the other pins, before apps that are only open
            self.pins.push(app);
            self.save_pins();
            self.damage_taskbar();
        }
    }

    pub(super) fn unpin(&mut self, app: App) {
        if let Some(i) = self.pins.iter().position(|&a| a == app) {
            self.pins.remove(i);
            self.save_pins();
            self.damage_taskbar();
        }
    }

    // ---- tooltips ----------------------------------------------------------

    /// What the mouse rests on moved: hide the tooltip and wait again.
    pub(super) fn hover_moved(&mut self, hover: Option<Hover>) {
        if hover == self.hover_now {
            return;
        }
        self.hover_now = hover;
        self.hover_since = interrupts::ticks();
        self.tip_checked = false;
        if let Some((r, _)) = self.tip.take() {
            self.damage(r.inset(-12));
        }
    }

    pub(super) fn hide_tip(&mut self) {
        if let Some((r, _)) = self.tip.take() {
            self.damage(r.inset(-12));
        }
    }

    /// Show the tooltip once the mouse has rested long enough.
    pub(super) fn tick_tip(&mut self) {
        if self.tip_checked || interrupts::ticks() - self.hover_since < TIP_DELAY {
            return;
        }
        self.tip_checked = true;
        let in_flyout = matches!(self.hover_now, Some(Hover::Hidden(_)));
        let busy = self.search.open || self.start.open || (self.panel.is_some() && !in_flyout);
        if self.left || self.right || self.popup.is_some() || busy {
            return;
        }
        let Some((text, anchor)) = self.hover_now.and_then(|h| self.tip_for(h)) else {
            return;
        };
        let w = UI.width(&text) + 20;
        let x = (anchor.x + anchor.w / 2 - w / 2).clamp(4, self.width - w - 4);
        let r = Rect::new(x, self.height - TASKBAR_H - 40, w, 30);
        self.damage(r.inset(-12));
        self.tip = Some((r, text));
    }

    fn tip_for(&self, hover: Hover) -> Option<(String, Rect)> {
        let (text, anchor): (String, Rect) = match hover {
            Hover::Task(item) => {
                let text = match item {
                    TaskItem::Start => String::from("Start"),
                    TaskItem::Search => String::from("Search"),
                    TaskItem::TaskView => String::from("Task View"),
                    TaskItem::App(a) if self.windows[a.index()].open => self.window_title(a),
                    TaskItem::App(a) => String::from(a.title()),
                };
                (text, self.task_rect(item)?)
            }
            Hover::Tray(i) => {
                let text = match i {
                    0 => String::from(match self.layout {
                        crate::keyboard::Layout::Us => "English (US), Alt+Shift to switch",
                        crate::keyboard::Layout::Ru => "Русский, Alt+Shift to switch",
                    }),
                    1 => {
                        let mut s = String::from(self.tray.net.label());
                        s.push_str(", volume ");
                        s.push_str(itoa(self.tray.volume).as_str());
                        s.push('%');
                        s
                    }
                    2 => String::from(self.date.as_str()),
                    _ => String::from("Show hidden icons"),
                };
                (text, self.tray_rect(i))
            }
            Hover::ShowDesktop => (String::from("Show desktop"), self.show_desktop_rect()),
            Hover::Hidden(i) => {
                let r = tray::hidden_icon_rect(self.panel_rect(Panel::Hidden), i as i32);
                // above the flyout, not the taskbar
                return Some((String::from(HIDDEN[i]), r.offset(0, -60)));
            }
            _ => return None,
        };
        Some((text, anchor))
    }

    pub(super) fn draw_tip(&self, c: &mut Canvas) {
        let Some((mut r, text)) = self.tip.as_ref().map(|(r, t)| (*r, t)) else {
            return;
        };
        if let Some(Hover::Hidden(i)) = self.hover_now {
            let p = self.panel_rect(Panel::Hidden);
            let a = tray::hidden_icon_rect(p, i as i32);
            r.x = (a.x + a.w / 2 - r.w / 2).clamp(4, self.width - r.w - 4);
            r.y = p.y - 38;
        }
        if !c.visible(r.inset(-12)) {
            return;
        }
        c.shadow(r, 5, 8, 2, 70);
        c.fill_round(r, 5, theme::menu());
        c.outline_round(r, 5, theme::frame());
        c.text_centered(r, text, theme::text());
    }

    // ---- drawing -----------------------------------------------------------

    pub(super) fn draw_taskbar(&self, c: &mut Canvas) {
        let top = self.height - TASKBAR_H;
        let bar = Rect::new(0, top, self.width, TASKBAR_H);
        if !c.visible(bar) {
            return;
        }
        // see-through, like acrylic
        c.fill_round_alpha(bar, 0, theme::taskbar(), 220);
        c.fill_rect(0, top, self.width, 1, theme::frame());

        for (item, r) in self.task_layout() {
            if !c.visible(r) {
                continue;
            }
            let active = match item {
                TaskItem::Start => self.start.open,
                TaskItem::Search => self.search.open,
                TaskItem::TaskView => self.tv.open,
                TaskItem::App(a) => self.focused == Some(a) && self.windows[a.index()].visible(),
            };
            let lit = self.hover.level(Hover::Task(item));
            if item == TaskItem::Search {
                self.draw_search_box(c, r, lit);
                continue;
            }
            if active {
                c.fill_round_alpha(r, 5, theme::glass(), 200);
                c.outline_round(r, 5, theme::stroke());
            } else if lit > 0 {
                c.fill_round_alpha(r, 5, theme::glass(), 140 * lit / ONE);
            }
            match item {
                TaskItem::Start => draw_start_logo(c, r.x + 10, r.y + 8),
                TaskItem::TaskView => task_view_icon(c, r.x + 11, r.y + 10),
                TaskItem::App(a) => {
                    self.icons.draw_medium(c, a, r.x + 10, r.y + 7);
                    let w = self.windows[a.index()];
                    if w.open && !w.away {
                        // a pill under open apps, longer for the active one
                        let (len, color) = if active {
                            (16, theme::accent())
                        } else {
                            (6, theme::thumb())
                        };
                        let pill = Rect::new(r.x + (r.w - len) / 2, r.bottom() - 4, len, 3);
                        c.fill_round(pill, 1, color);
                    }
                }
                TaskItem::Search => {}
            }
        }

        // the tray: ^, layout, network and volume, clock and date
        for i in 0..4 {
            let r = self.tray_rect(i);
            let open = match i {
                1 => self.panel == Some(Panel::Quick),
                2 => self.panel == Some(Panel::Calendar),
                3 => self.panel == Some(Panel::Hidden),
                _ => false,
            };
            let lit = if open {
                ONE
            } else {
                self.hover.level(Hover::Tray(i))
            };
            if lit > 0 {
                c.fill_round_alpha(r, 5, theme::glass(), 170 * lit / ONE);
            }
        }
        let chevron = self.tray_rect(3);
        let (cx, cy) = (chevron.x + chevron.w / 2, chevron.y + 19);
        let up = self.panel != Some(Panel::Hidden);
        for k in 0..2 {
            let (dy, ey) = if up { (3, -2) } else { (-2, 3) };
            c.line(cx - 5, cy + dy + k, cx, cy + ey + k, theme::text());
            c.line(cx, cy + ey + k, cx + 5, cy + dy + k, theme::text());
        }
        let layout = self.tray_rect(0);
        c.text_centered(layout, tray::layout_label(self.layout), theme::text());
        let quick = self.tray_rect(1);
        let bg = theme::taskbar();
        let y = quick.y + 12;
        tray::network_icon(c, quick.x + 14, y, self.tray.net, theme::text(), bg);
        tray::volume_icon(c, quick.x + 42, y, self.tray.volume, theme::text());
        let clock = self.tray_rect(2);
        let line = |i: i32| Rect::new(clock.x, clock.y + 2 + i * 18, clock.w, 18);
        c.text_centered(line(0), self.clock.as_str(), theme::text());
        c.text_centered(line(1), self.date.as_str(), theme::text());

        let sd = self.show_desktop_rect();
        c.fill_rect(sd.x, sd.y + 12, 1, sd.h - 24, theme::thumb());
        let lit = self.hover.level(Hover::ShowDesktop);
        if lit > 0 {
            c.fill_round_alpha(sd.offset(1, 0), 0, theme::glass(), 160 * lit / ONE);
        }
    }

    fn draw_search_box(&self, c: &mut Canvas, r: Rect, lit: i32) {
        let open = self.search.open;
        let face = if open {
            theme::control_lit()
        } else {
            super::canvas::mix(
                theme::control(),
                theme::control_lit(),
                (lit * 255 / ONE) as u32,
            )
        };
        c.fill_round(r, r.h / 2, face);
        c.outline_round(r, r.h / 2, theme::frame());
        if open {
            c.fill_rect(r.x + 16, r.bottom() - 2, r.w - 32, 2, theme::accent());
        }
        search::magnifier(c, r.x + 21, r.y + 15, 1, theme::text());
        let ty = r.y + (r.h - UI.line_height) / 2;
        let q = self.search.query.as_str();
        if q.is_empty() {
            c.draw_text(r.x + 40, ty, "Search", theme::text_dim());
            if open && self.cursor_on {
                c.fill_rect(r.x + 40, ty, 1, UI.line_height, theme::text());
            }
        } else {
            // the end of the text when it is too long
            let mut shown = q;
            while UI.width(shown) > r.w - 60 {
                let mut it = shown.chars();
                it.next();
                shown = it.as_str();
            }
            let w = c.draw_text(r.x + 40, ty, shown, theme::text());
            if open && self.cursor_on {
                c.fill_rect(r.x + 41 + w, ty, 1, UI.line_height, theme::text());
            }
        }
    }

    /// The ^ flyout: small icons that don't fit in the tray.
    pub(super) fn draw_hidden_icons(&self, c: &mut Canvas, p: Rect) {
        c.fill(p, theme::panel());
        for i in 0..tray::HIDDEN_ICONS {
            let r = tray::hidden_icon_rect(p, i);
            let lit = self.hover.level(Hover::Hidden(i as usize));
            if lit > 0 {
                c.fill_round_alpha(r.inset(2), 5, theme::glass(), 230 * lit / ONE);
                c.outline_round_alpha(r.inset(2), 5, theme::stroke(), lit);
            }
            let (x, y) = (r.x + 8, r.y + 8);
            match i {
                0 => self.icons.draw_pic(c, Pic::Drives, MEDIUM, x, y),
                1 => self.icons.draw(c, App::Settings, MEDIUM, x, y),
                _ => self.icons.draw(c, App::About, MEDIUM, x, y),
            }
        }
    }

    pub(super) fn hidden_click(&mut self, i: usize) {
        self.close_panel();
        match i {
            0 => self.show_folder("/"),
            1 => self.open(App::Settings),
            _ => self.open(App::About),
        }
    }
}

/// Two overlapping windows, for the Task View button.
fn task_view_icon(c: &mut Canvas, x: i32, y: i32) {
    c.outline_round(Rect::new(x, y, 14, 14), 2, theme::text());
    c.fill_round(Rect::new(x + 7, y + 6, 15, 14), 2, theme::taskbar());
    c.fill_round(Rect::new(x + 8, y + 7, 14, 13), 2, theme::text());
    c.fill_round(Rect::new(x + 10, y + 9, 10, 9), 1, theme::taskbar());
}

fn itoa(n: i32) -> crate::StackString<12> {
    use core::fmt::Write;
    let mut s = crate::StackString::new();
    let _ = write!(s, "{}", n);
    s
}
