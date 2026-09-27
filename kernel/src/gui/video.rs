//! Video Player: plays AVI files with Motion JPEG video (what cameras and
//! `ffmpeg -c:v mjpeg` make) and raw .mjpeg streams, from the hard disk
//! or from a CD or DVD. Every frame is a JPEG, decoded when it is due;
//! frames the computer is too slow for are skipped, so the video keeps
//! time. There is no sound yet.
//!
//! Without a video it shows the library: the videos in Videos,
//! Downloads and Desktop and on the disc.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::canvas::{mix, rgb, Canvas, Rect};
use super::filedialog::{self, FileDialog, Mode};
use super::picture::Image;
use super::text::{HEADING, UI_BOLD};
use super::{theme, MouseEvent, MouseKind};
use crate::keyboard::Key;
use crate::{fs, interrupts};

pub const CLIENT_W: i32 = 1024;
pub const CLIENT_H: i32 = 680;

const CONTROLS_H: i32 = 86;
const ROW_H: i32 = 56;
const LIST_Y: i32 = 110;
const MAX_ITEMS: usize = 60;

const BLACK: u32 = rgb(0x0c, 0x0c, 0x0e);

/// Whether a file name looks like a video the player can try.
pub fn is_video(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".avi", ".mjpg", ".mjpeg"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

// ---- files ----------------------------------------------------------------

/// A video read into memory: where each frame's JPEG is.
struct Movie {
    data: Vec<u8>,
    frames: Vec<(usize, usize)>,
    /// Microseconds each frame is shown.
    frame_us: u64,
    width: u32,
    height: u32,
}

impl Movie {
    fn duration_ms(&self) -> u64 {
        self.frames.len() as u64 * self.frame_us / 1000
    }

    fn frame(&self, i: usize) -> Option<Image> {
        let &(at, len) = self.frames.get(i)?;
        let jpeg = self.data.get(at..at + len)?;
        crate::web::image::decode(&with_huffman_tables(jpeg))
    }
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// Read an AVI (RIFF) file: the frame rate from the stream header and
/// the frames of the first video stream from the `movi` list.
fn parse_avi(data: Vec<u8>) -> Result<Movie, &'static str> {
    if data.get(0..4) != Some(b"RIFF") || data.get(8..12) != Some(b"AVI ") {
        return Err("This is not an AVI file.");
    }
    let mut m = Movie {
        data: Vec::new(),
        frames: Vec::new(),
        frame_us: 40_000,
        width: 0,
        height: 0,
    };
    let mut video_stream: Option<u32> = None;
    let mut streams = 0u32;
    let mut mjpeg = true;
    // (start, end) of lists still to walk
    let mut todo = alloc::vec![(12usize, data.len())];
    while let Some((mut at, end)) = todo.pop() {
        while at + 8 <= end {
            let id = &data[at..at + 4];
            let size = u32_at(&data, at + 4).unwrap_or(0) as usize;
            let body = at + 8;
            let body_end = (body + size).min(end);
            match id {
                b"LIST" if body + 4 <= body_end => {
                    let kind = &data[body..body + 4];
                    if kind == b"strl" {
                        streams += 1;
                    }
                    if matches!(kind, b"hdrl" | b"strl" | b"movi" | b"rec ") {
                        // walk it now, then go on after it
                        todo.push((body_end + (size & 1), end));
                        todo.push((body + 4, body_end));
                        break;
                    }
                }
                b"avih" => {
                    if let Some(us) = u32_at(&data, body) {
                        if us > 0 {
                            m.frame_us = us as u64;
                        }
                    }
                    m.width = u32_at(&data, body + 32).unwrap_or(0);
                    m.height = u32_at(&data, body + 36).unwrap_or(0);
                }
                b"strh" if data.get(body..body + 4) == Some(b"vids") && video_stream.is_none() => {
                    video_stream = Some(streams.saturating_sub(1));
                    let handler = data.get(body + 4..body + 8).unwrap_or(b"    ");
                    let handler = handler.to_ascii_uppercase();
                    if !(handler == *b"MJPG" || handler == *b"AVRN" || handler == *b"JPEG" || handler == *b"\0\0\0\0" || handler == *b"    ") {
                        mjpeg = false;
                    }
                    let scale = u32_at(&data, body + 20).unwrap_or(0) as u64;
                    let rate = u32_at(&data, body + 24).unwrap_or(0) as u64;
                    if scale > 0 && rate > 0 {
                        m.frame_us = 1_000_000 * scale / rate;
                    }
                }
                _ if size > 0 && (id[2..4] == *b"dc" || id[2..4] == *b"db") => {
                    let n = (id[0] as char).to_digit(10).zip((id[1] as char).to_digit(10));
                    if let Some((a, b)) = n {
                        if Some(a * 10 + b) == video_stream.or(Some(0)) && body_end > body {
                            m.frames.push((body, body_end - body));
                        }
                    }
                }
                _ => {}
            }
            at = body + size + (size & 1);
        }
    }
    if !mjpeg {
        return Err("This AVI uses a video format the player can't decode yet. It plays Motion JPEG (MJPG) video.");
    }
    // keep only frames that are JPEG pictures
    m.frames.retain(|&(at, len)| len > 4 && data[at] == 0xff && data[at + 1] == 0xd8);
    if m.frames.is_empty() {
        return Err("No Motion JPEG frames were found in this video.");
    }
    m.data = data;
    Ok(m)
}

/// A raw Motion JPEG stream: JPEG pictures one after another, shown at
/// 25 frames a second.
fn parse_mjpeg(data: Vec<u8>) -> Result<Movie, &'static str> {
    let mut frames = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] == 0xff {
            match data[i + 1] {
                0xd8 if start.is_none() => start = Some(i),
                0xd9 => {
                    if let Some(s) = start.take() {
                        frames.push((s, i + 2 - s));
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    if frames.is_empty() {
        return Err("No JPEG frames were found in this file.");
    }
    Ok(Movie {
        data,
        frames,
        frame_us: 40_000,
        width: 0,
        height: 0,
    })
}

/// Motion JPEG frames often leave out the Huffman tables and expect the
/// standard ones from the JPEG specification; put those in when missing.
fn with_huffman_tables(jpeg: &[u8]) -> Cow<'_, [u8]> {
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xff {
        let marker = jpeg[i + 1];
        if marker == 0xc4 {
            return Cow::Borrowed(jpeg);
        }
        if marker == 0xda {
            break;
        }
        let len = (jpeg[i + 2] as usize) << 8 | jpeg[i + 3] as usize;
        i += 2 + len;
    }
    let mut out = Vec::with_capacity(jpeg.len() + DEFAULT_DHT.len());
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&DEFAULT_DHT);
    out.extend_from_slice(&jpeg[2..]);
    Cow::Owned(out)
}

/// The standard luminance and chrominance tables (JPEG Annex K.3) as one
/// DHT segment.
const DEFAULT_DHT: [u8; 420] = {
    const DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    const DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    const DC_BITS_C: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
    const AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
    const AC_VALS: [u8; 162] = [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    const AC_BITS_C: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
    const AC_VALS_C: [u8; 162] = [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
        0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
        0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
        0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    let mut out = [0u8; 420];
    // marker and length: 2 + 4 tables of (1 + 16 + values)
    out[0] = 0xff;
    out[1] = 0xc4;
    out[2] = (418 >> 8) as u8;
    out[3] = (418 & 0xff) as u8;
    let mut at = 4;
    let tables: [(u8, &[u8; 16], &[u8]); 4] = [
        (0x00, &DC_BITS, &DC_VALS),
        (0x10, &AC_BITS, &AC_VALS),
        (0x01, &DC_BITS_C, &DC_VALS),
        (0x11, &AC_BITS_C, &AC_VALS_C),
    ];
    let mut t = 0;
    while t < 4 {
        let (class, bits, vals) = tables[t];
        out[at] = class;
        at += 1;
        let mut i = 0;
        while i < 16 {
            out[at] = bits[i];
            at += 1;
            i += 1;
        }
        let mut i = 0;
        while i < vals.len() {
            out[at] = vals[i];
            at += 1;
            i += 1;
        }
        t += 1;
    }
    out
};

// ---- the player -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Play,
    Library,
    Open,
    Previous,
    Next,
    Loop,
}

fn video_rect() -> Rect {
    Rect::new(0, 0, CLIENT_W, CLIENT_H - CONTROLS_H)
}

fn seek_rect() -> Rect {
    Rect::new(24, CLIENT_H - CONTROLS_H + 14, CLIENT_W - 48, 10)
}

fn buttons() -> [(Button, Rect); 6] {
    let y = CLIENT_H - CONTROLS_H + 36;
    [
        (Button::Play, Rect::new(CLIENT_W / 2 - 22, y - 2, 44, 44)),
        (Button::Previous, Rect::new(CLIENT_W / 2 - 22 - 60, y + 4, 44, 32)),
        (Button::Next, Rect::new(CLIENT_W / 2 + 22 + 16, y + 4, 44, 32)),
        (Button::Library, Rect::new(CLIENT_W - 24 - 92 - 8 - 92, y + 4, 92, 32)),
        (Button::Open, Rect::new(CLIENT_W - 24 - 92, y + 4, 92, 32)),
        (Button::Loop, Rect::new(CLIENT_W / 2 + 22 + 16 + 44 + 16, y + 4, 64, 32)),
    ]
}

fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

pub struct Video {
    path: Option<String>,
    movie: Option<Movie>,
    error: Option<String>,
    frame: Option<Image>,
    /// Index of the frame in `frame`.
    shown: usize,
    playing: bool,
    looping: bool,
    /// The tick the first frame would have been shown at.
    start: u64,
    pressed: Option<Button>,
    seeking: bool,
    library: Vec<(String, u32)>,
    scanned: bool,
    hover_row: Option<usize>,
    dialog: Option<FileDialog>,
    /// Frames shown and skipped since the video started, for the log.
    dropped: u32,
}

impl Video {
    pub fn new() -> Self {
        Self {
            path: None,
            movie: None,
            error: None,
            frame: None,
            shown: 0,
            playing: false,
            looping: false,
            start: 0,
            pressed: None,
            seeking: false,
            library: Vec::new(),
            scanned: false,
            hover_row: None,
            dialog: None,
            dropped: 0,
        }
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => format!("{} - Video Player", fs::file_name(p)),
            None => String::from("Video Player"),
        }
    }

    pub fn busy(&self) -> bool {
        self.playing
    }

    /// The window closed: stop playing.
    pub fn stop(&mut self) {
        self.playing = false;
    }

    fn show_library(&mut self) {
        self.playing = false;
        self.path = None;
        self.movie = None;
        self.frame = None;
        self.error = None;
        self.scanned = false;
    }

    pub fn open_file(&mut self, path: &str) {
        self.path = Some(String::from(path));
        self.movie = None;
        self.frame = None;
        self.error = None;
        self.playing = false;
        self.dropped = 0;
        let lower = path.to_ascii_lowercase();
        let parsed = match fs::read(path) {
            Ok(data) if lower.ends_with(".avi") => parse_avi(data),
            Ok(data) => parse_mjpeg(data),
            Err(e) => Err(e.message()),
        };
        match parsed {
            Ok(m) => {
                crate::serial::write_str("\nvideo: opened ");
                crate::serial::write_str(fs::file_name(path));
                crate::serial::write_str(&format!(", {} frames\n", m.frames.len()));
                self.frame = m.frame(0);
                self.shown = 0;
                self.movie = Some(m);
                self.play();
            }
            Err(e) => self.error = Some(String::from(e)),
        }
    }

    fn play(&mut self) {
        let Some(m) = &self.movie else { return };
        if self.shown + 1 >= m.frames.len() {
            self.shown = 0;
            self.frame = m.frame(0);
        }
        self.playing = true;
        self.start = interrupts::ticks() - self.ticks_for(self.shown);
    }

    fn ticks_for(&self, frame: usize) -> u64 {
        let us = self.movie.as_ref().map_or(40_000, |m| m.frame_us);
        frame as u64 * us * interrupts::TIMER_HZ / 1_000_000
    }

    fn seek_to(&mut self, frame: usize) {
        let Some(m) = &self.movie else { return };
        let frame = frame.min(m.frames.len() - 1);
        self.shown = frame;
        self.frame = m.frame(frame);
        if self.playing {
            self.start = interrupts::ticks() - self.ticks_for(frame);
        }
    }

    fn seek_by_ms(&mut self, ms: i64) {
        let Some(m) = &self.movie else { return };
        let frames = ms * 1000 / m.frame_us as i64;
        let to = (self.shown as i64 + frames).max(0) as usize;
        self.seek_to(to);
    }

    /// Show the frame that is due. Returns whether the picture changed.
    pub fn tick(&mut self) -> bool {
        if self.path.is_none() && !self.scanned {
            self.scanned = true;
            self.library = super::photos::library(
                &["Videos", "Downloads", "Desktop", "Documents"],
                is_video,
                MAX_ITEMS,
            )
            .into_iter()
            .map(|p| {
                let size = fs::list(&fs::parent(&p))
                    .ok()
                    .and_then(|items| {
                        items
                            .into_iter()
                            .find(|i| fs::same_name(&i.name, fs::file_name(&p)))
                    })
                    .map_or(0, |i| i.size);
                (p, size)
            })
            .collect();
            return true;
        }
        if !self.playing {
            return false;
        }
        let Some(m) = &self.movie else { return false };
        let elapsed = interrupts::ticks() - self.start;
        let due = (elapsed * 1_000_000 / interrupts::TIMER_HZ / m.frame_us) as usize;
        if due >= m.frames.len() {
            if self.looping {
                self.shown = 0;
                self.frame = m.frame(0);
                self.start = interrupts::ticks();
                return true;
            }
            self.playing = false;
            crate::serial::write_str(&format!(
                "\nvideo: finished, {} frames skipped\n",
                self.dropped
            ));
            return true;
        }
        if due == self.shown {
            return false;
        }
        self.dropped += (due - self.shown).saturating_sub(1) as u32;
        self.shown = due;
        if let Some(img) = m.frame(due) {
            self.frame = Some(img);
        }
        true
    }

    fn step(&mut self, by: i32) {
        let Some(path) = self.path.clone() else { return };
        let dir = fs::parent(&path);
        let Ok(items) = fs::list(&dir) else { return };
        let vids: Vec<String> = items
            .into_iter()
            .filter(|i| !i.dir && is_video(&i.name))
            .map(|i| fs::join(&dir, &i.name))
            .collect();
        let n = vids.len() as i32;
        if n < 2 {
            return;
        }
        let i = vids.iter().position(|p| fs::same_name(p, &path)).unwrap_or(0) as i32;
        let next = vids[((i + by).rem_euclid(n)) as usize].clone();
        self.open_file(&next);
    }

    fn press(&mut self, b: Button) {
        match b {
            Button::Play => {
                if self.playing {
                    self.playing = false;
                } else {
                    self.play();
                }
            }
            Button::Library => self.show_library(),
            Button::Open => {
                let user = crate::users::current_name().unwrap_or_default();
                let dir = self
                    .path
                    .as_deref()
                    .map(fs::parent)
                    .unwrap_or_else(|| fs::join(&fs::home(user.as_str()), "Videos"));
                self.dialog = Some(FileDialog::new(Mode::Open, &dir, ""));
            }
            Button::Previous => self.step(-1),
            Button::Next => self.step(1),
            Button::Loop => self.looping = !self.looping,
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
            Key::Char(' ') | Key::Char('k') | Key::Enter if self.movie.is_some() => {
                self.press(Button::Play)
            }
            Key::Left => self.seek_by_ms(-5000),
            Key::Right => self.seek_by_ms(5000),
            Key::Home => self.seek_to(0),
            Key::PageUp => self.step(-1),
            Key::PageDown => self.step(1),
            Key::Char('l') | Key::Char('L') => self.press(Button::Loop),
            Key::Ctrl('o') => self.press(Button::Open),
            Key::Escape if self.path.is_some() => self.show_library(),
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

    fn seek_at(&mut self, x: i32) {
        let Some(m) = &self.movie else { return };
        let r = seek_rect();
        let f = ((x - r.x).clamp(0, r.w) as usize * m.frames.len()) / r.w.max(1) as usize;
        self.seek_to(f);
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if self.path.is_some() || y < LIST_Y || x < 24 || x > CLIENT_W - 24 {
            return None;
        }
        let i = ((y - LIST_Y) / ROW_H) as usize;
        (i < self.library.len()).then_some(i)
    }

    pub fn on_hover(&mut self, x: i32, y: i32) -> bool {
        let row = self.row_at(x, y);
        core::mem::replace(&mut self.hover_row, row) != row
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
                for (b, r) in buttons() {
                    if r.contains(ev.x, ev.y) && self.enabled(b) {
                        self.pressed = Some(b);
                        return true;
                    }
                }
                if self.movie.is_some() && seek_rect().inset(-8).contains(ev.x, ev.y) {
                    self.seeking = true;
                    self.seek_at(ev.x);
                    return true;
                }
                if self.movie.is_some() && video_rect().contains(ev.x, ev.y) {
                    self.press(Button::Play);
                    return true;
                }
                if let Some(i) = self.row_at(ev.x, ev.y) {
                    let path = self.library[i].0.clone();
                    self.open_file(&path);
                    return true;
                }
                false
            }
            MouseKind::Move if self.seeking => {
                self.seek_at(ev.x);
                true
            }
            MouseKind::Up => {
                self.seeking = false;
                if let Some(b) = self.pressed.take() {
                    if buttons().iter().any(|&(bb, r)| bb == b && r.contains(ev.x, ev.y)) {
                        self.press(b);
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn enabled(&self, b: Button) -> bool {
        match b {
            Button::Library | Button::Open => true,
            Button::Play | Button::Loop => self.movie.is_some(),
            Button::Previous | Button::Next => self.path.is_some(),
        }
    }

    // ---- drawing --------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas) {
        let v = video_rect();
        c.fill(v, BLACK);
        if self.path.is_none() {
            self.draw_library(c);
        } else if let Some(img) = &self.frame {
            let (iw, ih) = (img.width as i32, img.height as i32);
            let scale_w = v.w * 1024 / iw.max(1);
            let scale_h = v.h * 1024 / ih.max(1);
            let s = scale_w.min(scale_h);
            let (w, h) = (iw * s / 1024, ih * s / 1024);
            let r = Rect::new(v.x + (v.w - w) / 2, v.y + (v.h - h) / 2, w, h);
            let mut sub = c.sub(v);
            blit_fit(&mut sub, img, r);
        } else if let Some(e) = &self.error {
            c.text_centered_in(&UI_BOLD, v.offset(0, -14), "This video can't be played", 0xffffff);
            c.text_centered(v.offset(0, 14), e, rgb(0xc8, 0xc8, 0xd0));
        }

        // the controls
        let bar = Rect::new(0, CLIENT_H - CONTROLS_H, CLIENT_W, CONTROLS_H);
        c.fill(bar, rgb(0x1c, 0x1d, 0x22));
        let seek = seek_rect();
        c.fill_round(seek, 5, rgb(0x3a, 0x3c, 0x44));
        let (pos_ms, total_ms) = match &self.movie {
            Some(m) => (self.shown as u64 * m.frame_us / 1000, m.duration_ms()),
            None => (0, 0),
        };
        if total_ms > 0 {
            let done = (seek.w as u64 * pos_ms / total_ms) as i32;
            c.fill_round(Rect::new(seek.x, seek.y, done.max(10), seek.h), 5, theme::accent());
            c.fill_circle(seek.x + done, seek.y + seek.h / 2, 8, 0xffffff);
        }
        let y = CLIENT_H - CONTROLS_H + 44;
        let time = format!("{} / {}", fmt_time(pos_ms), fmt_time(total_ms));
        c.draw_text(24, y + 4, &time, rgb(0xe0, 0xe0, 0xe6));
        for (b, r) in buttons() {
            let on = self.enabled(b);
            let fg = if on { 0xffffff } else { rgb(0x70, 0x70, 0x78) };
            let pressed = self.pressed == Some(b);
            match b {
                Button::Play => {
                    c.fill_round(r, r.w / 2, if pressed { mix(theme::accent(), 0, 60) } else { theme::accent() });
                    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
                    if self.playing {
                        c.fill_rect(cx - 7, cy - 9, 5, 18, 0xffffff);
                        c.fill_rect(cx + 2, cy - 9, 5, 18, 0xffffff);
                    } else {
                        c.fill_polygon(&[(cx - 5, cy - 10), (cx - 5, cy + 10), (cx + 10, cy)], 0xffffff);
                    }
                }
                Button::Previous | Button::Next => {
                    if pressed {
                        c.fill_round(r, 6, rgb(0x34, 0x36, 0x3e));
                    }
                    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
                    if b == Button::Previous {
                        c.fill_rect(cx - 9, cy - 7, 3, 14, fg);
                        c.fill_polygon(&[(cx + 7, cy - 7), (cx + 7, cy + 7), (cx - 5, cy)], fg);
                    } else {
                        c.fill_rect(cx + 7, cy - 7, 3, 14, fg);
                        c.fill_polygon(&[(cx - 7, cy - 7), (cx - 7, cy + 7), (cx + 5, cy)], fg);
                    }
                }
                _ => {
                    let lit = b == Button::Loop && self.looping;
                    let face = if lit {
                        theme::accent()
                    } else if pressed {
                        rgb(0x44, 0x46, 0x50)
                    } else {
                        rgb(0x2e, 0x30, 0x38)
                    };
                    c.fill_round(r, 6, face);
                    let label = match b {
                        Button::Library => "Library",
                        Button::Open => "Open...",
                        _ => "Loop",
                    };
                    c.text_centered(r, label, fg);
                }
            }
        }
        if let Some(d) = &mut self.dialog {
            d.draw(c, client(), true);
        }
    }

    fn draw_library(&self, c: &mut Canvas) {
        let v = video_rect();
        let mut s = c.sub(v);
        s.draw_text_in(&HEADING, 24, 22, "Videos", 0xffffff);
        let where_ = if fs::disc_label().is_some() {
            "In Videos, Downloads, Desktop, Documents and on the disc"
        } else {
            "In Videos, Downloads, Desktop and Documents. Insert a disc to see its videos too."
        };
        s.draw_text(24, 70, where_, rgb(0xa8, 0xa8, 0xb4));
        if self.scanned && self.library.is_empty() {
            let lines = [
                "No videos found.",
                "Video Player plays AVI files with Motion JPEG video and .mjpeg files.",
                "Make one from any video with ffmpeg:",
                "ffmpeg -i movie.mp4 -c:v mjpeg -q:v 5 -vf scale=640:-2 -an movie.avi",
            ];
            for (i, l) in lines.iter().enumerate() {
                s.draw_text(24, LIST_Y + 10 + i as i32 * 26, l, rgb(0xd0, 0xd0, 0xd8));
            }
            return;
        }
        for (i, (path, size)) in self.library.iter().enumerate() {
            let r = Rect::new(24, LIST_Y + i as i32 * ROW_H, v.w - 48, ROW_H - 6);
            if r.y > v.h {
                break;
            }
            let face = if self.hover_row == Some(i) {
                rgb(0x30, 0x32, 0x3a)
            } else {
                rgb(0x1e, 0x1f, 0x25)
            };
            s.fill_round(r, 8, face);
            // a film frame picture
            let icon = Rect::new(r.x + 12, r.y + 9, 44, 32);
            s.fill_round(icon, 5, theme::accent());
            let (cx, cy) = (icon.x + 22, icon.y + 16);
            s.fill_polygon(&[(cx - 5, cy - 8), (cx - 5, cy + 8), (cx + 8, cy)], 0xffffff);
            s.draw_text_in(&UI_BOLD, r.x + 72, r.y + 6, fs::file_name(path), 0xffffff);
            let place = if fs::is_on_disc(path) {
                format!("Disc  {}", fs::display(&fs::parent(path)))
            } else {
                fs::display(&fs::parent(path))
            };
            let info = format!("{}   {:.1} MB", place, *size as f32 / (1024.0 * 1024.0));
            s.draw_text(r.x + 72, r.y + 27, &info, rgb(0xa8, 0xa8, 0xb4));
        }
    }
}

fn client() -> Rect {
    Rect::new(0, 0, CLIENT_W, CLIENT_H)
}

/// Draw a video frame stretched to `r`: nearest pixels, which is fast.
fn blit_fit(c: &mut Canvas, img: &Image, r: Rect) {
    if img.width == 0 || r.w <= 0 || r.h <= 0 {
        return;
    }
    let visible = r.intersect(&c.clip_rect());
    let sx = (img.width << 16) / r.w as usize;
    let sy = (img.height << 16) / r.h as usize;
    let mut row = Vec::with_capacity(visible.w.max(0) as usize);
    for py in visible.y..visible.bottom() {
        let src_y = (((py - r.y) as usize * sy) >> 16).min(img.height - 1);
        let line = &img.pixels[src_y * img.width..(src_y + 1) * img.width];
        row.clear();
        for px in visible.x..visible.right() {
            let src_x = (((px - r.x) as usize * sx) >> 16).min(img.width - 1);
            row.push(line[src_x] & 0xff_ffff);
        }
        c.blit(visible.x, py, visible.w, 1, &row, row.len());
    }
}
