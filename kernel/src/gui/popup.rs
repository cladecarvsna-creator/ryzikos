//! Right-click menus of the desktop itself: on the desktop and its
//! icons, on taskbar buttons, on the Start button and on title bars.
//! Apps draw their own menus inside their windows; these float above
//! everything.

use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{Canvas, Rect};
use super::widgets::{self, Item};
use super::App;

/// What a menu item does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Open(App),
    Pin(App),
    Unpin(App),
    Close(App),
    Minimize(App),
    /// Move a window to desktop `n`.
    MoveTo(App, usize),
    MoveToNew(App),
    NewDesktop,
    TaskView,
    ShowDesktop,
    Search,
    /// Desktop icon `n`.
    OpenIcon(usize),
    RenameIcon(usize),
    DeleteIcons,
    EmptyBin,
    Refresh,
    /// Put the desktop icons back in columns from the top left.
    ArrangeIcons,
    NewFolder,
    NewFile,
    /// Settings on the Personalization or the System page.
    Personalize,
    DisplaySettings,
    /// The next built-in desktop background.
    NextBackground,
    /// Use desktop icon `n` (a picture) as the background.
    SetBackground(usize),
    Lock,
    SignOut,
    Restart,
    ShutDown,
    /// Cut ('x'), copy ('c'), paste ('v') or select all ('a') in an app,
    /// as if Ctrl and the letter were pressed.
    Edit(App, char),
}

pub struct Popup {
    pub rect: Rect,
    /// Label, shortcut text, and what it does (None is a separator or
    /// an item that can't be used now).
    entries: Vec<(String, &'static str, Option<Cmd>)>,
    pub hover: Option<usize>,
}

/// Collects the items of a menu.
#[derive(Default)]
pub struct Builder {
    entries: Vec<(String, &'static str, Option<Cmd>)>,
}

impl Builder {
    pub fn item(mut self, label: &str, cmd: Cmd) -> Self {
        self.entries.push((String::from(label), "", Some(cmd)));
        self
    }

    pub fn keyed(mut self, label: &str, key: &'static str, cmd: Cmd) -> Self {
        self.entries.push((String::from(label), key, Some(cmd)));
        self
    }

    /// An item shown greyed out, or usable when `on`.
    pub fn maybe(mut self, label: &str, cmd: Cmd, on: bool) -> Self {
        self.entries
            .push((String::from(label), "", on.then_some(cmd)));
        self
    }

    /// A greyed-out or usable item with its shortcut.
    pub fn keyed_maybe(mut self, label: &str, key: &'static str, cmd: Cmd, on: bool) -> Self {
        self.entries.push((String::from(label), key, on.then_some(cmd)));
        self
    }

    pub fn sep(mut self) -> Self {
        self.entries.push((String::new(), "", None));
        self
    }

    /// The menu with its top left corner at the pointer, or its bottom
    /// at `y` when `above` (over the taskbar), kept on the screen.
    pub fn at(self, x: i32, y: i32, above: bool, screen: Rect) -> Popup {
        let mut p = Popup {
            rect: Rect::default(),
            entries: self.entries,
            hover: None,
        };
        let mut r = widgets::menu_rect(x, y, &p.items());
        if above || r.bottom() > screen.bottom() - 4 {
            r.y = y - r.h;
        }
        r.x = r.x.clamp(4, screen.right() - r.w - 4);
        r.y = r.y.max(4);
        p.rect = r;
        p
    }
}

impl Popup {
    fn items(&self) -> Vec<Item<'_>> {
        self.entries
            .iter()
            .map(|(l, k, c)| (l.as_str(), *k, c.is_some()))
            .collect()
    }

    /// The command under a point, if any.
    pub fn cmd_at(&self, x: i32, y: i32) -> Option<Cmd> {
        let i = widgets::menu_item_at(self.rect, &self.items(), x, y)?;
        self.entries[i].2
    }

    /// Follow the pointer. Returns whether the highlight moved.
    pub fn set_hover(&mut self, x: i32, y: i32) -> bool {
        let hover = widgets::menu_item_at(self.rect, &self.items(), x, y);
        core::mem::replace(&mut self.hover, hover) != hover
    }

    /// The area it draws on, shadow included.
    pub fn bounds(&self) -> Rect {
        self.rect.inset(-14)
    }

    pub fn draw(&self, c: &mut Canvas) {
        if c.visible(self.bounds()) {
            widgets::draw_menu(c, self.rect, &self.items(), self.hover);
        }
    }
}
