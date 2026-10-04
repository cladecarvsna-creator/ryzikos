//! The blue screen, and "why did my computer restart?" after it.
//!
//! When the kernel panics or the CPU raises an exception, the panic
//! handler writes a short report, paints the screen blue with what went
//! wrong, saves the report on the system disk and restarts. The next
//! sign-in opens a window that explains the report.
//!
//! The panic handler can't trust anything: the heap or the disk may be
//! locked by the code that stopped, or half way through a change. So:
//! - the report is formatted into a fixed buffer, never on the heap;
//! - it goes into a file made at boot, [`SLOT`], whose sectors are looked
//!   up at boot as well, and only those sectors are written, straight to
//!   the disk: the file system's tables are never touched while stopping;
//! - a panic inside the panic handler skips everything and restarts.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::console::{Color, CONSOLE};
use crate::framebuffer::{Framebuffer, Rgb};
use crate::gui::text::{Font, HEADING, UI, UI_BOLD};
use crate::port::{inb, outb};
use crate::{fs, interrupts, serial};

/// The file the report is written into. It always has [`SLOT_BYTES`].
pub const SLOT: &str = "/boot/crash-report.txt";
const SECTORS: usize = 8;
const SLOT_BYTES: usize = SECTORS * 512;
/// The first line of a report.
const MAGIC: &str = "RYZIKOS CRASH REPORT";

/// Seconds the blue screen stays before restarting.
const RESTART_AFTER: u32 = 15;

/// Where the report's sectors are on the disk, found at boot.
static LBAS: [AtomicU64; SECTORS] = [const { AtomicU64::new(0) }; SECTORS];
static READY: AtomicBool = AtomicBool::new(false);
/// A hash of what the report file held at boot. Before writing, the
/// panic handler reads the sectors back and checks it: if the file was
/// deleted and its space given to another file, nothing is written.
static SLOT_HASH: AtomicU64 = AtomicU64::new(0);
/// A report from the last run that nobody has seen yet.
static PENDING: AtomicBool = AtomicBool::new(false);

/// 0 while running, 1 once the panic handler has started.
static STATE: AtomicU8 = AtomicU8::new(0);
/// The `panic` shell command asked for this crash.
static MANUAL: AtomicBool = AtomicBool::new(false);
/// A CPU exception that led to the panic: vector + 1 (0 for none), the
/// instruction, the address of a page fault and the error code.
static EXC_VECTOR: AtomicU64 = AtomicU64::new(0);
static EXC_RIP: AtomicU64 = AtomicU64::new(0);
static EXC_ADDRESS: AtomicU64 = AtomicU64::new(0);
static EXC_ERROR: AtomicU64 = AtomicU64::new(0);
/// The title of the window in front, kept as a pointer and length so
/// the panic handler can read it without a lock.
static APP_PTR: AtomicUsize = AtomicUsize::new(0);
static APP_LEN: AtomicUsize = AtomicUsize::new(0);

/// The report being built while stopping, and the file's sectors read
/// back before it is written.
static mut REPORT: [u8; SLOT_BYTES] = [0; SLOT_BYTES];
static mut CHECK: [u8; SLOT_BYTES] = [0; SLOT_BYTES];

/// FNV-1a.
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

// ---- while running --------------------------------------------------------

/// Note which window is in front, for the report.
pub fn set_app(title: &'static str) {
    APP_PTR.store(title.as_ptr() as usize, Ordering::Relaxed);
    APP_LEN.store(title.len(), Ordering::Relaxed);
}

fn app() -> &'static str {
    let (ptr, len) = (
        APP_PTR.load(Ordering::Relaxed),
        APP_LEN.load(Ordering::Relaxed),
    );
    if ptr == 0 {
        return "";
    }
    // set_app only ever stores a whole &'static str
    unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(ptr as *const u8, len)) }
}

/// The next panic was asked for from the shell.
pub fn manual() {
    MANUAL.store(true, Ordering::Relaxed);
}

/// A CPU exception is about to panic.
pub fn exception(vector: u64, rip: u64, address: u64, error: u64) {
    EXC_RIP.store(rip, Ordering::Relaxed);
    EXC_ADDRESS.store(address, Ordering::Relaxed);
    EXC_ERROR.store(error, Ordering::Relaxed);
    EXC_VECTOR.store(vector + 1, Ordering::Relaxed);
}

/// At boot, after the disks: pick up a report the last run left, then
/// get the report file ready and note where its sectors are.
pub fn init() {
    if fs::storage() != fs::Storage::Disk {
        serial::write_str("crash: no system disk, a crash report can't be kept\n");
        return;
    }
    let mut slot = fs::read(SLOT).ok().filter(|b| b.len() == SLOT_BYTES);
    if let Some(bytes) = &mut slot {
        if let Some(report) = Report::parse(bytes) {
            if report.get("new") == "1" {
                serial::write_str("crash: the last run stopped with ");
                serial::write_str(report.get("code"));
                serial::write_str("\n");
                PENDING.store(true, Ordering::Relaxed);
                // seen: "new: 1" becomes "new: 0", same size
                if let Some(i) = find(bytes, b"\nnew: 1") {
                    bytes[i + 6] = b'0';
                }
                if fs::write(SLOT, bytes).is_err() {
                    serial::write_str("crash: could not mark the report as seen\n");
                    return;
                }
            }
        }
    }
    let bytes = match slot {
        Some(b) => b,
        None => {
            let zeros = alloc::vec![0u8; SLOT_BYTES];
            if fs::write(SLOT, &zeros).is_err() {
                serial::write_str("crash: could not make the report file\n");
                return;
            }
            zeros
        }
    };
    SLOT_HASH.store(hash(&bytes), Ordering::Relaxed);
    match fs::file_sectors(SLOT, SECTORS) {
        Ok(lbas) => {
            for (slot, lba) in LBAS.iter().zip(lbas) {
                slot.store(lba, Ordering::Relaxed);
            }
            READY.store(true, Ordering::Relaxed);
            serial::write_str("crash: ready to keep a report\n");
        }
        Err(_) => serial::write_str("crash: the report file can't be found on the disk\n"),
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Whether the last run crashed and this is the first boot since.
/// Asking forgets it, so the window opens once.
pub fn take_pending() -> bool {
    PENDING.swap(false, Ordering::Relaxed)
}

/// A report read back from the disk: `key: value` lines.
pub struct Report {
    fields: Vec<(String, String)>,
}

impl Report {
    fn parse(bytes: &[u8]) -> Option<Report> {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let text = core::str::from_utf8(&bytes[..end]).ok()?;
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        let fields = lines
            .filter_map(|l| l.split_once(": "))
            .map(|(k, v)| (String::from(k), String::from(v)))
            .collect();
        Some(Report { fields })
    }

    /// The last report on the system disk, if there ever was one.
    pub fn last() -> Option<Report> {
        Report::parse(&fs::read(SLOT).ok()?)
    }

    pub fn get(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map_or("", |(_, v)| v.as_str())
    }

    /// The whole report as text, for copying and for the shell.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (key, label) in DETAILS {
            let v = self.get(key);
            if !v.is_empty() {
                let _ = writeln!(out, "{}: {}", label, v);
            }
        }
        out
    }
}

/// The report's fields in the order people read them, with their names.
pub const DETAILS: [(&str, &str); 9] = [
    ("code", "Error code"),
    ("message", "Message"),
    ("where", "Source"),
    ("cpu", "Processor"),
    ("app", "Window in front"),
    ("date", "Date"),
    ("time", "Time"),
    ("uptime", "Running for"),
    ("version", "RyzikOS version"),
];

/// What an error code means, for people.
pub fn explain(code: &str) -> &'static str {
    match code {
        "PANIC_REQUESTED" => {
            "The panic command in Terminal stopped the system on purpose, to try this screen. Nothing is wrong with the computer."
        }
        "OUT_OF_MEMORY" => "The system ran out of memory: something asked for more than the computer has.",
        "BAD_MEMORY_ACCESS" => {
            "The kernel reached for memory that isn't there. This is a bug in RyzikOS, not a broken computer."
        }
        "DIVIDE_BY_ZERO" => "The kernel tried to divide by zero. This is a bug in RyzikOS.",
        "BAD_INSTRUCTION" => {
            "The processor met an instruction it doesn't know. This is a kernel bug, or a very old processor."
        }
        "PROTECTION_FAULT" => "The kernel did something the processor doesn't allow. This is a bug in RyzikOS.",
        "DOUBLE_FAULT" => "A second error happened while handling the first one, and the kernel couldn't go on.",
        "CPU_EXCEPTION" => "The processor reported an error the kernel doesn't know how to handle.",
        _ => "Something unexpected went wrong inside the RyzikOS kernel. It stopped to keep your files safe.",
    }
}

/// What to do about it.
pub fn advice(code: &str) -> &'static str {
    match code {
        "PANIC_REQUESTED" => "Nothing to do.",
        "OUT_OF_MEMORY" => {
            "Close windows and tabs you don't need. In a virtual machine, give RyzikOS more memory (for example -m 1G in QEMU)."
        }
        _ => {
            "If it happens again, press Copy and send the report to the developer: it shows where the bug is. Your files on the disk are fine."
        }
    }
}

// ---- stopping -----------------------------------------------------------

/// A writer into a fixed buffer that drops what doesn't fit and never
/// cuts a character in half.
struct Fixed<'a> {
    buf: &'a mut [u8],
    len: usize,
    /// Newlines become spaces, for one-line fields.
    one_line: bool,
}

impl Fixed<'_> {
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Write for Fixed<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            let c = if self.one_line && (c == '\n' || c == '\r') {
                ' '
            } else {
                c
            };
            let mut tmp = [0u8; 4];
            let bytes = c.encode_utf8(&mut tmp).as_bytes();
            if self.len + bytes.len() > self.buf.len() {
                break;
            }
            self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
            self.len += bytes.len();
        }
        Ok(())
    }
}

/// A short name for what caused the panic.
fn stop_code(message: &str) -> &'static str {
    if MANUAL.load(Ordering::Relaxed) {
        return "PANIC_REQUESTED";
    }
    match EXC_VECTOR.load(Ordering::Relaxed) {
        0 => {}
        v => {
            return match v - 1 {
                0 => "DIVIDE_BY_ZERO",
                6 => "BAD_INSTRUCTION",
                8 => "DOUBLE_FAULT",
                13 => "PROTECTION_FAULT",
                14 => "BAD_MEMORY_ACCESS",
                _ => "CPU_EXCEPTION",
            }
        }
    }
    if message.starts_with("memory allocation of") {
        return "OUT_OF_MEMORY";
    }
    "KERNEL_PANIC"
}

/// Called by the kernel's `#[panic_handler]`. Never returns: the
/// computer restarts.
pub fn on_panic(info: &PanicInfo) -> ! {
    interrupts::disable();
    if STATE.swap(1, Ordering::SeqCst) != 0 {
        // the panic handler itself failed: no screen, no report
        serial::write_str("\ncrash: panic while stopping, restarting\n");
        wait_ms(3000, false);
        restart();
    }

    let mut message_buf = [0u8; 600];
    let mut message = Fixed {
        buf: &mut message_buf,
        len: 0,
        one_line: true,
    };
    let _ = write!(message, "{}", info.message());
    let message = message.as_str();
    let code = stop_code(message);

    let mut where_buf = [0u8; 160];
    let mut place = Fixed {
        buf: &mut where_buf,
        len: 0,
        one_line: true,
    };
    if let Some(l) = info.location() {
        let _ = write!(place, "{}:{}", l.file(), l.line());
    }
    let place = place.as_str();

    // the report, in the fixed buffer
    let report = unsafe { &mut *core::ptr::addr_of_mut!(REPORT) };
    report.fill(0);
    let mut w = Fixed {
        buf: &mut report[..],
        len: 0,
        one_line: false,
    };
    let ((year, month, day), (h, m, s)) = crate::clock::now();
    let up = interrupts::ticks() / interrupts::TIMER_HZ;
    let _ = writeln!(w, "{}", MAGIC);
    let _ = writeln!(w, "new: 1");
    let _ = writeln!(w, "code: {}", code);
    let _ = writeln!(w, "message: {}", message);
    let _ = writeln!(w, "where: {}", place);
    if let v @ 1.. = EXC_VECTOR.load(Ordering::Relaxed) {
        let _ = write!(
            w,
            "cpu: vector {}, rip={:#x}, error={:#x}",
            v - 1,
            EXC_RIP.load(Ordering::Relaxed),
            EXC_ERROR.load(Ordering::Relaxed)
        );
        if v - 1 == 14 {
            let _ = write!(w, ", address={:#x}", EXC_ADDRESS.load(Ordering::Relaxed));
        }
        let _ = writeln!(w);
    }
    let app = app();
    if !app.is_empty() {
        let _ = writeln!(w, "app: {}", app);
    }
    let _ = writeln!(w, "date: {:02}.{:02}.{}", day, month, year);
    let _ = writeln!(w, "time: {:02}:{:02}:{:02}", h, m, s);
    let _ = writeln!(
        w,
        "uptime: {}:{:02}:{:02}",
        up / 3600,
        up / 60 % 60,
        up % 60
    );
    let build = crate::update::BUILD;
    if build == 0 {
        let _ = writeln!(w, "version: {} (built from source)", crate::gui::VERSION);
    } else {
        let _ = writeln!(w, "version: {}.{}", crate::gui::VERSION, build);
    }

    serial::write_str("\nKERNEL PANIC: ");
    serial::write_str(message);
    serial::write_str("\nbsod: ");
    serial::write_str(code);
    serial::write_str("\n");
    serial::write_str(w.as_str());

    let mut screen = Screen::open();
    screen.draw(code, message, place, app);

    let saved = if READY.load(Ordering::Relaxed) {
        let mut lbas = [0u64; SECTORS];
        for (l, a) in lbas.iter_mut().zip(LBAS.iter()) {
            *l = a.load(Ordering::Relaxed);
        }
        let check = unsafe { &mut *core::ptr::addr_of_mut!(CHECK) };
        let same = unsafe { fs::panic_read(&lbas, check) }
            && hash(check) == SLOT_HASH.load(Ordering::Relaxed);
        if !same {
            serial::write_str("crash: the report file was changed or deleted\n");
        }
        same && unsafe { fs::panic_write(&lbas, &report[..]) }
    } else {
        false
    };
    serial::write_str(if saved {
        "crash: report saved to the disk\n"
    } else {
        "crash: report not saved\n"
    });
    screen.saved(saved);

    // restart after a while, or at once on a key
    wait_ms(1000, false);
    for left in (1..=RESTART_AFTER).rev() {
        screen.countdown(left);
        if wait_ms(1000, true) {
            break;
        }
    }
    serial::write_str("crash: restarting\n");
    restart()
}

/// Restart the computer: the PS/2 controller's reset line, or a triple
/// fault if that does nothing.
fn restart() -> ! {
    unsafe {
        outb(0x64, 0xfe);
    }
    wait_ms(500, false);
    #[repr(C, packed)]
    struct Empty {
        limit: u16,
        base: u64,
    }
    let empty = Empty { limit: 0, base: 0 };
    unsafe { core::arch::asm!("lidt [{}]", "int3", in(reg) &empty, options(nostack)) };
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}

/// Wait with interrupts off, counting the PIT's 1 ms periods. With `key`
/// set, a key press ends the wait early and returns true.
fn wait_ms(ms: u32, key: bool) -> bool {
    unsafe {
        // channel 0, rate generator, 1193 counts = 1 ms
        outb(0x43, 0x34);
        outb(0x40, 1193u16 as u8);
        outb(0x40, (1193u16 >> 8) as u8);
    }
    let count = || unsafe {
        outb(0x43, 0x00); // latch channel 0
        let lo = inb(0x40) as u16;
        let hi = inb(0x40) as u16;
        hi << 8 | lo
    };
    let mut last = count();
    let mut done = 0;
    while done < ms {
        let now = count();
        if now > last {
            done += 1; // it counted down to 0 and reloaded
        }
        last = now;
        if key {
            let status = unsafe { inb(0x64) };
            if status & 1 != 0 {
                let byte = unsafe { inb(0x60) };
                // a key going down, not the mouse and not a release
                if status & 0x20 == 0 && byte < 0x80 {
                    return true;
                }
            }
        }
    }
    false
}

// ---- the blue screen --------------------------------------------------------

/// Deep blue page, a lighter panel for the details, white text.
const PAGE: Rgb = Rgb::new(0x0d, 0x26, 0x63);
const PANEL: Rgb = Rgb::new(0x17, 0x3a, 0x8a);
const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);
const PALE: Rgb = Rgb::new(0xb8, 0xca, 0xf0);

/// Where the blue screen is drawn: the framebuffer, or the text console
/// without one.
enum Screen {
    Pixels(Layout),
    Text,
}

#[derive(Clone, Copy)]
struct Layout {
    fb: Framebuffer,
    /// Text size in 1/16: 16 is the fonts' own size.
    s: i32,
    /// The centre of the column and its width.
    cx: usize,
    width: usize,
    /// Where the "saved" line and the restart bar go.
    status_y: usize,
    bar_y: usize,
}

impl Screen {
    fn open() -> Screen {
        // whoever held the console will never run again
        unsafe { CONSOLE.force_unlock() };
        let fb = CONSOLE.lock().framebuffer();
        match fb {
            Some(fb) => {
                // 16 at 1280x720 and below, 24 at 1920x1080
                let s = (fb.height.min(fb.width * 9 / 16) as i32 * 16 / 720).clamp(12, 40);
                let width = (fb.width * 3 / 5).min(fb.width - 32);
                Screen::Pixels(Layout {
                    fb,
                    s,
                    cx: fb.width / 2,
                    width,
                    status_y: 0,
                    bar_y: 0,
                })
            }
            None => Screen::Text,
        }
    }

    fn draw(&mut self, code: &str, message: &str, place: &str, app: &str) {
        match self {
            Screen::Pixels(l) => (l.status_y, l.bar_y) = l.draw(code, message, place, app),
            Screen::Text => {
                let mut con = CONSOLE.lock();
                con.reattach();
                con.set_color(Color::White, Color::Blue);
                con.clear();
                let _ = writeln!(
                    con,
                    "\n  RyzikOS has stopped because of an error and will restart.\n"
                );
                let _ = writeln!(con, "  Error code: {}", code);
                let _ = writeln!(con, "  {}", message);
                if !place.is_empty() {
                    let _ = writeln!(con, "  at {}", place);
                }
            }
        }
    }

    fn saved(&self, saved: bool) {
        let text = if saved {
            "A report is saved. After the restart, RyzikOS will tell you what happened."
        } else {
            "The report could not be saved on a disk."
        };
        match self {
            Screen::Pixels(l) => {
                let scale = l.s * 5 / 4;
                l.fb.fill_rect(0, l.status_y, l.fb.width, l.line_h(&UI, scale), PAGE);
                l.centered(l.status_y, text, &UI, scale, PALE, PAGE);
            }
            Screen::Text => {
                let mut con = CONSOLE.lock();
                let _ = writeln!(con, "\n  {}", text);
            }
        }
    }

    fn countdown(&self, left: u32) {
        match self {
            Screen::Pixels(l) => {
                let fb = &l.fb;
                let h = (6 * l.s / 16).max(3) as usize;
                let x = l.cx - l.width / 2;
                fb.fill_rect(x, l.bar_y, l.width, h, PANEL);
                fb.fill_rect(
                    x,
                    l.bar_y,
                    l.width * left as usize / RESTART_AFTER as usize,
                    h,
                    WHITE,
                );
                let mut buf = [0u8; 120];
                let mut t = Fixed {
                    buf: &mut buf,
                    len: 0,
                    one_line: true,
                };
                let _ = write!(
                    t,
                    "Restarting in {} s  ·  press any key to restart now",
                    left
                );
                let scale = l.s * 5 / 4;
                let y = l.bar_y + h + (14 * l.s / 16) as usize;
                fb.fill_rect(0, y, fb.width, l.line_h(&UI, scale), PAGE);
                l.centered(y, t.as_str(), &UI, scale, PALE, PAGE);
            }
            Screen::Text => {
                let mut con = CONSOLE.lock();
                let _ = write!(con, "\r  Restarting in {:2} s, or press a key.", left);
            }
        }
    }
}

impl Layout {
    fn line_h(&self, font: &Font, scale: i32) -> usize {
        (font.line_height * scale / 16) as usize + 4 * self.s as usize / 16
    }

    fn width_of(font: &Font, scale: i32, text: &str) -> usize {
        let sixteenths: i32 = text.chars().map(|c| font.advance16(c) as i32).sum();
        (sixteenths * scale / 256) as usize
    }

    /// Smooth text from the desktop's fonts, `scale`/16 of their size,
    /// blended over `bg`. The glyphs are static data, so this needs no
    /// heap.
    #[allow(clippy::too_many_arguments)]
    fn text(&self, x: usize, y: usize, text: &str, font: &Font, scale: i32, color: Rgb, bg: Rgb) {
        let mut pen = x as i32 * 16;
        for ch in text.chars() {
            let Some(g) = font.glyph(ch).or_else(|| font.glyph('?')) else {
                continue;
            };
            let gx = (pen + 8) / 16 + g.x as i32 * scale / 16;
            let gy = y as i32 + g.y as i32 * scale / 16;
            self.glyph(
                gx,
                gy,
                (g.w as i32, g.h as i32),
                font.coverage(g),
                scale,
                color,
                bg,
            );
            pen += g.advance as i32 * scale / 16;
        }
    }

    fn centered(&self, y: usize, text: &str, font: &Font, scale: i32, color: Rgb, bg: Rgb) {
        let w = Self::width_of(font, scale, text);
        self.text(
            self.cx.saturating_sub(w / 2),
            y,
            text,
            font,
            scale,
            color,
            bg,
        );
    }

    /// One coverage map, resized with bilinear sampling.
    #[allow(clippy::too_many_arguments)]
    fn glyph(
        &self,
        x: i32,
        y: i32,
        (w, h): (i32, i32),
        cov: &[u8],
        scale: i32,
        color: Rgb,
        bg: Rgb,
    ) {
        let at = |cx: i32, cy: i32| -> i32 {
            if cx < 0 || cy < 0 || cx >= w || cy >= h {
                0
            } else {
                cov[(cy * w + cx) as usize] as i32
            }
        };
        let (tw, th) = ((w * scale + 15) / 16 + 1, (h * scale + 15) / 16 + 1);
        for ty in 0..th {
            for tx in 0..tw {
                // the source point under this pixel's centre, in 1/256
                let sx = ((tx * 2 + 1) * 16 * 128) / scale - 128;
                let sy = ((ty * 2 + 1) * 16 * 128) / scale - 128;
                let (x0, y0) = (sx.div_euclid(256), sy.div_euclid(256));
                let (fx, fy) = (sx.rem_euclid(256), sy.rem_euclid(256));
                let top = at(x0, y0) * (256 - fx) + at(x0 + 1, y0) * fx;
                let bottom = at(x0, y0 + 1) * (256 - fx) + at(x0 + 1, y0 + 1) * fx;
                let a = (top * (256 - fy) + bottom * fy) >> 16;
                if a > 0 && x + tx >= 0 && y + ty >= 0 {
                    let pixel = bg.mix(color, a.min(255) as u8);
                    self.fb
                        .put_raw((x + tx) as usize, (y + ty) as usize, self.fb.encode(pixel));
                }
            }
        }
    }

    /// Word-wrapped lines of `text` that fit `width`, at most `max`,
    /// each drawn by `draw(y, line)`; returns the y below them.
    #[allow(clippy::too_many_arguments)]
    fn wrap(
        &self,
        mut y: usize,
        text: &str,
        font: &Font,
        scale: i32,
        width: usize,
        max: usize,
        mut draw: impl FnMut(usize, &str),
    ) -> usize {
        let mut rest = text.trim();
        let mut lines = 0;
        while !rest.is_empty() && lines < max {
            let mut cut = rest.len();
            if Self::width_of(font, scale, rest) > width {
                let mut fit = 0;
                let mut last_space = None;
                for (i, c) in rest.char_indices() {
                    if Self::width_of(font, scale, &rest[..i + c.len_utf8()]) > width {
                        break;
                    }
                    fit = i + c.len_utf8();
                    if c == ' ' {
                        last_space = Some(i);
                    }
                }
                cut = last_space.filter(|&i| i > 0).unwrap_or(fit.max(1));
            }
            draw(y, rest[..cut].trim_end());
            rest = rest[cut..].trim_start();
            y += self.line_h(font, scale);
            lines += 1;
        }
        y
    }

    /// Paint the whole screen; returns the rows for the "saved" line and
    /// the restart bar.
    fn draw(&self, code: &str, message: &str, place: &str, app: &str) -> (usize, usize) {
        let fb = &self.fb;
        let s = self.s;
        let px = |n: i32| (n * s / 16) as usize;
        fb.fill_rect(0, 0, fb.width, fb.height, PAGE);

        // a ring with an exclamation mark
        let r = px(34) as isize;
        let mut y = fb.height / 9;
        let cy = y as isize + r;
        fb.fill_circle(self.cx as isize, cy, r, WHITE);
        fb.fill_circle(self.cx as isize, cy, r - px(5).max(2) as isize, PAGE);
        let bar = px(7).max(3);
        fb.fill_rect(self.cx - bar / 2, (cy - r / 2) as usize, bar, r as usize * 5 / 8, WHITE);
        fb.fill_circle(self.cx as isize, cy + r / 2 - bar as isize / 2, bar as isize * 3 / 5, WHITE);
        y += 2 * r as usize + px(28);

        let title = s * 3 / 2;
        self.centered(y, "RyzikOS has stopped", &HEADING, title, WHITE, PAGE);
        y += self.line_h(&HEADING, title) + px(8);
        let body = s * 3 / 2;
        y = self.wrap(y, explain(code), &UI, body, self.width, 3, |y, line| {
            self.centered(y, line, &UI, body, WHITE, PAGE)
        });
        y += px(26);

        // the details, on a panel
        let small = s * 5 / 4;
        let pad = px(22);
        let left = self.cx - self.width / 2;
        let label_w = Self::width_of(&UI_BOLD, small, "Window in front") + px(24);
        let value_w = self.width - 2 * pad - label_w;
        let rows: [(&str, &str, usize); 4] = [
            ("Error code", code, 1),
            ("Message", message, 3),
            ("Source", place, 1),
            ("Window in front", app, 1),
        ];
        let mut h = 2 * pad;
        for (_, value, max) in rows {
            if !value.is_empty() {
                h += self.wrap(0, value, &UI, small, value_w, max, |_, _| {}) + px(6);
            }
        }
        fb.fill_rect(left, y, self.width, h, PANEL);
        fb.fill_rect(left, y, px(4).max(2), h, WHITE);
        let mut ry = y + pad;
        for (label, value, max) in rows {
            if value.is_empty() {
                continue;
            }
            self.text(left + pad, ry, label, &UI_BOLD, small, PALE, PANEL);
            let bold = label == "Error code";
            ry = self.wrap(ry, value, &UI, small, value_w, max, |y, line| {
                let font = if bold { &UI_BOLD } else { &UI };
                self.text(left + pad + label_w, y, line, font, small, WHITE, PANEL)
            }) + px(6);
        }
        y += h + px(26);

        let status_y = y;
        self.centered(y, "Saving a report...", &UI, small, PALE, PAGE);
        y += self.line_h(&UI, small) + px(22);
        (status_y, y.min(fb.height.saturating_sub(px(60))))
    }
}
