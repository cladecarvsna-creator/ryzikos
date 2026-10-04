//! A shared clipboard with the computer RyzikOS runs on, when it runs in
//! QEMU. QEMU can't pass the host's clipboard to a guest by itself (only
//! SPICE can, with an agent inside the guest), so `run-windows.bat`
//! connects the second serial port (COM2) to a small PowerShell script,
//! `ryzikos-clipboard.ps1`, which reads and writes the Windows clipboard.
//!
//! Both sides send the same message whenever their clipboard gets new
//! text: a line `CLIP <bytes>` and then that many bytes of UTF-8. RyzikOS
//! says `HELLO RYZIKOS-CLIP 1` when it starts, and the script answers
//! with what is on the Windows clipboard. RyzikOS only writes to COM2
//! once a message came in, so on a real PC nothing is sent to whatever
//! might be plugged into that port.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::port::{inb, outb};
use crate::sync::IrqMutex;

const COM2: u16 = 0x2f8;
/// The longest text taken from the host, so a stray port can't fill memory.
const MAX: usize = 4 * 1024 * 1024;

/// COM2 exists.
static PRESENT: AtomicBool = AtomicBool::new(false);
/// The script on the host talked to us.
static CONNECTED: AtomicBool = AtomicBool::new(false);

enum State {
    /// Reading the line before the text.
    Header(Vec<u8>),
    /// Reading `left` more bytes of text.
    Body { text: Vec<u8>, left: usize },
}

static STATE: IrqMutex<State> = IrqMutex::new(State::Header(Vec::new()));

pub fn init() {
    unsafe {
        // the scratch register keeps what is written to it if the port exists
        outb(COM2 + 7, 0x5a);
        if inb(COM2 + 7) != 0x5a {
            return;
        }
        outb(COM2 + 1, 0x00); // no interrupts: the desktop polls
        outb(COM2 + 3, 0x80);
        outb(COM2, 0x01); // divisor 1: 115200 baud
        outb(COM2 + 1, 0x00);
        outb(COM2 + 3, 0x03); // 8 bits, no parity, one stop bit
        outb(COM2 + 2, 0xc7); // FIFO on and cleared
        outb(COM2 + 4, 0x0b);
    }
    PRESENT.store(true, Ordering::Relaxed);
    write(b"HELLO RYZIKOS-CLIP 1\n");
}

pub fn connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

fn write(bytes: &[u8]) {
    for &b in bytes {
        // give up on a port nobody reads
        let mut ready = false;
        for _ in 0..100_000 {
            if unsafe { inb(COM2 + 5) } & 0x20 != 0 {
                ready = true;
                break;
            }
        }
        if !ready {
            return;
        }
        unsafe { outb(COM2, b) };
    }
}

/// Text was copied in RyzikOS: hand it to the host.
pub fn send(text: &str) {
    if !connected() {
        return;
    }
    write(alloc::format!("CLIP {}\n", text.len()).as_bytes());
    write(text.as_bytes());
}

/// Read what came in. Returns text the host copied, if a whole message
/// arrived.
pub fn poll() -> Option<String> {
    if !PRESENT.load(Ordering::Relaxed) {
        return None;
    }
    let mut state = STATE.lock();
    let mut got = None;
    // a bounded amount per call, so a flood can't freeze the desktop
    for _ in 0..64 * 1024 {
        if unsafe { inb(COM2 + 5) } & 0x01 == 0 {
            break;
        }
        let b = unsafe { inb(COM2) };
        match &mut *state {
            State::Header(line) => {
                if b != b'\n' {
                    if line.len() < 256 {
                        line.push(b);
                    }
                    continue;
                }
                // a byte lost while starting up can leave junk in front
                let text = String::from_utf8_lossy(line);
                let len = text
                    .rfind("CLIP ")
                    .and_then(|i| text[i + 5..].trim().parse::<usize>().ok());
                if text.ends_with("HELLO") {
                    CONNECTED.store(true, Ordering::Relaxed);
                }
                *state = match len {
                    Some(0) => {
                        CONNECTED.store(true, Ordering::Relaxed);
                        State::Header(Vec::new())
                    }
                    Some(n) if n <= MAX => {
                        CONNECTED.store(true, Ordering::Relaxed);
                        State::Body {
                            text: Vec::with_capacity(n),
                            left: n,
                        }
                    }
                    _ => State::Header(Vec::new()),
                };
            }
            State::Body { text, left } => {
                text.push(b);
                *left -= 1;
                if *left == 0 {
                    let bytes = core::mem::take(text);
                    *state = State::Header(Vec::new());
                    if let Ok(s) = String::from_utf8(bytes) {
                        got = Some(s.replace("\r\n", "\n"));
                    }
                }
            }
        }
    }
    got
}
