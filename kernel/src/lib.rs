//! EverOS kernel. `kernel_main` is called from `boot/long_mode.asm`
//! once the CPU is in 64-bit long mode.

#![no_std]

extern crate alloc;

mod console;
mod fiber;
mod font;
mod framebuffer;
mod fs;
mod gui;
mod heap;
mod interrupts;
mod js;
mod keyboard;
mod multiboot;
mod net;
mod pci;
mod port;
mod ps2;
mod rtc;
mod serial;
mod shell;
mod sync;
mod users;
mod vga;
mod vmmouse;
mod web;

use core::fmt::Write;
use core::panic::PanicInfo;

use console::{Color, CONSOLE};
use interrupts::{KEYBOARD_BYTES, MOUSE_BYTES};

/// Cursor blink period in timer ticks.
const BLINK_TICKS: u64 = interrupts::TIMER_HZ / 2;

#[no_mangle]
pub extern "C" fn kernel_main(multiboot_info: usize) -> ! {
    serial::init();
    heap::init();
    users::init();
    let boot = unsafe { multiboot::parse(multiboot_info) };
    CONSOLE.lock().init(boot.framebuffer);

    console::print_colored(Color::LightCyan, format_args!("RyzikOS {}", gui::VERSION));
    println!(" - a hobby operating system in ASM and Rust");
    println!();

    interrupts::init();
    let mouse = ps2::init();
    interrupts::enable();
    fs::init();

    match &boot.framebuffer {
        Some(fb) => println!("Graphics:  {}x{} framebuffer", fb.width, fb.height),
        None => println!("Graphics:  none, using VGA text mode"),
    }
    println!("Keyboard:  ready (Alt+Shift switches EN/RU)");
    println!("Mouse:     {}", if mouse { "ready" } else { "not found" });
    match fs::storage() {
        fs::Storage::Disk => println!(
            "Disk:      {} MiB FAT32, files are saved on it",
            fs::capacity() / (1024 * 1024)
        ),
        _ => println!("Disk:      none, files are kept in memory until restart"),
    }
    match fs::disc_label() {
        Some(label) => println!("Disc:      {} in the drive, its files are at /Disc", label),
        None => println!("Disc:      none"),
    }
    println!("Привет! Кириллица тоже работает.");
    println!();
    println!("EverOS: kernel started");
    println!("Type 'help' for a list of commands.");
    println!();

    match boot.framebuffer {
        Some(fb) => gui::run(fb, &boot),
        None => run(&boot),
    }
}

/// The greeting at the top of the terminal.
fn print_banner() {
    console::print_colored(Color::LightCyan, format_args!("RyzikOS {}", gui::VERSION));
    println!(" - a hobby operating system in ASM and Rust");
    println!("Type 'help' for a list of commands, 'photos' or 'video' to open an app.");
    println!();
}

/// The main loop in VGA text mode: handle keys, mouse movement and the cursor blink,
/// sleeping in between.
fn run(boot: &multiboot::BootInfo) -> ! {
    let mut keyboard = keyboard::Keyboard::new();
    let mut mouse = ps2::MouseDecoder::new();
    let mut shell = shell::Shell::new();
    let mut next_blink = 0;
    let mut last_second = u64::MAX;
    let mut pointer = CONSOLE.lock().mouse_position();
    let mut status_dirty = true;

    shell.prompt();
    loop {
        while let Some(scancode) = KEYBOARD_BYTES.pop() {
            if let Some(key) = keyboard.feed(scancode) {
                if let keyboard::Key::LayoutChanged = key {
                    status_dirty = true;
                }
                shell.on_key(key, boot);
            }
        }

        let mut moved = false;
        while let Some(byte) = MOUSE_BYTES.pop() {
            if let Some(packet) = mouse.feed(byte) {
                pointer = CONSOLE.lock().move_mouse(packet.dx, packet.dy);
                moved |= packet.left || packet.right || packet.dx != 0 || packet.dy != 0;
            }
        }
        status_dirty |= moved;

        let now = interrupts::ticks();
        if now >= next_blink {
            CONSOLE.lock().blink();
            next_blink = now + BLINK_TICKS;
        }
        let second = now / interrupts::TIMER_HZ;
        if second != last_second || status_dirty {
            last_second = second;
            status_dirty = false;
            let mut text = StackString::<64>::new();
            let _ = write!(
                text,
                "{}  x:{:<4} y:{:<4} {}:{:02}:{:02}",
                keyboard.layout().name(),
                pointer.0,
                pointer.1,
                second / 3600,
                second / 60 % 60,
                second % 60
            );
            CONSOLE.lock().set_status(text.as_str());
        }

        interrupts::wait_for_interrupt(|| !KEYBOARD_BYTES.is_empty() || !MOUSE_BYTES.is_empty());
    }
}

/// Fixed-size string for formatting without a heap.
struct StackString<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackString<N> {
    fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    fn push_str(&mut self, s: &str) {
        let _ = self.write_str(s);
    }

    /// Remove the last character.
    fn pop(&mut self) {
        if let Some(c) = self.as_str().chars().next_back() {
            self.len -= c.len_utf8();
        }
    }
}

impl<const N: usize> Default for StackString<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for StackString<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let n = bytes.len().min(N - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(())
    }
}

fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    interrupts::disable();
    // Whoever held the console will never run again.
    unsafe { CONSOLE.force_unlock() };
    let mut con = CONSOLE.lock();
    con.reattach();
    // start a fresh line before switching colours, so a scroll does not
    // fill the new line with the panic background
    con.set_color(Color::LightRed, Color::Black);
    let _ = writeln!(con);
    con.set_color(Color::White, Color::Red);
    let _ = write!(con, " KERNEL PANIC ");
    con.set_color(Color::LightRed, Color::Black);
    let _ = writeln!(con, " {}", info.message());
    if let Some(location) = info.location() {
        let _ = writeln!(con, " at {}:{}", location.file(), location.line());
    }
    let _ = writeln!(con, " The system is halted.");
    halt()
}
