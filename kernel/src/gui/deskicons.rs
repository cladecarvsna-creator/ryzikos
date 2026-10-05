//! The icons on the desktop, like Windows 11: This PC, the Recycle Bin,
//! app shortcuts, then the files and folders in
//! the user's Desktop folder, in columns from the top left.
//!
//! Click selects, Ctrl+click adds, and dragging on the empty desktop
//! draws a see-through blue rectangle that selects what it touches.
//! Selected icons can be dragged onto the Recycle Bin or into a folder,
//! or anywhere on the desktop: they snap to the grid, and where each icon
//! stands is kept for the user.
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
            DeskItem::Entry { name, .. } => {
                // a shortcut shows just the program's name
                let lower = name.to_ascii_lowercase();
                if lower.ends_with(crate::web::LINK_EXT) {
                    &name[..name.len() - crate::web::LINK_EXT.len()]
                } else {
                    name
                }
            }
        }
    }

    /// A name for remembering where the icon stands.
    fn key(&self) -> String {
        match self {
            DeskItem::ThisPc => String::from("computer"),
            DeskItem::Bin => String::from("trash"),
            DeskItem::App(a) => alloc::format!("app:{}", a.key()),
            DeskItem::Entry { name, .. } => alloc::format!("file:{}", name),
        }
    }

    fn is_shortcut(&self) -> bool {
        matches!(self, DeskItem::Entry { name, dir: false }
            if name.to_ascii_lowercase().ends_with(crate::web::LINK_EXT))
    }
}

/// Where the user put the icons: a line per icon, "column row name".
fn layout_file() -> Option<String> {
    let name = users::current_name()?;
    Some(fs::join(&fs::app_data(name.as_str()), "desktop-icons.txt"))
}

fn load_layout() -> Vec<(String, (i32, i32))> {
    let Some(data) = layout_file().and_then(|p| fs::read(&p).ok()) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&data);
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let col = parts.next()?.parse().ok()?;
            let row = parts.next()?.parse().ok()?;
            Some((String::from(parts.next()?), (col, row)))
        })
        .collect()
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
    /// The grid cell (column, row) each item stands in.
    cells: Vec<(i32, i32)>,
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
            cells: Vec::new(),
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

    fn icon_cols(&self) -> i32 {
        ((self.width - LEFT) / CELL_W).max(1)
    }

    /// Icon `i`'s place on the desktop.
    pub(super) fn icon_rect(&self, i: usize) -> Rect {
        let rows = self.icon_rows();
        let (col, row) = self
            .desk_icons
            .cells
            .get(i)
            .copied()
            .unwrap_or((i as i32 / rows, i as i32 % rows));
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
        let placed = self.desk_icons.cells.iter().map(|c| c.0 + 1).max().unwrap_or(0);
        let cols = ((n as i32 - 1) / self.icon_rows() + 1).max(placed);
        Rect::new(0, 0, LEFT + cols * CELL_W + 8, self.height - TASKBAR_H)
    }

    /// Give every icon a cell: where the user left it if that cell is
    /// free, otherwise the first free cell, in columns from the top left.
    fn place_icons(&mut self) {
        let (cols, rows) = (self.icon_cols(), self.icon_rows());
        let saved = load_layout();
        let items = &self.desk_icons.items;
        let mut cells: Vec<Option<(i32, i32)>> = alloc::vec![None; items.len()];
        let mut taken: Vec<(i32, i32)> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let key = item.key();
            if let Some((_, cell)) = saved.iter().find(|(k, _)| *k == key) {
                let inside = cell.0 >= 0 && cell.0 < cols && cell.1 >= 0 && cell.1 < rows;
                if inside && !taken.contains(cell) {
                    cells[i] = Some(*cell);
                    taken.push(*cell);
                }
            }
        }
        let mut next = 0;
        for cell in cells.iter_mut().filter(|c| c.is_none()) {
            while taken.contains(&(next / rows, next % rows)) {
                next += 1;
            }
            let c = (next / rows, next % rows);
            taken.push(c);
            *cell = Some(c);
        }
        self.desk_icons.cells = cells.into_iter().map(|c| c.unwrap_or((0, 0))).collect();
    }

    /// Forget where the user put the icons and line them up again.
    pub(super) fn arrange_icons(&mut self) {
        if let Some(path) = layout_file() {
            let _ = fs::remove(&path);
        }
        let old = self.icons_area();
        self.desk_icons.forget();
        self.refresh_icons();
        self.damage(old);
    }

    /// Remember where every icon stands.
    fn save_layout(&self) {
        let Some(path) = layout_file() else {
            return;
        };
        // keep what was saved for icons that are not here now
        let mut lines: Vec<(String, (i32, i32))> = load_layout();
        for (item, cell) in self.desk_icons.items.iter().zip(&self.desk_icons.cells) {
            let key = item.key();
            lines.retain(|(k, _)| *k != key);
            lines.push((key, *cell));
        }
        let mut text = String::new();
        for (key, (col, row)) in lines.iter().rev().take(200).rev() {
            text.push_str(&alloc::format!("{} {} {}\n", col, row, key));
        }
        let _ = fs::write(&path, text.as_bytes());
    }

    /// Move the selected icons by (dx, dy) pixels, onto free cells.
    fn move_icons(&mut self, dx: i32, dy: i32) {
        let (cols, rows) = (self.icon_cols(), self.icon_rows());
        let moving = self.selected_icons();
        let mut taken: Vec<(i32, i32)> = (0..self.desk_icons.items.len())
            .filter(|i| !moving.contains(i))
            .map(|i| self.desk_icons.cells[i])
            .collect();
        for &i in &moving {
            let r = self.icon_rect(i);
            let x = r.x + r.w / 2 + dx - LEFT;
            let y = r.y + r.h / 2 + dy - TOP;
            let want = (
                x.div_euclid(CELL_W).clamp(0, cols - 1),
                y.div_euclid(CELL_H).clamp(0, rows - 1),
            );
            // the nearest free cell, looking further out step by step
            let mut best = self.desk_icons.cells[i];
            'search: for d in 0..cols.max(rows) {
                for c in want.0 - d..=want.0 + d {
                    for r in want.1 - d..=want.1 + d {
                        let edge = (c - want.0).abs() == d || (r - want.1).abs() == d;
                        if edge && c >= 0 && c < cols && r >= 0 && r < rows && !taken.contains(&(c, r)) {
                            best = (c, r);
                            break 'search;
                        }
                    }
                }
            }
            taken.push(best);
            self.desk_icons.cells[i] = best;
        }
        self.save_layout();
        self.damage(self.screen());
    }

    pub(super) fn damage_icons(&mut self) {
        self.damage(self.icons_area());
    }

    /// Place the icons again, for a new screen size.
    pub(super) fn relayout_icons(&mut self) {
        self.desk_icons.seen = None;
        self.refresh_icons();
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
        // the live CD's desktop starts the installer, like other live CDs
        if crate::install::available() {
            items.push(DeskItem::App(App::Installer));
        }
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
        self.place_icons();
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
        // a click in the rename box moves the caret, anywhere else finishes
        if let Some(i) = self.desk_icons.renaming.as_ref().map(|(i, _)| *i) {
            let box_ = self.icon_rename_rect(i);
            if !right && box_.contains(x, y) {
                if let Some((_, field)) = &mut self.desk_icons.renaming {
                    field.click(box_, x);
                }
                self.cursor_on = true;
                self.damage_icon_rename(i);
                return;
            }
        }
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
            // files and folders can also go into a folder or the Trash
            let files = self
                .selected_icons()
                .iter()
                .any(|&k| self.icon_path(k).is_some());
            let target = self.icon_at(x, y).filter(|&t| {
                files
                    && !self.desk_icons.selected[t]
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
            // dropped on the desktop: the icons stand where they were let go
            self.move_icons(drag.offset.0, drag.offset.1);
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
            DeskItem::ThisPc => self.show_folder(super::explorer::COMPUTER),
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
        self.cursor_on = true;
        self.damage_icon_rename(i);
    }

    /// Where icon `i`'s rename box goes: under the picture, wider than the
    /// icon so a long name fits, and kept on the screen.
    pub(super) fn icon_rename_rect(&self, i: usize) -> Rect {
        let r = self.icon_rect(i);
        let w = r.w + 60;
        let x = (r.x - 30).clamp(2, (self.width - w - 2).max(2));
        Rect::new(x, r.y + 56, w, 26)
    }

    /// Redraw icon `i` and its whole rename box, focus ring included.
    pub(super) fn damage_icon_rename(&mut self, i: usize) {
        let r = self.icon_rect(i).inset(-8).union(&self.icon_rename_rect(i).inset(-4));
        self.damage(r);
    }

    fn commit_icon_rename(&mut self) {
        let Some((i, field)) = self.desk_icons.renaming.take() else {
            return;
        };
        self.damage_icon_rename(i);
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
                _ => self.cursor_on = true,
            }
            self.damage_icon_rename(i);
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
                .item("Arrange icons", Cmd::ArrangeIcons)
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
            let box_ = self.icon_rename_rect(*i);
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
            it if it.is_shortcut() => {
                self.icons.draw_large(c, App::Program, x, y);
                // the little arrow that marks a shortcut
                let r = Rect::new(x + 2, y + 32, 16, 16);
                c.fill_round(r, 3, 0xffffff);
                c.outline_round(r, 3, rgb(0x60, 0x68, 0x78));
                super::browser::shortcut_arrow(c, r);
            }
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
