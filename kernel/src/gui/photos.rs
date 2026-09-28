//! Photos: a gallery of the pictures in the usual folders and on the
//! disc, and a viewer for one picture with zoom, panning, rotation and
//! the previous and next picture in its folder.
//!
//! Thumbnails are made one per tick in the background, so the gallery
//! opens at once and fills in.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::filedialog::{self, FileDialog, Mode};
use super::picture::{self, Image};
use super::text::{HEADING, UI_BOLD};
use super::{theme, MouseEvent, MouseKind};
use crate::fs;
use crate::keyboard::Key;

pub const CLIENT_W: i32 = 1100;
pub const CLIENT_H: i32 = 720;

/// The window's size now; it opens at CLIENT_W x CLIENT_H.
fn cw() -> i32 {
    super::client_w(super::App::Photos)
}
fn ch() -> i32 {
    super::client_h(super::App::Photos)
}

const BAR_H: i32 = 52;
const STATUS_H: i32 = 30;
const THUMB_W: i32 = 190;
const THUMB_H: i32 = 140;
const CELL_W: i32 = 210;
const CELL_H: i32 = 180;
const GRID_X: i32 = 24;
const GRID_Y: i32 = BAR_H + 56;
/// Most pictures the gallery shows.
const MAX_ITEMS: usize = 120;

const BACKDROP: u32 = rgb(0x18, 0x19, 0x1d);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Gallery,
    Open,
    Previous,
    Next,
    ZoomOut,
    ZoomIn,
    Fit,
    Rotate,
    Edit,
    Background,
}

const BUTTONS: [(Button, &str, i32); 10] = [
    (Button::Gallery, "Gallery", 84),
    (Button::Open, "Open...", 84),
    (Button::Previous, "< Previous", 104),
    (Button::Next, "Next >", 84),
    (Button::ZoomOut, "-", 40),
    (Button::ZoomIn, "+", 40),
    (Button::Fit, "Fit", 56),
    (Button::Rotate, "Rotate", 76),
    (Button::Edit, "Edit in Draw", 118),
    (Button::Background, "Set as background", 164),
];

fn button_rects() -> Vec<(Button, &'static str, Rect)> {
    let mut x = 12;
    BUTTONS
        .iter()
        .map(|&(b, label, w)| {
            let r = Rect::new(x, 10, w, 32);
            x += w + 6;
            if matches!(b, Button::Open | Button::Next | Button::Fit | Button::Rotate) {
                x += 10;
            }
            (b, label, r)
        })
        .collect()
}

fn view_rect() -> Rect {
    Rect::new(0, BAR_H, cw(), ch() - BAR_H - STATUS_H)
}

/// Pictures (or videos) in the usual places: some home folders, the
/// disc and the folders at the top of the disc.
pub fn library(folders: &[&str], wanted: fn(&str) -> bool, max: usize) -> Vec<String> {
    let home = fs::home(crate::users::current_name().unwrap_or_default().as_str());
    let mut dirs: Vec<String> = folders.iter().map(|f| fs::join(&home, f)).collect();
    // every disc and other disk, and the folders at their top
    for d in fs::drives() {
        if !d.ready || d.kind == fs::DriveKind::System {
            continue;
        }
        dirs.push(d.path.clone());
        if let Ok(items) = fs::list(&d.path) {
            for i in items.iter().filter(|i| i.dir) {
                dirs.push(fs::join(&d.path, &i.name));
            }
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(items) = fs::list(&dir) else { continue };
        for item in items {
            if !item.dir && wanted(&item.name) && out.len() < max {
                out.push(fs::join(&dir, &item.name));
            }
        }
    }
    out
}

struct Thumb {
    path: String,
    /// None until made; an empty image if it could not be read.
    image: Option<Image>,
}

pub struct Photos {
    /// The picture shown, or None for the gallery.
    path: Option<String>,
    image: Option<Image>,
    error: Option<String>,
    /// Pictures in the same folder, for Previous and Next.
    siblings: Vec<String>,
    /// Zoom in 1/100; 0 means fit to the window.
    zoom: i32,
    pan: (i32, i32),
    drag: Option<(i32, i32, i32, i32)>,
    pressed: Option<Button>,
    thumbs: Vec<Thumb>,
    scanned: bool,
    scroll: i32,
    dialog: Option<FileDialog>,
    /// Asks the desktop to open a picture in Draw.
    pub edit_request: Option<String>,
}

impl Photos {
    /// The window got a new size.
    pub fn resized(&mut self) {
        self.scroll = self.scroll.clamp(0, self.max_scroll());
    }

    pub fn new() -> Self {
        Self {
            path: None,
            image: None,
            error: None,
            siblings: Vec::new(),
            zoom: 0,
            pan: (0, 0),
            drag: None,
            pressed: None,
            thumbs: Vec::new(),
            scanned: false,
            scroll: 0,
            dialog: None,
            edit_request: None,
        }
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => format!("{} - Photos", fs::file_name(p)),
            None => String::from("Photos"),
        }
    }

    /// Show the gallery again, read afresh.
    pub fn show_gallery(&mut self) {
        self.path = None;
        self.image = None;
        self.error = None;
        self.scanned = false;
    }

    pub fn open_file(&mut self, path: &str) {
        self.path = Some(String::from(path));
        self.zoom = 0;
        self.pan = (0, 0);
        self.error = None;
        self.image = match fs::read(path) {
            Ok(data) => picture::decode(&data),
            Err(e) => {
                self.error = Some(String::from(e.message()));
                None
            }
        };
        if self.image.is_none() && self.error.is_none() {
            self.error = Some(String::from("This picture can't be read. Photos shows PNG, JPEG and BMP."));
        }
        let dir = fs::parent(path);
        self.siblings = fs::list(&dir)
            .map(|items| {
                items
                    .into_iter()
                    .filter(|i| !i.dir && picture::is_picture(&i.name))
                    .map(|i| fs::join(&dir, &i.name))
                    .collect()
            })
            .unwrap_or_default();
    }

    fn step(&mut self, by: i32) {
        let Some(path) = &self.path else { return };
        let n = self.siblings.len() as i32;
        if n == 0 {
            return;
        }
        let i = self
            .siblings
            .iter()
            .position(|p| fs::same_name(p, path))
            .unwrap_or(0) as i32;
        let next = self.siblings[((i + by).rem_euclid(n)) as usize].clone();
        self.open_file(&next);
    }

    /// The zoom in use, in 1/100.
    fn effective_zoom(&self) -> i32 {
        if self.zoom > 0 {
            return self.zoom;
        }
        let Some(img) = &self.image else { return 100 };
        let v = view_rect().inset(16);
        let z = (v.w * 100 / img.width.max(1) as i32).min(v.h * 100 / img.height.max(1) as i32);
        z.clamp(1, 100)
    }

    fn set_zoom(&mut self, z: i32) {
        self.zoom = z.clamp(5, 800);
    }

    fn rotate(&mut self) {
        let Some(img) = &self.image else { return };
        let (w, h) = (img.width, img.height);
        let mut out = alloc::vec![0u32; w * h];
        // clockwise: the old bottom row becomes the new left column
        for y in 0..h {
            for x in 0..w {
                out[x * h + (h - 1 - y)] = img.pixels[y * w + x];
            }
        }
        self.image = Some(Image {
            width: h,
            height: w,
            pixels: out,
        });
        self.pan = (0, 0);
    }

    fn press(&mut self, b: Button) {
        match b {
            Button::Gallery => self.show_gallery(),
            Button::Open => {
                let dir = self
                    .path
                    .as_deref()
                    .map(fs::parent)
                    .unwrap_or_else(|| fs::join(&fs::home(crate::users::current_name().unwrap_or_default().as_str()), "Pictures"));
                self.dialog = Some(FileDialog::new(Mode::Open, &dir, ""));
            }
            Button::Previous => self.step(-1),
            Button::Next => self.step(1),
            Button::ZoomOut => {
                let z = self.effective_zoom();
                self.set_zoom(z * 4 / 5);
            }
            Button::ZoomIn => {
                let z = self.effective_zoom();
                self.set_zoom(z * 5 / 4 + 1);
            }
            Button::Fit => {
                self.zoom = 0;
                self.pan = (0, 0);
            }
            Button::Rotate => self.rotate(),
            Button::Edit => {
                if let Some(p) = &self.path {
                    self.edit_request = Some(p.clone());
                }
            }
            Button::Background => {
                if let Some(p) = &self.path {
                    super::personalize::set_wallpaper(p);
                }
            }
        }
    }

    fn enabled(&self, b: Button) -> bool {
        match b {
            Button::Gallery | Button::Open => true,
            Button::Previous | Button::Next => self.path.is_some() && self.siblings.len() > 1,
            Button::Edit => {
                self.image.is_some() && self.path.as_deref().is_some_and(|p| !fs::is_on_disc(p))
            }
            _ => self.image.is_some(),
        }
    }

    // ---- background work --------------------------------------------------

    /// Find pictures for the gallery and make one thumbnail. Returns
    /// whether the window must be drawn again.
    pub fn tick(&mut self) -> bool {
        if self.path.is_some() {
            return false;
        }
        if !self.scanned {
            self.scanned = true;
            let paths = library(
                &["Pictures", "Downloads", "Desktop", "Documents"],
                picture::is_picture,
                MAX_ITEMS,
            );
            // keep thumbnails already made
            let mut old = core::mem::take(&mut self.thumbs);
            self.thumbs = paths
                .into_iter()
                .map(|path| {
                    let image = old
                        .iter_mut()
                        .find(|t| t.path == path)
                        .and_then(|t| t.image.take());
                    Thumb { path, image }
                })
                .collect();
            return true;
        }
        let Some(t) = self.thumbs.iter_mut().find(|t| t.image.is_none()) else {
            return false;
        };
        t.image = Some(
            fs::read(&t.path)
                .ok()
                .and_then(|d| picture::decode(&d))
                .map(|img| shrink(&img, THUMB_W as usize, THUMB_H as usize))
                .unwrap_or_else(Image::empty),
        );
        true
    }

    pub fn busy(&self) -> bool {
        self.path.is_none() && (!self.scanned || self.thumbs.iter().any(|t| t.image.is_none()))
    }

    // ---- input ----------------------------------------------------------------

    fn dialog_event(&mut self, event: filedialog::Event) -> bool {
        match event {
            filedialog::Event::None => false,
            filedialog::Event::Redraw => true,
            filedialog::Event::Cancel => {
                self.dialog = None;
                true
            }
            filedialog::Event::Chosen(path) => {
                self.dialog = None;
                self.open_file(&path);
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
            Key::Left | Key::PageUp => self.step(-1),
            Key::Right | Key::PageDown | Key::Char(' ') => self.step(1),
            Key::Char('+') | Key::Char('=') => self.press(Button::ZoomIn),
            Key::Char('-') => self.press(Button::ZoomOut),
            Key::Char('0') => self.press(Button::Fit),
            Key::Char('r') | Key::Char('R') | Key::Ctrl('r') => self.press(Button::Rotate),
            Key::Ctrl('o') => self.press(Button::Open),
            Key::Escape if self.path.is_some() => self.show_gallery(),
            Key::Up if self.path.is_none() => self.scroll_by(-CELL_H),
            Key::Down if self.path.is_none() => self.scroll_by(CELL_H),
            _ => return false,
        }
        true
    }

    fn max_scroll(&self) -> i32 {
        let per_row = ((cw() - 2 * GRID_X) / CELL_W).max(1) as usize;
        let rows = self.thumbs.len().div_ceil(per_row) as i32;
        (GRID_Y + rows * CELL_H + 20 - (ch() - STATUS_H)).max(0)
    }

    fn scroll_by(&mut self, dy: i32) {
        self.scroll = (self.scroll + dy).clamp(0, self.max_scroll());
    }

    pub fn on_wheel(&mut self, clicks: i32) -> bool {
        if let Some(d) = &mut self.dialog {
            return d.on_wheel(clicks, client());
        }
        if self.path.is_some() {
            if self.image.is_none() {
                return false;
            }
            let z = self.effective_zoom();
            self.set_zoom(if clicks < 0 { z * 5 / 4 + 1 } else { z * 4 / 5 });
        } else {
            self.scroll_by(clicks * 60);
        }
        true
    }

    fn thumb_rect(&self, i: usize) -> Rect {
        let per_row = ((cw() - 2 * GRID_X) / CELL_W).max(1) as usize;
        Rect::new(
            GRID_X + (i % per_row) as i32 * CELL_W,
            GRID_Y + (i / per_row) as i32 * CELL_H - self.scroll,
            CELL_W - 12,
            CELL_H - 8,
        )
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) -> bool {
        if let Some(d) = &mut self.dialog {
            if let MouseKind::Down { right: false } = ev.kind {
                let event = d.on_click(client(), ev.x, ev.y);
                return self.dialog_event(event);
            }
            return false;
        }
        match ev.kind {
            MouseKind::Down { right: false } => {
                for (b, _, r) in button_rects() {
                    if r.contains(ev.x, ev.y) && self.enabled(b) {
                        self.pressed = Some(b);
                        return true;
                    }
                }
                if self.path.is_none() {
                    let view = view_rect();
                    if !view.contains(ev.x, ev.y) {
                        return false;
                    }
                    for i in 0..self.thumbs.len() {
                        if self.thumb_rect(i).contains(ev.x, ev.y) {
                            // one click opens, like a gallery on a phone
                            let path = self.thumbs[i].path.clone();
                            self.open_file(&path);
                            return true;
                        }
                    }
                    return false;
                }
                if view_rect().contains(ev.x, ev.y) {
                    self.drag = Some((ev.x, ev.y, self.pan.0, self.pan.1));
                }
                false
            }
            MouseKind::Move => {
                if let Some((x0, y0, px, py)) = self.drag {
                    self.pan = (px + ev.x - x0, py + ev.y - y0);
                    return true;
                }
                false
            }
            MouseKind::Up => {
                self.drag = None;
                if let Some(b) = self.pressed.take() {
                    if button_rects()
                        .iter()
                        .any(|&(bb, _, r)| bb == b && r.contains(ev.x, ev.y))
                    {
                        self.press(b);
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    // ---- drawing --------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas) {
        // the toolbar
        c.fill_rect(0, 0, cw(), BAR_H, theme::face());
        c.fill_rect(0, BAR_H - 1, cw(), 1, theme::stroke());
        for (b, label, r) in button_rects() {
            if self.enabled(b) {
                theme::button(c, r, label, self.pressed == Some(b));
            } else {
                c.fill_round(r, theme::CONTROL_RADIUS, theme::face());
                c.outline_round(r, theme::CONTROL_RADIUS, theme::stroke());
                c.text_centered(r, label, mix(theme::text_dim(), theme::face(), 110));
            }
        }

        let view = view_rect();
        {
            let mut v = c.sub(view);
            v.fill_rect(0, 0, view.w, view.h, BACKDROP);
        }
        let status;
        if self.path.is_none() {
            self.draw_gallery(c, view);
            status = format!(
                "{} pictures in Pictures, Downloads, Desktop, Documents{}",
                self.thumbs.len(),
                if fs::disc_label().is_some() { " and on the disc" } else { "" }
            );
        } else if let Some(img) = &self.image {
            let z = self.effective_zoom();
            let w = img.width as i32 * z / 100;
            let h = img.height as i32 * z / 100;
            let x = view.x + (view.w - w) / 2 + self.pan.0;
            let y = view.y + (view.h - h) / 2 + self.pan.1;
            {
                let mut v = c.sub(view);
                draw_image(&mut v, img, Rect::new(x - view.x, y - view.y, w.max(1), h.max(1)));
            }
            let path = self.path.as_deref().unwrap_or("");
            let pos = self
                .siblings
                .iter()
                .position(|p| fs::same_name(p, path))
                .map(|i| format!("   {} of {}", i + 1, self.siblings.len()))
                .unwrap_or_default();
            status = format!(
                "{}   {} x {}   {}%{}",
                fs::display(path),
                img.width,
                img.height,
                z,
                pos
            );
        } else {
            let msg = self.error.clone().unwrap_or_default();
            c.text_centered(view, &msg, rgb(0xe0, 0xe0, 0xe6));
            status = String::from(fs::display(self.path.as_deref().unwrap_or("")));
        }

        c.fill_rect(0, ch() - STATUS_H, cw(), STATUS_H, theme::face());
        c.fill_rect(0, ch() - STATUS_H, cw(), 1, theme::stroke());
        c.draw_text(14, ch() - STATUS_H + 7, &status, theme::text_dim());

        if let Some(d) = &mut self.dialog {
            d.draw(c, client(), true);
        }
    }

    fn draw_gallery(&self, c: &mut Canvas, view: Rect) {
        let mut v = c.sub(view);
        let dy = view.y;
        v.draw_text_in(&HEADING, GRID_X, GRID_Y - 50 - dy - self.scroll, "Gallery", 0xffffff);
        if self.scanned && self.thumbs.is_empty() {
            v.text_centered(
                Rect::new(0, 0, view.w, view.h),
                "No pictures yet. Put PNG, JPEG or BMP files in Pictures, or insert a disc.",
                rgb(0xc8, 0xc8, 0xd0),
            );
            return;
        }
        for (i, t) in self.thumbs.iter().enumerate() {
            let r = self.thumb_rect(i).offset(0, -dy);
            if r.bottom() < 0 || r.y > view.h {
                continue;
            }
            v.fill_round(r, 10, rgb(0x26, 0x28, 0x2e));
            let pic = Rect::new(r.x + (r.w - THUMB_W) / 2, r.y + 6, THUMB_W, THUMB_H);
            match &t.image {
                Some(img) if img.width > 0 => {
                    let (w, h) = (img.width as i32, img.height as i32);
                    let x = pic.x + (pic.w - w) / 2;
                    let y = pic.y + (pic.h - h) / 2;
                    v.blit(x, y, w, h, &img.pixels, img.width);
                }
                Some(_) => v.text_centered(pic, "?", rgb(0x90, 0x90, 0x98)),
                None => v.fill_round(pic.inset(20), 8, rgb(0x30, 0x32, 0x3a)),
            }
            let name = fs::file_name(&t.path);
            let label = Rect::new(r.x + 4, r.bottom() - 26, r.w - 8, 22);
            let mut s = v.sub(label);
            s.draw_text_in(&UI_BOLD, 4, 2, &short(name, 24), rgb(0xe8, 0xe8, 0xee));
        }
    }
}

fn client() -> Rect {
    Rect::new(0, 0, cw(), ch())
}

fn short(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return String::from(name);
    }
    let mut s: String = name.chars().take(max - 3).collect();
    s.push_str("...");
    s
}

/// A copy that fits in `w` x `h`, keeping its shape, on an opaque
/// background.
pub fn shrink(img: &Image, w: usize, h: usize) -> Image {
    let scale_w = w * 1024 / img.width.max(1);
    let scale_h = h * 1024 / img.height.max(1);
    let scale = scale_w.min(scale_h).min(1024).max(1);
    let nw = (img.width * scale / 1024).max(1);
    let nh = (img.height * scale / 1024).max(1);
    let step = (img.width / nw).max(1);
    let mut pixels = alloc::vec![0u32; nw * nh];
    for y in 0..nh {
        let sy = y * img.height / nh;
        for x in 0..nw {
            let sx = x * img.width / nw;
            let p = img.sample(sx, sy, step);
            let a = p >> 24;
            pixels[y * nw + x] = if a == 255 {
                p & 0xff_ffff
            } else {
                mix(0x2a2c32, p & 0xff_ffff, a)
            };
        }
    }
    Image {
        width: nw,
        height: nh,
        pixels,
    }
}

/// Draw an image stretched to `r`, averaging pixels when it is made
/// smaller; see-through parts show a checkerboard.
pub fn draw_image(c: &mut Canvas, img: &Image, r: Rect) {
    if img.width == 0 || r.w <= 0 || r.h <= 0 {
        return;
    }
    let (iw, ih) = (img.width as i64, img.height as i64);
    let sx = iw * 1024 / r.w as i64;
    let sy = ih * 1024 / r.h as i64;
    let step = ((sx.max(sy) + 512) / 1024).max(1) as usize;
    let visible = r.intersect(&c.clip_rect());
    for py in visible.y..visible.bottom() {
        let src_y = (((py - r.y) as i64 * sy) / 1024).clamp(0, ih - 1) as usize;
        for px in visible.x..visible.right() {
            let src_x = (((px - r.x) as i64 * sx) / 1024).clamp(0, iw - 1) as usize;
            let p = img.sample(src_x, src_y, step);
            let a = p >> 24;
            if a == 255 {
                c.pixel(px, py, p & 0xff_ffff);
            } else {
                let check = if ((px - r.x) / 8 + (py - r.y) / 8) % 2 == 0 {
                    0xcccccc
                } else {
                    0xffffff
                };
                c.pixel(px, py, mix(check, p & 0xff_ffff, a));
            }
        }
    }
}
