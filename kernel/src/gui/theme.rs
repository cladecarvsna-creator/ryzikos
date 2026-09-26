//! Colours and widgets shared by the desktop and the apps, in the style
//! of Windows 11: light or dark surfaces, thin borders and rounded
//! corners, and an accent colour. Settings > Personalization picks the
//! mode and the accent (personalize.rs); everything asks here for its
//! colours when it draws, so a change shows on the next frame.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::canvas::{mix, rgb, Canvas, Color, Rect};

static DARK: AtomicBool = AtomicBool::new(false);
static ACCENT_COLOR: AtomicU32 = AtomicU32::new(0x0067c0);

/// Accent colours to choose from, as in Windows: a name and the colour.
pub const ACCENTS: [(&str, Color); 10] = [
    ("Blue", rgb(0x00, 0x67, 0xc0)),
    ("Navy", rgb(0x00, 0x3e, 0x92)),
    ("Purple", rgb(0x74, 0x4d, 0xa9)),
    ("Pink", rgb(0xbf, 0x00, 0x77)),
    ("Red", rgb(0xc4, 0x2b, 0x1c)),
    ("Orange", rgb(0xca, 0x50, 0x10)),
    ("Gold", rgb(0x98, 0x6f, 0x0b)),
    ("Green", rgb(0x10, 0x7c, 0x10)),
    ("Teal", rgb(0x00, 0x7c, 0x89)),
    ("Graphite", rgb(0x51, 0x5c, 0x6b)),
];

/// Switch between the light and the dark look and set the accent.
pub fn set(dark: bool, accent: Color) {
    DARK.store(dark, Ordering::Relaxed);
    ACCENT_COLOR.store(accent, Ordering::Relaxed);
}

pub fn dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

fn pick(light: Color, dark_color: Color) -> Color {
    if dark() {
        dark_color
    } else {
        light
    }
}

/// Window and panel background.
pub fn face() -> Color {
    pick(rgb(0xf3, 0xf3, 0xf3), rgb(0x20, 0x20, 0x20))
}

/// Cards, lists and text boxes: white in the light look.
pub fn light() -> Color {
    pick(rgb(0xff, 0xff, 0xff), rgb(0x2b, 0x2b, 0x2b))
}

/// A little lighter than `face`, for side bars and footers.
pub fn raised() -> Color {
    pick(rgb(0xf7, 0xf8, 0xfa), rgb(0x27, 0x27, 0x27))
}

/// Thin borders around controls.
pub fn stroke() -> Color {
    pick(rgb(0xdc, 0xdc, 0xdc), rgb(0x3e, 0x3e, 0x3e))
}

/// Borders of menus, flyouts and dialogs.
pub fn frame() -> Color {
    pick(rgb(0xc8, 0xca, 0xd2), rgb(0x4a, 0x4a, 0x4e))
}

pub fn shadow() -> Color {
    pick(rgb(0xb4, 0xb4, 0xb4), rgb(0x0c, 0x0c, 0x0c))
}

pub fn text() -> Color {
    pick(rgb(0x1a, 0x1a, 0x1a), rgb(0xf2, 0xf2, 0xf2))
}

pub fn text_dim() -> Color {
    pick(rgb(0x70, 0x70, 0x70), rgb(0xa6, 0xa6, 0xa6))
}

/// The accent colour, a little lighter in the dark look so it stands out.
pub fn accent() -> Color {
    let a = ACCENT_COLOR.load(Ordering::Relaxed);
    if dark() {
        mix(a, 0xffffff, 90)
    } else {
        a
    }
}

/// The accent as chosen, for things that look the same in both modes.
pub fn accent_base() -> Color {
    ACCENT_COLOR.load(Ordering::Relaxed)
}

/// A pale accent for chosen items.
pub fn accent_light() -> Color {
    let a = ACCENT_COLOR.load(Ordering::Relaxed);
    if dark() {
        mix(face(), a, 90)
    } else {
        mix(0xffffff, a, 28)
    }
}

/// Text on an accent-coloured button.
pub fn on_accent() -> Color {
    if dark() {
        rgb(0x10, 0x10, 0x10)
    } else {
        0xffffff
    }
}

/// Under the mouse, in menus and lists.
pub fn hover() -> Color {
    pick(rgb(0xe6, 0xe8, 0xee), rgb(0x38, 0x38, 0x3a))
}

/// A soft highlight for list rows under the mouse.
pub fn row_hover() -> Color {
    let a = ACCENT_COLOR.load(Ordering::Relaxed);
    pick(mix(0xffffff, a, 22), mix(rgb(0x2b, 0x2b, 0x2b), a, 34))
}

/// Chosen list rows and selected text, as Windows paints them: the
/// accent mixed into the background.
pub fn selection() -> Color {
    let a = ACCENT_COLOR.load(Ordering::Relaxed);
    pick(mix(0xffffff, a, 64), mix(rgb(0x2b, 0x2b, 0x2b), a, 110))
}

/// The border of a chosen item and of the selection rectangle.
pub fn selection_edge() -> Color {
    let a = ACCENT_COLOR.load(Ordering::Relaxed);
    pick(mix(0xffffff, a, 150), mix(0xffffff, a, 170))
}

/// Menus and flyouts.
pub fn menu() -> Color {
    pick(rgb(0xf9, 0xf9, 0xfb), rgb(0x2c, 0x2c, 0x2c))
}

/// Flyouts like the start menu and search: a touch cooler than `face`.
pub fn panel() -> Color {
    pick(rgb(0xf5, 0xf6, 0xfa), rgb(0x24, 0x24, 0x26))
}

/// The footer strip of the start menu and quick settings.
pub fn footer() -> Color {
    pick(rgb(0xec, 0xee, 0xf4), rgb(0x1c, 0x1c, 0x1e))
}

/// Buttons and tiles at rest.
pub fn control() -> Color {
    pick(rgb(0xfb, 0xfb, 0xfd), rgb(0x33, 0x33, 0x35))
}

/// Buttons and tiles under the mouse.
pub fn control_lit() -> Color {
    pick(0xffffff, rgb(0x3c, 0x3c, 0x3e))
}

/// The scroll bar track and thumb.
pub fn track() -> Color {
    pick(rgb(0xf6, 0xf6, 0xf8), rgb(0x26, 0x26, 0x26))
}

pub fn thumb() -> Color {
    pick(rgb(0x9a, 0x9c, 0xa4), rgb(0x70, 0x70, 0x74))
}

/// The taskbar, drawn see-through over the wallpaper.
pub fn taskbar() -> Color {
    pick(rgb(0xf0, 0xf2, 0xf8), rgb(0x1c, 0x1c, 0x1e))
}

/// Title bars of the active window and of the others.
pub fn title_active() -> Color {
    pick(rgb(0xee, 0xf1, 0xf8), rgb(0x1c, 0x1c, 0x1c))
}

pub fn window_border(focused: bool) -> Color {
    match (dark(), focused) {
        (false, true) => rgb(0x8c, 0x90, 0x9c),
        (false, false) => rgb(0xb4, 0xb4, 0xb8),
        (true, true) => mix(
            rgb(0x50, 0x50, 0x54),
            ACCENT_COLOR.load(Ordering::Relaxed),
            90,
        ),
        (true, false) => rgb(0x3c, 0x3c, 0x40),
    }
}

/// Warnings like "unsaved" or "disk full".
pub fn warning() -> Color {
    pick(rgb(0xb0, 0x5a, 0x00), rgb(0xfc, 0xb0, 0x4c))
}

pub fn error() -> Color {
    pick(rgb(0xc4, 0x2b, 0x1c), rgb(0xff, 0x7a, 0x6c))
}

/// Corner radius of buttons and small controls.
pub const CONTROL_RADIUS: i32 = 5;

/// The light-look colours under their old names, for code that has not
/// moved to the functions above (the browser's own chrome).
#[allow(dead_code)]
pub const FACE: Color = rgb(0xf3, 0xf3, 0xf3);
#[allow(dead_code)]
pub const LIGHT: Color = rgb(0xff, 0xff, 0xff);
#[allow(dead_code)]
pub const STROKE: Color = rgb(0xdc, 0xdc, 0xdc);
#[allow(dead_code)]
pub const SHADOW: Color = rgb(0xb4, 0xb4, 0xb4);
#[allow(dead_code)]
pub const TEXT: Color = rgb(0x1a, 0x1a, 0x1a);
#[allow(dead_code)]
pub const TEXT_DIM: Color = rgb(0x70, 0x70, 0x70);
#[allow(dead_code)]
pub const ACCENT: Color = rgb(0x00, 0x67, 0xc0);
#[allow(dead_code)]
pub const ACCENT_LIGHT: Color = rgb(0xe3, 0xee, 0xfa);

/// A push button with a centred label.
pub fn button(c: &mut Canvas, r: Rect, label: &str, pressed: bool) {
    colored_button(c, r, label, control(), pressed);
}

/// A button showing an option that is turned on, such as the chosen tool.
pub fn toggle_button(c: &mut Canvas, r: Rect, label: &str, on: bool) {
    if on {
        c.fill_round(r, CONTROL_RADIUS, accent_light());
        c.outline_round(r, CONTROL_RADIUS, mix(accent(), light(), 120));
        c.text_centered(r, label, if dark() { text() } else { accent() });
    } else {
        button(c, r, label, false);
    }
}

pub fn colored_button(c: &mut Canvas, r: Rect, label: &str, face: Color, pressed: bool) {
    let face = if pressed {
        mix(face, shadow(), 50)
    } else {
        face
    };
    c.fill_round(r, CONTROL_RADIUS, face);
    c.outline_round(r, CONTROL_RADIUS, stroke());
    // the slightly darker bottom edge Windows 11 buttons have
    if !pressed {
        c.fill_rect(
            r.x + CONTROL_RADIUS,
            r.bottom() - 1,
            r.w - 2 * CONTROL_RADIUS,
            1,
            mix(stroke(), text(), 30),
        );
    }
    c.text_centered(r, label, if pressed { text_dim() } else { text() });
}

/// The button everything else in a dialog leads to, in the accent colour.
pub fn accent_button(c: &mut Canvas, r: Rect, label: &str, pressed: bool) {
    let face = if pressed {
        mix(accent(), light(), 50)
    } else {
        accent()
    };
    c.fill_round(r, CONTROL_RADIUS, face);
    c.outline_round(r, CONTROL_RADIUS, mix(accent(), text(), 40));
    c.text_centered(r, label, on_accent());
}

/// What lights up buttons on see-through bars, blended with some alpha:
/// white over the light taskbar, soft grey over the dark one.
pub fn glass() -> Color {
    pick(0xffffff, rgb(0x5c, 0x5c, 0x60))
}
