//! The RyzikOS dock and menu bar.
//!
//! The dock floats over the bottom of the screen: the launcher (the
//! RyzikOS logo), the apps, and after a divider Search and Task View.
//! Pinned apps are always there; other apps join while they are open on
//! the current desktop. A dot under an icon shows the app is open, a
//! longer one in the accent colour that it has the keyboard. Icons rise
//! a little under the mouse. Right-click pins and unpins; the pins are
//! kept in the user's AppData folder on the disk.
//!
//! The menu bar runs along the top: the logo, which opens the system
//! menu, and the name of the app in front at the left; the ^ button for
//! hidden icons, the layout, network and volume, and the date and clock
//! at the right. Their flyouts open downwards. Resting the mouse on a
//! button shows what it is.

use alloc::string::String;
use alloc::vec::Vec;

use super::anim::ONE;
use super::canvas::{Canvas, Rect};
use super::icons::{Pic, LARGE, MEDIUM};
use super::popup::{Builder, Cmd};
use super::search::{self, Search};
use super::text::{UI, UI_BOLD};
use super::theme;
use super::tray::{self, Panel};
use super::{App, Desktop, Hover, APPS, MENUBAR_H, SPREAD, TASKBAR_H};
use crate::{fs, interrupts, serial, users};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskItem {
    Start,
    Search,
    TaskView,
    App(App),
}

/// A square in the dock holding one 48 pixel icon.
const SLOT: i32 = 56;
const GAP: i32 = 4;
/// Room for the divider before Search and Task View.
const DIVIDER: i32 = 17;
const DOCK_H: i32 = 68;
/// Space under the dock, above the bottom of the screen.
const DOCK_MARGIN: i32 = 8;
const DOCK_PAD: i32 = 8;
const DOCK_RADIUS: i32 = 20;
/// How far an icon rises under the mouse.
const LIFT: i32 = 4;
/// Buttons in the menu bar: the layout, quick settings, the clock, ^
/// and the logo.
pub const TRAY_BUTTONS: usize = 5;
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
const HIDDEN: [&str; 3] = ["System Disk", "Settings", "About RyzikOS"];

impl Desktop<'_> {
    // ---- buttons -----------------------------------------------------------

    /// Everything in the dock, from the left.
    pub(super) fn task_items(&self) -> Vec<TaskItem> {
        let mut out = alloc::vec![TaskItem::Start];
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
        out.extend([TaskItem::Search, TaskItem::TaskView]);
        out
    }

    /// The dock's glass, around its icons.
    pub(super) fn dock_rect(&self) -> Rect {
        let n = self.task_items().len() as i32;
        let w = n * SLOT + (n - 1) * GAP + DIVIDER + 2 * DOCK_PAD;
        let y = self.height - DOCK_MARGIN - DOCK_H;
        Rect::new((self.width - w) / 2, y, w, DOCK_H)
    }

    /// The icons and where they are, centred in the dock.
    pub(super) fn task_layout(&self) -> Vec<(TaskItem, Rect)> {
        let dock = self.dock_rect();
        let mut x = dock.x + DOCK_PAD;
        let y = dock.y + 4;
        self.task_items()
            .into_iter()
            .map(|item| {
                if item == TaskItem::Search {
                    x += DIVIDER;
                }
                let r = Rect::new(x, y, SLOT, SLOT);
                x += SLOT + GAP;
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

    /// Menu bar button `i`: the layout, quick settings, the clock, the
    /// ^ for hidden icons (left of the layout) and the logo at the left.
    /// They reach the top edge of the screen, so the mouse can't miss.
    pub(super) fn tray_rect(&self, i: usize) -> Rect {
        let (y, h) = (0, MENUBAR_H);
        let clock = Rect::new(self.width - 8 - 156, y, 156, h);
        let quick = Rect::new(clock.x - 4 - 60, y, 60, h);
        let layout = Rect::new(quick.x - 4 - 46, y, 46, h);
        match i {
            0 => layout,
            1 => quick,
            2 => clock,
            3 => Rect::new(layout.x - 4 - 30, y, 30, h),
            _ => Rect::new(0, y, 50, h),
        }
    }

    /// "Show desktop" has no button in RyzikOS: Win+D does it.
    pub(super) fn show_desktop_rect(&self) -> Rect {
        Rect::default()
    }

    pub(super) fn search_panel(&self) -> Rect {
        Search::panel(self.width, self.height, TASKBAR_H, MENUBAR_H)
    }

    // ---- clicks ------------------------------------------------------------

    pub(super) fn taskbar_press(&mut self, x: i32, y: i32, right: bool) {
        if right {
            return self.taskbar_menu(x, y);
        }
        if self.show_desktop_rect().contains(x, y) {
            return self.toggle_show_desktop();
        }
        if y < MENUBAR_H {
            match (0..TRAY_BUTTONS).find(|&i| self.tray_rect(i).contains(x, y)) {
                Some(0) => self.toggle_layout = true,
                Some(1) => self.open_panel(Panel::Quick),
                Some(2) => self.open_panel(Panel::Calendar),
                Some(3) => self.open_panel(Panel::Hidden),
                Some(_) => {
                    let r = self.tray_rect(4);
                    let menu = self.system_menu(r.x, r.bottom() + 4);
                    self.show_popup(menu);
                }
                None => {}
            }
            return;
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
        if y < MENUBAR_H {
            let menu = self.system_menu(x, MENUBAR_H + 4);
            return self.show_popup(menu);
        }
        let top = self.dock_rect().y - 8;
        let menu = match self.task_at(x, y) {
            Some(TaskItem::App(app)) => {
                let r = self.task_rect(TaskItem::App(app)).unwrap_or_default();
                let mut b = Builder::default().item(app.title(), Cmd::Open(app)).sep();
                b = if self.pins.contains(&app) {
                    b.item("Remove from Dock", Cmd::Unpin(app))
                } else {
                    b.item("Keep in Dock", Cmd::Pin(app))
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
                .item("Overview", Cmd::TaskView)
                .item("Show desktop", Cmd::ShowDesktop)
                .sep()
                .item("Dock settings", Cmd::Open(App::Settings))
                .at(x - 20, top, true, self.screen()),
        };
        self.show_popup(menu);
    }

    /// The menu under the logo in the menu bar.
    fn system_menu(&self, x: i32, y: i32) -> super::popup::Popup {
        Builder::default()
            .item("About RyzikOS", Cmd::Open(App::About))
            .item("Settings", Cmd::Open(App::Settings))
            .sep()
            .keyed("Search", "Super+S", Cmd::Search)
            .keyed("Overview", "Super+Tab", Cmd::TaskView)
            .keyed("Show desktop", "Super+D", Cmd::ShowDesktop)
            .sep()
            .keyed("Lock", "Super+L", Cmd::Lock)
            .item("Sign out", Cmd::SignOut)
            .item("Restart", Cmd::Restart)
            .item("Shut down", Cmd::ShutDown)
            .at(x, y, false, self.screen())
    }

    /// The menu of the launcher's right-click and Win+X.
    pub(super) fn start_menu_popup(&self, x: i32, y: i32) -> super::popup::Popup {
        Builder::default()
            .item("Terminal", Cmd::Open(App::Terminal))
            .item("Files", Cmd::Open(App::Explorer))
            .item("Settings", Cmd::Open(App::Settings))
            .keyed("Search", "Super+S", Cmd::Search)
            .keyed("Overview", "Super+Tab", Cmd::TaskView)
            .sep()
            .keyed("Lock", "Super+L", Cmd::Lock)
            .item("Sign out", Cmd::SignOut)
            .item("Restart", Cmd::Restart)
            .item("Shut down", Cmd::ShutDown)
            .sep()
            .keyed("Desktop", "Super+D", Cmd::ShowDesktop)
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
        // under the menu bar, or over the dock
        let y = if anchor.y < MENUBAR_H {
            MENUBAR_H + 6
        } else {
            self.dock_rect().y - 38
        };
        let r = Rect::new(x, y, w, 30);
        self.damage(r.inset(-12));
        self.tip = Some((r, text));
    }

    fn tip_for(&self, hover: Hover) -> Option<(String, Rect)> {
        let (text, anchor): (String, Rect) = match hover {
            Hover::Task(item) => {
                let text = match item {
                    TaskItem::Start => String::from("Launcher"),
                    TaskItem::Search => String::from("Search"),
                    TaskItem::TaskView => String::from("Overview"),
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
                    3 => String::from("Show hidden icons"),
                    _ => String::from("RyzikOS"),
                };
                (text, self.tray_rect(i))
            }
            Hover::ShowDesktop => (String::from("Show desktop"), self.show_desktop_rect()),
            Hover::Hidden(i) => {
                let r = tray::hidden_icon_rect(self.panel_rect(Panel::Hidden), i as i32);
                // under the flyout, not the menu bar
                return Some((String::from(HIDDEN[i]), r.offset(0, 60)));
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
            r.y = p.bottom() + 8;
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
        self.draw_menu_bar(c);
        self.draw_dock(c);
    }

    fn draw_dock(&self, c: &mut Canvas) {
        let dock = self.dock_rect();
        if !c.visible(dock.inset(-SPREAD)) {
            return;
        }
        // frosted glass floating over the wallpaper
        c.shadow(dock, DOCK_RADIUS, 12, 3, 70);
        c.fill_round_alpha(dock, DOCK_RADIUS, theme::taskbar(), 200);
        c.outline_round_alpha(dock, DOCK_RADIUS, theme::frame(), ONE * 3 / 4);
        let edge = Rect::new(dock.x + 1, dock.y + 1, dock.w - 2, dock.h - 2);
        c.outline_round_alpha(edge, DOCK_RADIUS - 1, theme::glass(), ONE / 3);

        let layout = self.task_layout();
        if let Some((_, r)) = layout.iter().find(|(i, _)| *i == TaskItem::Search) {
            // the divider before Search and Task View
            let x = r.x - GAP / 2 - DIVIDER / 2;
            c.fill_rect(x, dock.y + 14, 1, dock.h - 28, theme::thumb());
        }
        for (item, r) in layout {
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
            let lift = if active { LIFT / 2 } else { 0 }.max(LIFT * lit / ONE);
            let (x, y) = (r.x + (SLOT - LARGE as i32) / 2, r.y + 1 - lift);
            match item {
                TaskItem::Start => self.icons.draw_logo(c, LARGE, x, y),
                TaskItem::Search | TaskItem::TaskView => {
                    let tile = Rect::new(x + 2, y + 2, LARGE as i32 - 4, LARGE as i32 - 4);
                    let face = if active {
                        theme::accent()
                    } else {
                        theme::control_lit()
                    };
                    c.fill_round(tile, 12, face);
                    c.outline_round(tile, 12, theme::stroke());
                    let ink = if active {
                        theme::on_accent()
                    } else {
                        theme::text()
                    };
                    let (cx, cy) = (tile.x + tile.w / 2, tile.y + tile.h / 2);
                    if item == TaskItem::Search {
                        search::magnifier(c, cx - 3, cy - 3, 2, ink);
                    } else {
                        task_view_icon(c, cx - 11, cy - 10, ink, face);
                    }
                }
                TaskItem::App(a) => self.icons.draw(c, a, LARGE, x, y),
            }
            let open = match item {
                TaskItem::App(a) => {
                    let w = self.windows[a.index()];
                    w.open && !w.away
                }
                _ => active,
            };
            if open {
                // a dot under open apps, a longer bar for the one in front
                let (len, color) = if active {
                    (14, theme::accent())
                } else {
                    (5, theme::text_dim())
                };
                let dot = Rect::new(r.x + (r.w - len) / 2, r.y + LARGE as i32 + 5, len, 4);
                c.fill_round(dot, 2, color);
            }
        }
    }

    fn draw_menu_bar(&self, c: &mut Canvas) {
        let bar = Rect::new(0, 0, self.width, MENUBAR_H);
        if !c.visible(bar) {
            return;
        }
        c.fill_round_alpha(bar, 0, theme::taskbar(), 210);
        c.fill_rect(0, MENUBAR_H - 1, self.width, 1, theme::frame());

        for i in 0..TRAY_BUTTONS {
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
                let face = Rect::new(r.x, r.y + 3, r.w, r.h - 6);
                c.fill_round_alpha(face, 6, theme::glass(), 190 * lit / ONE);
            }
        }

        // the logo and the name of the app in front
        let logo = self.tray_rect(4);
        self.icons
            .draw_logo(c, 20, logo.x + 16, logo.y + (logo.h - 20) / 2);
        let name = match self.focused {
            Some(app) if self.windows[app.index()].visible() => app.title(),
            _ => "RyzikOS",
        };
        let ty = (MENUBAR_H - UI.line_height) / 2;
        c.draw_text_in(&UI_BOLD, logo.right() + 4, ty, name, theme::text());

        let chevron = self.tray_rect(3);
        let (cx, cy) = (chevron.x + chevron.w / 2, chevron.y + chevron.h / 2);
        let down = self.panel != Some(Panel::Hidden);
        for k in 0..2 {
            let (dy, ey) = if down { (-2, 3) } else { (3, -2) };
            c.line(cx - 5, cy + dy + k, cx, cy + ey + k, theme::text());
            c.line(cx, cy + ey + k, cx + 5, cy + dy + k, theme::text());
        }
        let layout = self.tray_rect(0);
        c.text_centered(layout, tray::layout_label(self.layout), theme::text());
        let quick = self.tray_rect(1);
        let bg = theme::taskbar();
        let y = quick.y + (quick.h - 16) / 2;
        tray::network_icon(c, quick.x + 10, y, self.tray.net, theme::text(), bg);
        tray::volume_icon(c, quick.x + 36, y, self.tray.volume, theme::text());
        let clock = self.tray_rect(2);
        let date_w = UI.width(self.date.as_str());
        let time_w = UI_BOLD.width(self.clock.as_str());
        let x = clock.x + (clock.w - date_w - 10 - time_w) / 2;
        c.draw_text(x, ty, self.date.as_str(), theme::text_dim());
        let x = x + date_w + 10;
        c.draw_text_in(&UI_BOLD, x, ty, self.clock.as_str(), theme::text());
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
fn task_view_icon(c: &mut Canvas, x: i32, y: i32, ink: u32, bg: u32) {
    c.outline_round(Rect::new(x, y, 14, 14), 2, ink);
    c.fill_round(Rect::new(x + 7, y + 6, 15, 14), 2, bg);
    c.fill_round(Rect::new(x + 8, y + 7, 14, 13), 2, ink);
    c.fill_round(Rect::new(x + 10, y + 9, 10, 9), 1, bg);
}

fn itoa(n: i32) -> crate::StackString<12> {
    use core::fmt::Write;
    let mut s = crate::StackString::new();
    let _ = write!(s, "{}", n);
    s
}
