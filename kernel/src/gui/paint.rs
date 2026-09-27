//! Paint: draw on a picture with the mouse.
//!
//! Left button paints with the chosen colour, right button with white.
//! Tools: brush, eraser and flood fill, four brush sizes, and Clear.
//! Pictures are opened from and saved to the disk as PNG or BMP
//! (Ctrl+O, Ctrl+S), and one click makes the picture the desktop
//! background.

use alloc::format;
use alloc::string::String;

use super::canvas::{mix, rgb, Canvas, Color, Rect};
use super::filedialog::{self, FileDialog, Mode};
use super::personalize::{self, Fit};
use super::text::UI;
use super::{picture, theme, wallpaper};
use super::{MouseEvent, MouseKind};
use crate::fs;
use crate::keyboard::Key;
use crate::serial;
use crate::sync::StaticBuffer;

pub const PICTURE_W: usize = 960;
pub const PICTURE_H: usize = 600;
const TOOLBAR_H: i32 = 54;
pub const CLIENT_W: i32 = PICTURE_W as i32;
pub const CLIENT_H: i32 = TOOLBAR_H + PICTURE_H as i32;

const WHITE: Color = rgb(0xff, 0xff, 0xff);

const PALETTE: [Color; 24] = [
    rgb(0x00, 0x00, 0x00),
    rgb(0x7f, 0x7f, 0x7f),
    rgb(0x88, 0x00, 0x15),
    rgb(0xed, 0x1c, 0x24),
    rgb(0xff, 0x7f, 0x27),
    rgb(0xff, 0xf2, 0x00),
    rgb(0x22, 0xb1, 0x4c),
    rgb(0x00, 0xa2, 0xe8),
    rgb(0x3f, 0x48, 0xcc),
    rgb(0xa3, 0x49, 0xa4),
    rgb(0x6c, 0x3a, 0x1e),
    rgb(0x00, 0x60, 0x40),
    rgb(0xff, 0xff, 0xff),
    rgb(0xc3, 0xc3, 0xc3),
    rgb(0xb9, 0x7a, 0x57),
    rgb(0xff, 0xae, 0xc9),
    rgb(0xff, 0xc9, 0x0e),
    rgb(0xef, 0xe4, 0xb0),
    rgb(0xb5, 0xe6, 0x1d),
    rgb(0x99, 0xd9, 0xea),
    rgb(0x70, 0x92, 0xbe),
    rgb(0xc8, 0xbf, 0xe7),
    rgb(0x40, 0x40, 0x40),
    rgb(0x80, 0xff, 0xc0),
];
const SWATCH: i32 = 22;
const PALETTE_X: i32 = 52;

const SIZES: [i32; 4] = [1, 3, 6, 10];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Brush,
    Eraser,
    Fill,
}

const TOOLS: [(Tool, &str); 3] = [
    (Tool::Brush, "Brush"),
    (Tool::Eraser, "Eraser"),
    (Tool::Fill, "Fill"),
];

static PICTURE: StaticBuffer<{ PICTURE_W * PICTURE_H }> = StaticBuffer::new();
/// Work stack for flood fill, packed as `y << 16 | x`.
static FILL_STACK: StaticBuffer<{ 64 * 1024 }> = StaticBuffer::new();

pub struct Paint {
    picture: &'static mut [u32],
    stack: &'static mut [u32],
    color: Color,
    size: usize,
    tool: Tool,
    /// Last painted point while a button is held, and its colour.
    stroke: Option<(i32, i32, Color)>,
    /// The file the picture came from or was saved to.
    path: Option<String>,
    /// Changed since it was opened or saved.
    modified: bool,
    dialog: Option<FileDialog>,
    /// What happened last, shown in the toolbar: saved, opened, failed.
    note: String,
    /// The file button held down.
    pressed: Option<FileButton>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileButton {
    New,
    Open,
    Save,
    SaveAs,
    Background,
}

const FILE_BUTTONS: [(FileButton, &str, Rect); 5] = [
    (FileButton::New, "New", Rect::new(560, 5, 60, 20)),
    (FileButton::Open, "Open", Rect::new(626, 5, 60, 20)),
    (FileButton::Save, "Save", Rect::new(692, 5, 60, 20)),
    (FileButton::SaveAs, "Save as", Rect::new(758, 5, 80, 20)),
    (
        FileButton::Background,
        "Set as background",
        Rect::new(560, 29, 160, 20),
    ),
];

fn client() -> Rect {
    Rect::new(0, 0, CLIENT_W, CLIENT_H)
}

fn tool_button(i: usize) -> Rect {
    Rect::new(340 + i as i32 * 64, 5, 60, 20)
}

fn size_button(i: usize) -> Rect {
    Rect::new(340 + i as i32 * 36, 29, 32, 20)
}

fn clear_button() -> Rect {
    Rect::new(CLIENT_W - 76, 5, 70, 20)
}

fn swatch(i: usize) -> Rect {
    let (col, row) = ((i % 12) as i32, (i / 12) as i32);
    Rect::new(
        PALETTE_X + col * SWATCH,
        5 + row * SWATCH,
        SWATCH - 2,
        SWATCH - 2,
    )
}

impl Paint {
    pub fn new() -> Self {
        let picture = PICTURE.take();
        picture.fill(WHITE);
        Self {
            picture,
            stack: FILL_STACK.take(),
            color: PALETTE[0],
            size: 1,
            tool: Tool::Brush,
            stroke: None,
            path: None,
            modified: false,
            dialog: None,
            note: String::new(),
            pressed: None,
        }
    }

    // ---- files -------------------------------------------------------------

    fn pictures_folder() -> String {
        let user = crate::users::current_name().unwrap_or_default();
        fs::join(&fs::home(user.as_str()), "Pictures")
    }

    /// The name shown in the toolbar.
    fn file_label(&self) -> String {
        let name = self.path.as_deref().map_or("Untitled", fs::file_name);
        format!("{}{}", name, if self.modified { " *" } else { "" })
    }

    /// Load a picture file, shrinking it to the page if it is bigger.
    pub fn open_file(&mut self, path: &str) {
        self.dialog = None;
        let img = fs::read(path).ok().and_then(|d| picture::decode(&d));
        let Some(img) = img else {
            self.note = String::from("Can't open that file: it is not a PNG, JPEG or BMP picture");
            return;
        };
        let fit = if img.width > PICTURE_W || img.height > PICTURE_H {
            Fit::Fit
        } else {
            Fit::Center
        };
        let (w, h) = (PICTURE_W as i32, PICTURE_H as i32);
        wallpaper::fit(&img, fit, WHITE, self.picture, w, h);
        self.path = Some(String::from(path));
        self.modified = false;
        self.note = format!("Opened, {} x {}", img.width, img.height);
        serial::write_str("paint: opened ");
        serial::write_str(path);
        serial::write_str("\n");
    }

    /// Save to `path`: BMP if the name ends in .bmp, PNG otherwise.
    fn save_to(&mut self, path: &str) -> bool {
        let mut path = String::from(path);
        let lower = path.to_ascii_lowercase();
        let bmp = lower.ends_with(".bmp");
        if !bmp && !lower.ends_with(".png") {
            path.push_str(".png");
        }
        let data = if bmp {
            picture::encode_bmp(self.picture, PICTURE_W, PICTURE_H)
        } else {
            picture::encode_png(self.picture, PICTURE_W, PICTURE_H)
        };
        match fs::write(&path, &data) {
            Ok(()) => {
                self.note = format!("Saved, {} KB", data.len().div_ceil(1024));
                self.path = Some(path.clone());
                self.modified = false;
                serial::write_str("paint: saved ");
                serial::write_str(&path);
                serial::write_str("\n");
                true
            }
            Err(e) => {
                self.note = format!("Could not save: {}", e.message());
                false
            }
        }
    }

    fn save_as(&mut self) {
        let dir = match &self.path {
            Some(p) => fs::parent(p),
            None => Self::pictures_folder(),
        };
        let name = match &self.path {
            Some(p) => String::from(fs::file_name(p)),
            None => fs::unique_name(&dir, "Drawing", ".png"),
        };
        self.dialog = Some(FileDialog::new(Mode::Save, &dir, &name));
    }

    fn save(&mut self) {
        match self.path.clone() {
            Some(p) => {
                self.save_to(&p);
            }
            None => self.save_as(),
        }
    }

    /// Make the picture the desktop background, saving it first.
    fn set_background(&mut self) {
        if self.path.is_none() {
            // nowhere yet: straight into Pictures, no questions
            let dir = Self::pictures_folder();
            let _ = fs::create_dir(&dir);
            let name = fs::unique_name(&dir, "Drawing background", ".png");
            if !self.save_to(&fs::join(&dir, &name)) {
                return;
            }
        } else if self.modified {
            let p = self.path.clone().unwrap_or_default();
            if !self.save_to(&p) {
                return;
            }
        }
        if let Some(p) = &self.path {
            personalize::set_wallpaper(p);
            self.note = String::from("Set as background");
        }
    }

    fn new_picture(&mut self) {
        self.picture.fill(WHITE);
        self.path = None;
        self.modified = false;
        self.note.clear();
    }

    fn file_command(&mut self, b: FileButton) {
        match b {
            FileButton::New => self.new_picture(),
            FileButton::Open => {
                let dir = Self::pictures_folder();
                self.dialog = Some(FileDialog::new(Mode::Open, &dir, ""));
            }
            FileButton::Save => self.save(),
            FileButton::SaveAs => self.save_as(),
            FileButton::Background => self.set_background(),
        }
    }

    fn dialog_event(&mut self, event: filedialog::Event) -> bool {
        match event {
            filedialog::Event::None => false,
            filedialog::Event::Redraw => true,
            filedialog::Event::Cancel => {
                self.dialog = None;
                true
            }
            filedialog::Event::Chosen(path) => {
                let saving = self.dialog.as_ref().is_some_and(|d| d.mode == Mode::Save);
                self.dialog = None;
                if saving {
                    self.save_to(&path);
                } else {
                    self.open_file(&path);
                }
                true
            }
        }
    }

    pub fn on_key(&mut self, key: Key) -> bool {
        if let Some(d) = &mut self.dialog {
            let event = d.on_key(key, client());
            return self.dialog_event(event);
        }
        match key {
            Key::Ctrl('s') => self.save(),
            Key::Ctrl('o') => self.file_command(FileButton::Open),
            Key::Ctrl('n') => self.new_picture(),
            _ => return false,
        }
        true
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        match &mut self.dialog {
            Some(d) => d.on_wheel(clicks, client()),
            None => false,
        }
    }

    // ---- mouse -------------------------------------------------------------

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        if let Some(d) = &mut self.dialog {
            if let MouseKind::Down { right: false } = ev.kind {
                let event = d.on_click(client(), ev.x, ev.y);
                return self.dialog_event(event);
            }
            return false;
        }
        if let MouseKind::Up = ev.kind {
            if let Some(b) = self.pressed.take() {
                if FILE_BUTTONS
                    .iter()
                    .any(|f| f.0 == b && f.2.contains(ev.x, ev.y))
                {
                    self.file_command(b);
                }
                return true;
            }
        }
        let (x, y) = (ev.x, ev.y - TOOLBAR_H);
        match ev.kind {
            MouseKind::Down { right } => {
                if ev.y < TOOLBAR_H {
                    if let Some(f) = FILE_BUTTONS.iter().find(|f| f.2.contains(ev.x, ev.y)) {
                        if !right {
                            self.pressed = Some(f.0);
                        }
                        return true;
                    }
                    return self.toolbar_click(ev.x, ev.y, right);
                }
                if !(0..PICTURE_W as i32).contains(&x) || !(0..PICTURE_H as i32).contains(&y) {
                    return false;
                }
                let color = match (self.tool, right) {
                    (Tool::Eraser, _) | (_, true) => WHITE,
                    _ => self.color,
                };
                self.modified = true;
                if self.tool == Tool::Fill {
                    self.flood_fill(x, y, color);
                } else {
                    self.stamp(x, y, color);
                    self.stroke = Some((x, y, color));
                }
                true
            }
            MouseKind::Move => match self.stroke {
                Some((lx, ly, color)) => {
                    Canvas::trace_line(lx, ly, x, y, |px, py| self.stamp(px, py, color));
                    self.stroke = Some((x, y, color));
                    true
                }
                None => false,
            },
            MouseKind::Up => {
                self.stroke = None;
                false
            }
        }
    }

    fn toolbar_click(&mut self, x: i32, y: i32, right: bool) -> bool {
        if let Some(i) = (0..PALETTE.len()).find(|&i| swatch(i).contains(x, y)) {
            if !right {
                self.color = PALETTE[i];
                if self.tool == Tool::Eraser {
                    self.tool = Tool::Brush;
                }
            }
            return true;
        }
        if let Some(i) = (0..TOOLS.len()).find(|&i| tool_button(i).contains(x, y)) {
            self.tool = TOOLS[i].0;
            return true;
        }
        if let Some(i) = (0..SIZES.len()).find(|&i| size_button(i).contains(x, y)) {
            self.size = i;
            return true;
        }
        if clear_button().contains(x, y) {
            self.picture.fill(WHITE);
            self.modified = true;
            return true;
        }
        false
    }

    /// Paint a round dot of the brush size at a picture point.
    fn stamp(&mut self, cx: i32, cy: i32, color: Color) {
        let mut r = SIZES[self.size];
        if self.tool == Tool::Eraser {
            r = r * 2 + 2;
        }
        let r = r - 1;
        for dy in -r..=r {
            for dx in -r..=r {
                let (x, y) = (cx + dx, cy + dy);
                if dx * dx + dy * dy <= r * r + r
                    && (0..PICTURE_W as i32).contains(&x)
                    && (0..PICTURE_H as i32).contains(&y)
                {
                    self.picture[y as usize * PICTURE_W + x as usize] = color;
                }
            }
        }
    }

    /// Scanline flood fill from a point, replacing its colour.
    fn flood_fill(&mut self, x: i32, y: i32, color: Color) {
        let (w, h) = (PICTURE_W, PICTURE_H);
        let target = self.picture[y as usize * w + x as usize];
        if target == color {
            return;
        }
        let mut top = 0;
        self.stack[top] = (y as u32) << 16 | x as u32;
        top += 1;
        while top > 0 {
            top -= 1;
            let (sx, sy) = (
                (self.stack[top] & 0xffff) as usize,
                (self.stack[top] >> 16) as usize,
            );
            let row = sy * w;
            if self.picture[row + sx] != target {
                continue;
            }
            let mut left = sx;
            while left > 0 && self.picture[row + left - 1] == target {
                left -= 1;
            }
            let mut right = sx;
            while right + 1 < w && self.picture[row + right + 1] == target {
                right += 1;
            }
            self.picture[row + left..=row + right].fill(color);
            // queue one seed per run of target pixels above and below
            for ny in [sy.wrapping_sub(1), sy + 1] {
                if ny >= h {
                    continue;
                }
                let mut in_run = false;
                for nx in left..=right {
                    let matches = self.picture[ny * w + nx] == target;
                    if matches && !in_run && top < self.stack.len() {
                        self.stack[top] = (ny as u32) << 16 | nx as u32;
                        top += 1;
                    }
                    in_run = matches;
                }
            }
        }
    }

    pub fn draw(&mut self, c: &mut Canvas) {
        c.fill_rect(0, 0, CLIENT_W, TOOLBAR_H, theme::face());
        c.fill_rect(0, TOOLBAR_H - 1, CLIENT_W, 1, theme::stroke());

        // current colour
        let current = Rect::new(8, 7, 36, 36);
        c.fill_round(current, 8, self.color);
        c.outline_round(current, 8, theme::shadow());
        for (i, &color) in PALETTE.iter().enumerate() {
            let r = swatch(i);
            if color == self.color {
                c.fill_round(r.inset(-2), 12, theme::accent());
                c.fill_round(r, 10, theme::face());
            }
            let dot = r.inset(1);
            c.fill_round(dot, 9, color);
            c.outline_round(dot, 9, mix(color, theme::text(), 60));
        }

        for (i, &(tool, name)) in TOOLS.iter().enumerate() {
            theme::toggle_button(c, tool_button(i), name, self.tool == tool);
        }
        for (i, &size) in SIZES.iter().enumerate() {
            let r = size_button(i);
            theme::toggle_button(c, r, "", self.size == i);
            let d = (2 * size - 1).clamp(1, 14);
            let dot = Rect::new(r.x + (r.w - d) / 2, r.y + (r.h - d) / 2, d, d);
            c.fill_round(dot, d / 2, theme::text());
        }
        theme::button(c, clear_button(), "Clear", false);
        for (b, label, r) in FILE_BUTTONS {
            if b == FileButton::Background {
                theme::accent_button(c, r, label, self.pressed == Some(b));
            } else {
                theme::button(c, r, label, self.pressed == Some(b));
            }
        }
        let status = if self.note.is_empty() {
            self.file_label()
        } else {
            format!("{}  -  {}", self.file_label(), self.note)
        };
        let mut status = status;
        while UI.width(&status) > CLIENT_W - 736 && status.pop().is_some() {}
        c.draw_text(730, 31, &status, theme::text_dim());

        c.blit(
            0,
            TOOLBAR_H,
            PICTURE_W as i32,
            PICTURE_H as i32,
            self.picture,
            PICTURE_W,
        );
        if let Some(d) = &mut self.dialog {
            d.draw(c, client(), true);
        }
    }
}
