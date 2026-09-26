//! Personalization: light or dark mode, the accent colour and the
//! desktop background, as Settings > Personalization sets them.
//!
//! The choices live here, shared by Settings, File Explorer and Paint
//! (both can set a picture as the background). Any change is saved at
//! once to the user's `AppData/personalization.txt`, and to
//! `/$lockscreen.txt` so the lock screen after a restart looks the same.
//! The desktop picks changes up in its main loop and applies them.

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

use super::canvas::{rgb, Color};
use super::theme;
use crate::fs;
use crate::sync::IrqMutex;

/// How a picture is fitted to the screen, as in Windows (which calls one
/// of them "Fit" too).
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum Fit {
    /// Cover the whole screen, cutting off what does not fit.
    Fill,
    /// Show the whole picture, with bars of the background colour.
    Fit,
    Stretch,
    /// Actual size in the middle.
    Center,
    /// Actual size, repeated.
    Tile,
}

pub const FITS: [(Fit, &str); 5] = [
    (Fit::Fill, "Fill"),
    (Fit::Fit, "Fit"),
    (Fit::Stretch, "Stretch"),
    (Fit::Center, "Center"),
    (Fit::Tile, "Tile"),
];

impl Fit {
    fn key(self) -> &'static str {
        match self {
            Fit::Fill => "fill",
            Fit::Fit => "fit",
            Fit::Stretch => "stretch",
            Fit::Center => "center",
            Fit::Tile => "tile",
        }
    }

    fn from_key(key: &str) -> Option<Fit> {
        FITS.iter().map(|f| f.0).find(|f| f.key() == key)
    }
}

/// What the desktop shows.
#[derive(Clone, PartialEq, Eq)]
pub enum Background {
    /// One of the pictures that come with EverOS (wallpaper.rs).
    Builtin(usize),
    /// A PNG, JPEG or BMP file on the disk.
    Picture(String),
    /// Just the background colour.
    Solid,
}

/// Background colours to choose from, also used around fitted pictures.
pub const COLORS: [Color; 8] = [
    rgb(0x00, 0x00, 0x00),
    rgb(0x1f, 0x2a, 0x44),
    rgb(0x00, 0x63, 0xb1),
    rgb(0x2d, 0x7d, 0x9a),
    rgb(0x10, 0x7c, 0x10),
    rgb(0x74, 0x4d, 0xa9),
    rgb(0xc3, 0x00, 0x52),
    rgb(0x68, 0x76, 0x8a),
];

#[derive(Clone, PartialEq, Eq)]
pub struct Prefs {
    pub dark: bool,
    pub accent: Color,
    pub background: Background,
    pub fit: Fit,
    /// The background colour: the whole desktop for `Solid`, the bars
    /// beside a fitted picture otherwise.
    pub color: Color,
}

impl Prefs {
    pub const fn new() -> Self {
        Prefs {
            dark: false,
            accent: rgb(0x00, 0x67, 0xc0),
            background: Background::Builtin(0),
            fit: Fit::Fill,
            color: rgb(0x00, 0x00, 0x00),
        }
    }

    fn to_text(&self) -> String {
        let wallpaper = match &self.background {
            Background::Builtin(i) => format!("builtin:{}", i),
            Background::Picture(path) => format!("file:{}", path),
            Background::Solid => String::from("solid"),
        };
        format!(
            "# EverOS personalization\nmode={}\naccent={:06x}\nwallpaper={}\nfit={}\ncolor={:06x}\n",
            if self.dark { "dark" } else { "light" },
            self.accent,
            wallpaper,
            self.fit.key(),
            self.color,
        )
    }

    fn from_text(text: &str) -> Prefs {
        let mut p = Prefs::new();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let hex = || u32::from_str_radix(value, 16).ok().map(|c| c & 0xff_ffff);
            match key.trim() {
                "mode" => p.dark = value == "dark",
                "accent" => p.accent = hex().unwrap_or(p.accent),
                "color" => p.color = hex().unwrap_or(p.color),
                "fit" => p.fit = Fit::from_key(value).unwrap_or(p.fit),
                "wallpaper" => {
                    p.background = if let Some(i) = value.strip_prefix("builtin:") {
                        Background::Builtin(i.parse().unwrap_or(0))
                    } else if let Some(path) = value.strip_prefix("file:") {
                        Background::Picture(String::from(path))
                    } else if value == "solid" {
                        Background::Solid
                    } else {
                        p.background
                    }
                }
                _ => {}
            }
        }
        p
    }
}

static PREFS: IrqMutex<Prefs> = IrqMutex::new(Prefs::new());
/// Set when the look changed and the desktop has not caught up.
static CHANGED: AtomicBool = AtomicBool::new(false);
/// Set when the background changed (not only the colours).
static BACKGROUND_CHANGED: AtomicBool = AtomicBool::new(false);

const LOCK_FILE: &str = "/$lockscreen.txt";

pub fn get() -> Prefs {
    PREFS.lock().clone()
}

/// Change the settings, save them and tell the desktop.
pub fn update(f: impl FnOnce(&mut Prefs)) {
    let (before, after) = {
        let mut p = PREFS.lock();
        let before = p.clone();
        f(&mut p);
        (before, p.clone())
    };
    if before == after {
        return;
    }
    if before.background != after.background
        || before.color != after.color
        || before.fit != after.fit
    {
        BACKGROUND_CHANGED.store(true, Ordering::Relaxed);
    }
    theme::set(after.dark, after.accent);
    CHANGED.store(true, Ordering::Relaxed);
    save(&after);
}

/// Use a picture file as the desktop background.
pub fn set_wallpaper(path: &str) {
    update(|p| p.background = Background::Picture(String::from(path)));
}

/// Whether the look changed since the last call, and whether that
/// includes the background.
pub fn take_changed() -> Option<bool> {
    if CHANGED.swap(false, Ordering::Relaxed) {
        Some(BACKGROUND_CHANGED.swap(false, Ordering::Relaxed))
    } else {
        None
    }
}

fn user_file() -> Option<String> {
    let user = crate::users::current_name()?;
    Some(fs::join(
        &fs::app_data(user.as_str()),
        "personalization.txt",
    ))
}

fn save(p: &Prefs) {
    let text = p.to_text();
    if let Some(path) = user_file() {
        let _ = fs::write(&path, text.as_bytes());
    }
    let _ = fs::write(LOCK_FILE, text.as_bytes());
}

fn load_from(path: &str) -> bool {
    let Ok(data) = fs::read(path) else {
        return false;
    };
    let p = Prefs::from_text(core::str::from_utf8(&data).unwrap_or(""));
    theme::set(p.dark, p.accent);
    *PREFS.lock() = p;
    true
}

/// At start: the look the lock screen had last time.
pub fn load_boot() {
    load_from(LOCK_FILE);
}

/// After signing in: the user's own settings, if they have any. Returns
/// whether anything changed.
pub fn load_user() -> bool {
    let before = get();
    if let Some(path) = user_file() {
        if !load_from(&path) {
            // a new user starts with what the lock screen shows
            save(&before);
        }
    }
    let after = get();
    if after.background != before.background
        || after.color != before.color
        || after.fit != before.fit
    {
        BACKGROUND_CHANGED.store(true, Ordering::Relaxed);
    }
    if after != before {
        let _ = fs::write(LOCK_FILE, after.to_text().as_bytes());
        CHANGED.store(true, Ordering::Relaxed);
        true
    } else {
        false
    }
}
