//! Turns PS/2 scancodes (set 1) into keys, with US and Russian layouts.
//! Alt+Shift switches the layout.

use core::sync::atomic::{AtomicBool, Ordering};

/// Whether Shift and Ctrl are held now, for apps that select text with
/// Shift+arrows or jump by words with Ctrl+arrows.
static SHIFT: AtomicBool = AtomicBool::new(false);
static CTRL: AtomicBool = AtomicBool::new(false);
static ALT: AtomicBool = AtomicBool::new(false);
static SUPER: AtomicBool = AtomicBool::new(false);

pub fn shift_held() -> bool {
    SHIFT.load(Ordering::Relaxed)
}

pub fn ctrl_held() -> bool {
    CTRL.load(Ordering::Relaxed)
}

pub fn alt_held() -> bool {
    ALT.load(Ordering::Relaxed)
}

/// Whether the Windows key is held, for Win+Tab, Win+Ctrl+Left and so on.
pub fn super_held() -> bool {
    SUPER.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Us,
    Ru,
}

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Layout::Us => "EN",
            Layout::Ru => "RU",
        }
    }
}

#[derive(Clone, Copy)]
pub enum Key {
    Char(char),
    /// Ctrl plus a letter (given as the lowercase Latin letter), a digit
    /// or Tab ('\t').
    Ctrl(char),
    Enter,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    /// The Windows key, pressed and let go on its own.
    Super,
    /// Alt was let go (Alt+Tab picks the window then).
    AltUp,
    PageUp,
    PageDown,
    Home,
    End,
    Delete,
    /// F1 to F12.
    Function(u8),
    /// Alt+Shift switched the layout.
    LayoutChanged,
    /// Print Screen (also Alt+Print Screen, which sends SysRq).
    PrintScreen,
}

/// Key rows of the main block as [layout][shift] strings.
struct Row {
    first: u8,
    chars: [[&'static str; 2]; 2],
}

const ROWS: [Row; 6] = [
    Row {
        first: 0x02,
        chars: [
            ["1234567890-=", "!@#$%^&*()_+"],
            ["1234567890-=", "!\"№;%:?*()_+"],
        ],
    },
    Row {
        first: 0x10,
        chars: [
            ["qwertyuiop[]", "QWERTYUIOP{}"],
            ["йцукенгшщзхъ", "ЙЦУКЕНГШЩЗХЪ"],
        ],
    },
    Row {
        first: 0x1e,
        chars: [
            ["asdfghjkl;'`", "ASDFGHJKL:\"~"],
            ["фывапролджэё", "ФЫВАПРОЛДЖЭЁ"],
        ],
    },
    Row {
        first: 0x2b,
        chars: [
            ["\\zxcvbnm,./", "|ZXCVBNM<>?"],
            ["\\ячсмитьбю.", "/ЯЧСМИТЬБЮ,"],
        ],
    },
    Row {
        first: 0x37,
        chars: [["*", "*"], ["*", "*"]],
    },
    Row {
        first: 0x39,
        chars: [[" ", " "], [" ", " "]],
    },
];

pub struct Keyboard {
    left_shift: bool,
    right_shift: bool,
    ctrl: bool,
    alt: bool,
    /// The Windows key is down, and whether another key was pressed
    /// with it (then letting it go doesn't open the start menu).
    win: bool,
    win_used: bool,
    caps_lock: bool,
    /// The previous byte was the 0xe0 prefix of an extended key.
    extended: bool,
    layout: Layout,
}

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            left_shift: false,
            right_shift: false,
            ctrl: false,
            alt: false,
            win: false,
            win_used: false,
            caps_lock: false,
            extended: false,
            layout: Layout::Us,
        }
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Switch between EN and RU, as Alt+Shift does.
    pub fn toggle_layout(&mut self) {
        self.switch_layout();
    }

    fn shift(&self) -> bool {
        self.left_shift || self.right_shift
    }

    fn switch_layout(&mut self) -> Option<Key> {
        self.layout = match self.layout {
            Layout::Us => Layout::Ru,
            Layout::Ru => Layout::Us,
        };
        Some(Key::LayoutChanged)
    }

    pub fn feed(&mut self, scancode: u8) -> Option<Key> {
        let key = self.decode(scancode);
        SHIFT.store(self.shift(), Ordering::Relaxed);
        CTRL.store(self.ctrl, Ordering::Relaxed);
        ALT.store(self.alt, Ordering::Relaxed);
        SUPER.store(self.win, Ordering::Relaxed);
        key
    }

    fn decode(&mut self, scancode: u8) -> Option<Key> {
        if scancode == 0xe0 {
            self.extended = true;
            return None;
        }
        let extended = core::mem::replace(&mut self.extended, false);
        let released = scancode & 0x80 != 0;
        let code = scancode & 0x7f;

        // modifiers
        match (code, extended) {
            (0x2a, false) | (0x36, false) => {
                let was_down = self.shift();
                if code == 0x2a {
                    self.left_shift = !released;
                } else {
                    self.right_shift = !released;
                }
                return if !released && !was_down && self.alt {
                    self.switch_layout()
                } else {
                    None
                };
            }
            (0x2a, true) | (0x36, true) => return None, // fake shifts
            (0x1d, _) => {
                self.ctrl = !released;
                return None;
            }
            (0x38, _) => {
                let was_down = self.alt;
                self.alt = !released;
                return if !released && !was_down && self.shift() {
                    self.switch_layout()
                } else if released && was_down {
                    Some(Key::AltUp)
                } else {
                    None
                };
            }
            (0x5b | 0x5c, true) => {
                let was_down = self.win;
                self.win = !released;
                if !released && !was_down {
                    self.win_used = false;
                }
                return (released && was_down && !self.win_used).then_some(Key::Super);
            }
            _ => {}
        }
        if released {
            return None;
        }
        if self.win {
            self.win_used = true;
        }
        if extended {
            return match code {
                0x48 => Some(Key::Up),
                0x50 => Some(Key::Down),
                0x4b => Some(Key::Left),
                0x4d => Some(Key::Right),
                0x49 => Some(Key::PageUp),
                0x51 => Some(Key::PageDown),
                0x47 => Some(Key::Home),
                0x4f => Some(Key::End),
                0x53 => Some(Key::Delete),
                0x1c => Some(Key::Enter), // keypad enter
                0x37 => Some(Key::PrintScreen),
                _ => None,
            };
        }
        match code {
            0x01 => return Some(Key::Escape),
            0x0e => return Some(Key::Backspace),
            // Ctrl+Tab switches browser tabs
            0x0f if self.ctrl => return Some(Key::Ctrl('\t')),
            0x0f => return Some(Key::Char('\t')),
            0x1c => return Some(Key::Enter),
            0x54 => return Some(Key::PrintScreen),
            0x3b..=0x44 => return Some(Key::Function(code - 0x3a)),
            0x57 | 0x58 => return Some(Key::Function(code - 0x57 + 11)),
            0x3a => {
                self.caps_lock = !self.caps_lock;
                return None;
            }
            _ => {}
        }
        if self.ctrl {
            let c = self.lookup(code, Layout::Us, false)?;
            return (c.is_ascii_lowercase() || c.is_ascii_digit()).then_some(Key::Ctrl(c));
        }
        let plain = self.lookup(code, self.layout, false)?;
        // Caps Lock only affects letters
        let shifted = self.shift() ^ (self.caps_lock && plain.is_alphabetic());
        self.lookup(code, self.layout, shifted).map(Key::Char)
    }

    fn lookup(&self, code: u8, layout: Layout, shifted: bool) -> Option<char> {
        let row = ROWS.iter().rev().find(|row| code >= row.first)?;
        row.chars[layout as usize][shifted as usize]
            .chars()
            .nth((code - row.first) as usize)
    }
}
