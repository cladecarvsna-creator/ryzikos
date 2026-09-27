//! The icons on the desktop, like Windows 11: This PC, the Recycle Bin,
//! app shortcuts, then the files and folders in
//! the user's Desktop folder, in columns from the top left.
//!
//! Click selects, Ctrl+click adds, and dragging on the empty desktop
//! draws a see-through blue rectangle that selects what it touches.
//! Selected icons can be dragged onto the Recycle Bin or into a folder.
//! Delete moves files to the Recycle Bin, F2 renames, Enter opens, and
//! right-click shows what can be done.

use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::icons::{Pic, LARGE};
use super::popup::{Builder, Cmd};
use super::text::UI;
use super::theme;
use super::widgets::{self, FieldEvent, TextField};
use super::{App, Desktop, DOUBLE_CLICK_TICKS, TASKBAR_H};
use crate::fs::{self, recycle};
use crate::keyboard::{self, Key};
use crate::{interrupts, serial, users};

const CELL_W: i32 = 92;
const CELL_H: i32 = 102;
const LEFT: i32 = 6;
const TOP: i32 = super::MENUBAR_H + 6;
/// The apps with a shortcut on the desktop.
const SHORTCUTS: [App; 8] = [
    App::Browser,
    App::Photos,
    App::Video,
    App::Terminal,
    App::Notepad,
    App::Paint,
    App::Calculator,
    App::Settings,
];

#[derive(Clone, PartialEq, Eq)]
pub enum DeskItem {
    ThisPc,
    Bin,
    App(App),
    Entry { name: String, dir: bool },
}

impl DeskItem {
    fn label(&self) -> &str {
        match self {
            DeskItem::ThisPc => "Computer",
            DeskItem::Bin => "Trash",
            DeskItem::App(a) => a.title(),
            DeskItem::Entry { name, .. } => name,
        }
    }
}

/// The blue rectangle being dragged out, and what was selected before.
struct Band {
    from: (i32, i32),
    rect: Rect,
    before: Vec<bool>,
}

/// Icons being dragged: how far they moved and what they are over.
struct Drag {
    offset: (i32, i32),
    target: Option<usize>,
}

pub struct DeskIcons {
    pub items: Vec<DeskItem>,
    pub selected: Vec<bool>,
    pub hover: Option<usize>,
    /// `fs::changes()` when the Desktop folder was read.
    seen: Option<u32>,
    bin_full: bool,
    band: Option<Band>,
    /// An icon pressed, and where, until it is dragged or let go.
    pressed: Option<(usize, i32, i32)>,
    drag: Option<Drag>,
    last_click: (u64, usize),
    pub renaming: Option<(usize, TextField)>,
}

impl DeskIcons {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: Vec::new(),
            hover: None,
            seen: None,
            bin_full: false,
            band: None,
            pressed: None,
            drag: None,
            last_click: (0, usize::MAX),
            renaming: None,
        }
    }

    /// Whether the mouse is busy with icons (a rectangle or a drag).
    pub fn busy(&self) -> bool {
        self.band.is_some() || self.pressed.is_some() || self.drag.is_some()
    }

    /// Read everything again next time.
    pub fn forget(&mut self) {
        self.seen = None;
    }
}

/// The signed-in user's Desktop folder.
fn desktop_dir() -> Option<String> {
    let name = users::current_name()?;
    Some(fs::join(&fs::home(name.as_str()), "Desktop"))
}

fn user() -> String {
    String::from(users::current_name().unwrap_or_default().as_str())
}

impl Desktop<'_> {
    fn icon_rows(&self) -> i32 {
        ((self.height - TASKBAR_H - TOP) / CELL_H).max(1)
    }

    /// Icon `i`'s cell, in columns from the top left.
    pub(super) fn icon_rect(&self, i: usize) -> Rect {
        let rows = self.icon_rows();
        let (col, row) = (i as i32 / rows, i as i32 % rows);
        Rect::new(
            LEFT + col * CELL_W,
            TOP + row * CELL_H,
            CELL_W - 4,
            CELL_H - 6,
        )
    }

    fn icon_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.desk_icons.items.len()).find(|&i| self.icon_rect(i).contains(x, y))
    }

    /// Everything the icons cover.
    fn icons_area(&self) -> Rect {
        let n = self.desk_icons.items.len().max(1);
        let cols = (n as i32 - 1) / self.icon_rows() + 1;
        Rect::new(0, 0, LEFT + cols * CELL_W + 8, self.height - TASKBAR_H)
    }

    pub(super) fn damage_icons(&mut self) {
        self.damage(self.icons_area());
    }

    /// Read the Desktop folder again if files changed.
    pub(super) fn refresh_icons(&mut self) {
        let changes = fs::changes();
        if self.desk_icons.seen == Some(changes) || self.desk_icons.busy() {
            return;
        }
        self.desk_icons.seen = Some(changes);
        let old_area = self.icons_area();
        let mut items = alloc::vec![DeskItem::ThisPc, DeskItem::Bin];
        items.extend(SHORTCUTS.into_iter().map(DeskItem::App));
        if let Some(dir) = desktop_dir() {
            for info in fs::list(&dir).unwrap_or_default() {
                items.push(DeskItem::Entry {
                    name: info.name,
                    dir: info.dir,
                });
            }
        }
        // keep the selection on the same things
        let icons = &mut self.desk_icons;
        let selected = items
            .iter()
            .map(|it| {
                icons
                    .items
                    .iter()
                    .zip(&icons.selected)
                    .any(|(old, &s)| s && old == it)
            })
            .collect();
        icons.items = items;
        icons.selected = selected;
        icons.hover = None;
        icons.renaming = None;
        icons.bin_full = !recycle::is_empty(&user());
        self.damage(old_area);
        self.damage_icons();
    }

    fn select_only(&mut self, i: Option<usize>) {
        for (k, s) in self.desk_icons.selected.iter_mut().enumerate() {
            *s = Some(k) == i;
        }
    }

    fn selected_icons(&self) -> Vec<usize> {
        (0..self.desk_icons.items.len())
            .filter(|&i| self.desk_icons.selected[i])
            .collect()
    }

    /// The path of a file or folder icon.
    pub(super) fn icon_path(&self, i: usize) -> Option<String> {
        match &self.desk_icons.items[i] {
            DeskItem::Entry { name, .. } => Some(fs::join(&desktop_dir()?, name)),
            _ => None,
        }
    }

    // ---- mouse -------------------------------------------------------------

    pub(super) fn icons_press(&mut self, x: i32, y: i32, right: bool) {
        self.commit_icon_rename();
        let hit = self.icon_at(x, y);
        let ctrl = keyboard::ctrl_held();
        if right {
            if let Some(i) = hit {
                if !self.desk_icons.selected[i] {
                    self.select_only(Some(i));
                }
            } else {
                self.select_only(None);
            }
            self.damage_icons();
            let menu = self.icons_menu(hit, x, y);
            self.show_popup(menu);
            return;
        }
        match hit {
            Some(i) => {
                if ctrl {
                    self.desk_icons.selected[i] = !self.desk_icons.selected[i];
                } else if !self.desk_icons.selected[i] {
                    self.select_only(Some(i));
                }
                let now = interrupts::ticks();
                let (at, last) = self.desk_icons.last_click;
                if last == i && now - at <= DOUBLE_CLICK_TICKS && !ctrl {
                    self.desk_icons.last_click = (0, usize::MAX);
                    self.select_only(Some(i));
                    self.damage_icons();
                    self.open_icon(i);
                    return;
                }
                self.desk_icons.last_click = (now, i);
                self.desk_icons.pressed = Some((i, x, y));
            }
            None => {
                if !ctrl {
                    self.select_only(None);
                }
                self.desk_icons.band = Some(Band {
                    from: (x, y),
                    rect: Rect::new(x, y, 0, 0),
                    before: self.desk_icons.selected.clone(),
                });
            }
        }
        self.damage_icons();
    }

    pub(super) fn icons_move(&mut self, x: i32, y: i32) {
        if let Some(band) = &mut self.desk_icons.band {
            let (x0, y0) = band.from;
            let bottom = self.height - TASKBAR_H;
            let (x, y) = (x.clamp(0, self.width - 1), y.clamp(0, bottom - 1));
            let r = Rect::new(x0.min(x), y0.min(y), (x - x0).abs() + 1, (y - y0).abs() + 1);
            let old = core::mem::replace(&mut band.rect, r);
            let before = band.before.clone();
            for (i, was) in before.into_iter().enumerate() {
                let touched = !self.icon_rect(i).intersect(&r).is_empty();
                self.desk_icons.selected[i] = was || touched;
            }
            self.damage(old.union(&r).inset(-2));
            self.damage_icons();
            return;
        }
        if let Some((i, px, py)) = self.desk_icons.pressed {
            if (x - px).abs() + (y - py).abs() < 5 && self.desk_icons.drag.is_none() {
                return;
            }
            // only files and folders move; the rest just stays selected
            let movable = self
                .selected_icons()
                .iter()
                .any(|&k| self.icon_path(k).is_some());
            if !movable {
                return;
            }
            let target = self.icon_at(x, y).filter(|&t| {
                !self.desk_icons.selected[t]
                    && match &self.desk_icons.items[t] {
                        DeskItem::Bin => true,
                        DeskItem::Entry { dir, .. } => *dir,
                        _ => false,
                    }
            });
            let _ = i;
            self.desk_icons.drag = Some(Drag {
                offset: (x - px, y - py),
                target,
            });
            self.damage(self.screen());
        }
    }

    pub(super) fn icons_release(&mut self) {
        if let Some(band) = self.desk_icons.band.take() {
            self.damage(band.rect.inset(-2));
            return;
        }
        self.desk_icons.pressed = None;
        let Some(drag) = self.desk_icons.drag.take() else {
            return;
        };
        self.damage(self.screen());
        let Some(t) = drag.target else {
            return;
        };
        let paths: Vec<String> = self
            .selected_icons()
            .into_iter()
            .filter_map(|i| self.icon_path(i))
            .collect();
        let into = match &self.desk_icons.items[t] {
            DeskItem::Bin => None,
            _ => self.icon_path(t),
        };
        for path in paths {
            let result = match &into {
                None => recycle::recycle(&user(), &path),
                Some(dir) => fs::move_path(&path, &fs::join(dir, fs::file_name(&path))),
            };
            if result.is_err() {
                serial::write_str("desktop: could not move an icon\n");
            }
        }
        self.refresh_icons();
    }

    /// The mouse moved with no button held.
    pub(super) fn icons_hover(&mut self, x: i32, y: i32, over_desktop: bool) {
        let hover = if over_desktop {
            self.icon_at(x, y)
        } else {
            None
        };
        if hover != self.desk_icons.hover {
            for i in [hover, self.desk_icons.hover].into_iter().flatten() {
                self.damage(self.icon_rect(i));
            }
            self.desk_icons.hover = hover;
        }
    }

    // ---- actions -----------------------------------------------------------

    pub(super) fn open_icon(&mut self, i: usize) {
        match self.desk_icons.items[i].clone() {
            DeskItem::ThisPc => self.show_folder("/"),
            DeskItem::Bin => {
                let bin = super::explorer::bin_folder();
                let _ = fs::create_dir(recycle::ROOT);
                let _ = fs::create_dir(&bin);
                self.show_folder(&bin);
            }
            DeskItem::App(a) => self.open(a),
            DeskItem::Entry { dir, .. } => {
                if let Some(path) = self.icon_path(i) {
                    if dir {
                        self.show_folder(&path);
                    } else {
                        self.open_file(&path);
                    }
                }
            }
        }
    }

    /// Move the selected files and folders to the Recycle Bin.
    pub(super) fn delete_icons(&mut self) {
        let paths: Vec<String> = self
            .selected_icons()
            .into_iter()
            .filter_map(|i| self.icon_path(i))
            .collect();
        for path in paths {
            if recycle::recycle(&user(), &path).is_ok() {
                serial::write_str("desktop: moved to the Recycle Bin\n");
            }
        }
        self.refresh_icons();
    }

    pub(super) fn new_on_desktop(&mut self, folder: bool) {
        let Some(dir) = desktop_dir() else {
            return;
        };
        let name = if folder {
            fs::unique_name(&dir, "New folder", "")
        } else {
            fs::unique_name(&dir, "New Text Document", ".txt")
        };
        let path = fs::join(&dir, &name);
        let ok = if folder {
            fs::create_dir(&path)
        } else {
            fs::write(&path, &[])
        };
        if ok.is_err() {
            return;
        }
        self.refresh_icons();
        let found = self
            .desk_icons
            .items
            .iter()
            .position(|it| matches!(it, DeskItem::Entry { name: n, .. } if *n == name));
        if let Some(i) = found {
            self.select_only(Some(i));
            self.rename_icon(i);
        }
    }

    pub(super) fn rename_icon(&mut self, i: usize) {
        let DeskItem::Entry { name, dir } = &self.desk_icons.items[i] else {
            return;
        };
        let mut field = TextField::new(name);
        let base = match name.rfind('.') {
            Some(p) if p > 0 && !dir => name[..p].chars().count(),
            _ => name.chars().count(),
        };
        field.select(0, base);
        self.desk_icons.renaming = Some((i, field));
        self.focused_away();
        self.damage(self.icon_rect(i).inset(-8));
    }

    fn commit_icon_rename(&mut self) {
        let Some((i, field)) = self.desk_icons.renaming.take() else {
            return;
        };
        self.damage(self.icon_rect(i).inset(-8));
        let new = field.string();
        let new = new.trim().trim_end_matches('.');
        if let (Some(path), false) = (self.icon_path(i), new.is_empty()) {
            if fs::file_name(&path) != new {
                let _ = fs::rename(&path, new);
            }
        }
        self.refresh_icons();
    }

    pub(super) fn empty_bin(&mut self) {
        let _ = recycle::empty(&user());
        serial::write_str("desktop: emptied the Recycle Bin\n");
        self.refresh_icons();
    }

    /// Keys while the desktop, not a window, has the keyboard.
    pub(super) fn icons_key(&mut self, key: Key) {
        if let Some((i, field)) = &mut self.desk_icons.renaming {
            let i = *i;
            match field.on_key(key) {
                FieldEvent::Enter => self.commit_icon_rename(),
                FieldEvent::Escape => {
                    self.desk_icons.renaming = None;
                }
                _ => {}
            }
            self.damage(self.icon_rect(i).inset(-8));
            return;
        }
        let first = self.selected_icons().first().copied();
        let n = self.desk_icons.items.len();
        match key {
            Key::Enter => {
                if let Some(i) = first {
                    self.open_icon(i);
                }
            }
            Key::Delete => self.delete_icons(),
            Key::Function(2) => {
                if let Some(i) = first {
                    self.rename_icon(i);
                }
            }
            Key::Function(5) => {
                self.desk_icons.forget();
                self.refresh_icons();
            }
            Key::Ctrl('a') => self.desk_icons.selected.iter_mut().for_each(|s| *s = true),
            Key::Escape => self.select_only(None),
            Key::Down | Key::Right | Key::Up | Key::Left if n > 0 => {
                let rows = self.icon_rows() as usize;
                let i = first.unwrap_or(0);
                let next = match key {
                    Key::Down => i + 1,
                    Key::Up => i.saturating_sub(1),
                    Key::Right => i + rows,
                    _ => i.saturating_sub(rows),
                };
                self.select_only(Some(if first.is_none() { 0 } else { next.min(n - 1) }));
            }
            _ => return,
        }
        self.damage_icons();
    }

    fn icons_menu(&self, hit: Option<usize>, x: i32, y: i32) -> super::popup::Popup {
        let screen = self.screen();
        let Some(i) = hit else {
            return Builder::default()
                .keyed("Refresh", "F5", Cmd::Refresh)
                .sep()
                .item("New folder", Cmd::NewFolder)
                .item("New text document", Cmd::NewFile)
                .sep()
                .item("Overview", Cmd::TaskView)
                .item("New desktop", Cmd::NewDesktop)
                .sep()
                .item("Next desktop background", Cmd::NextBackground)
                .item("Display settings", Cmd::DisplaySettings)
                .item("Personalize", Cmd::Personalize)
                .at(x, y, false, screen);
        };
        let several = self.selected_icons().len() > 1;
        let b = Builder::default().keyed("Open", "Enter", Cmd::OpenIcon(i));
        match &self.desk_icons.items[i] {
            DeskItem::ThisPc => b.sep().item("Properties", Cmd::Open(App::About)),
            DeskItem::Bin => {
                b.sep()
                    .maybe("Empty Trash", Cmd::EmptyBin, self.desk_icons.bin_full)
            }
            DeskItem::App(a) => {
                let b = b.sep();
                if self.pins.contains(a) {
                    b.item("Remove from Dock", Cmd::Unpin(*a))
                } else {
                    b.item("Keep in Dock", Cmd::Pin(*a))
                }
            }
            DeskItem::Entry { name, dir } => {
                let b = if !dir && super::picture::is_picture(name) && !several {
                    b.item("Set as desktop background", Cmd::SetBackground(i))
                } else {
                    b
                };
                b.sep().maybe("Rename", Cmd::RenameIcon(i), !several).keyed(
                    "Delete",
                    "Del",
                    Cmd::DeleteIcons,
                )
            }
        }
        .at(x, y, false, screen)
    }

    // ---- drawing -----------------------------------------------------------

    pub(super) fn draw_desk_icons(&self, c: &mut Canvas) {
        let icons = &self.desk_icons;
        let dragging = icons.drag.as_ref();
        for (i, item) in icons.items.iter().enumerate() {
            let r = self.icon_rect(i);
            if !c.visible(r.inset(-8)) {
                continue;
            }
            let target = dragging.is_some_and(|d| d.target == Some(i));
            if icons.selected[i] || target {
                let a = theme::accent_base();
                c.fill_round_alpha(r, 4, mix(a, 0xffffff, 150), 96);
                c.outline_round_alpha(r, 4, mix(a, 0xffffff, 190), 180);
            } else if icons.hover == Some(i) {
                c.fill_round_alpha(r, 4, mix(theme::accent_base(), 0xffffff, 180), 56);
            }
            self.draw_desk_icon(c, item, r.x + (r.w - 48) / 2, r.y + 6);
            if icons.renaming.as_ref().is_some_and(|(k, _)| *k == i) {
                continue;
            }
            let lines = wrap(item.label(), r.w - 4);
            for (k, line) in lines.iter().enumerate() {
                let lr = Rect::new(r.x, r.y + 58 + k as i32 * 17, r.w, 17);
                c.text_centered(lr.offset(1, 1), line, rgb(0x08, 0x0c, 0x18));
                c.text_centered(lr, line, 0xffffff);
            }
        }
        if let Some(band) = &icons.band {
            let r = band.rect;
            c.fill_round_alpha(r, 0, theme::accent_base(), 104);
            let edge = mix(theme::accent_base(), 0xffffff, 80);
            c.fill_rect(r.x, r.y, r.w, 1, edge);
            c.fill_rect(r.x, r.bottom() - 1, r.w, 1, edge);
            c.fill_rect(r.x, r.y, 1, r.h, edge);
            c.fill_rect(r.right() - 1, r.y, 1, r.h, edge);
        }
    }

    /// The rename box, drawn above windows' shadows but under windows.
    pub(super) fn draw_icon_rename(&self, c: &mut Canvas) {
        if let Some((i, field)) = &self.desk_icons.renaming {
            let r = self.icon_rect(*i);
            let box_ = Rect::new(r.x - 30, r.y + 56, r.w + 60, 26);
            let mut field = field.clone();
            field.draw(c, box_, true, self.cursor_on);
        }
    }

    /// Icons being dragged follow the pointer, over everything.
    pub(super) fn draw_icon_drag(&self, c: &mut Canvas) {
        let Some(drag) = &self.desk_icons.drag else {
            return;
        };
        for (n, i) in self.selected_icons().into_iter().take(12).enumerate() {
            let r = self.icon_rect(i).offset(drag.offset.0, drag.offset.1);
            let _ = n;
            self.draw_desk_icon(c, &self.desk_icons.items[i], r.x + (r.w - 48) / 2, r.y + 6);
        }
        if let Some(t) = drag.target {
            let verb = match &self.desk_icons.items[t] {
                DeskItem::Bin => String::from("Move to Trash"),
                it => {
                    let mut s = String::from("Move to ");
                    s.push_str(it.label());
                    s
                }
            };
            let w = UI.width(&verb) + 20;
            let r = Rect::new(self.mouse_x + 16, self.mouse_y + 24, w, 26);
            c.fill_round(r, 4, theme::menu());
            c.outline_round(r, 4, theme::stroke());
            c.text_centered(r, &verb, theme::text());
        }
    }

    fn draw_desk_icon(&self, c: &mut Canvas, item: &DeskItem, x: i32, y: i32) {
        match item {
            DeskItem::ThisPc => self.icons.draw_pic(c, Pic::Computer, LARGE, x, y),
            DeskItem::Bin => {
                let pic = if self.desk_icons.bin_full {
                    Pic::BinFull
                } else {
                    Pic::BinEmpty
                };
                self.icons.draw_pic(c, pic, LARGE, x, y);
            }
            DeskItem::App(a) => self.icons.draw_large(c, *a, x, y),
            DeskItem::Entry { dir: true, .. } => widgets::folder_icon(c, x, y + 4, 48),
            DeskItem::Entry { .. } => widgets::file_icon(c, x, y + 2, 48),
        }
    }
}

/// Split a label into at most two centred lines, breaking at spaces, and
/// cut the second with "…" if it is still too long.
fn wrap(text: &str, w: i32) -> Vec<String> {
    if UI.width(text) <= w {
        return alloc::vec![String::from(text)];
    }
    let mut first = String::new();
    let mut rest = text;
    for (at, _) in text.match_indices(' ') {
        if UI.width(&text[..at]) <= w {
            first = String::from(&text[..at]);
            rest = &text[at + 1..];
        }
    }
    if first.is_empty() {
        // no space to break at: cut by letters
        let mut n = 0;
        for (k, ch) in text.char_indices() {
            if UI.width(&text[..k + ch.len_utf8()]) > w {
                break;
            }
            n = k + ch.len_utf8();
        }
        first = String::from(&text[..n]);
        rest = &text[n..];
    }
    alloc::vec![first, super::search::fit(rest, w)]
}
