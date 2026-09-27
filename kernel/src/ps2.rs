//! PS/2 controller (i8042): turns on keyboard and mouse interrupts and
//! decodes mouse packets.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::port::{inb, outb};

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;

const CONFIG_KEYBOARD_IRQ: u8 = 1 << 0;
const CONFIG_MOUSE_IRQ: u8 = 1 << 1;
const CONFIG_MOUSE_CLOCK_OFF: u8 = 1 << 5;

const ACK: u8 = 0xfa;

/// The mouse has a wheel and sends four-byte packets (IntelliMouse).
static WHEEL: AtomicBool = AtomicBool::new(false);

/// Wait until the controller can take a byte. False on timeout.
fn wait_write() -> bool {
    (0..100_000).any(|_| unsafe { inb(STATUS) } & STATUS_INPUT_FULL == 0)
}

/// Wait for a byte from the controller. None on timeout.
fn read() -> Option<u8> {
    (0..100_000)
        .find(|_| unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL != 0)
        .map(|_| unsafe { inb(DATA) })
}

fn command(cmd: u8) {
    if wait_write() {
        unsafe { outb(COMMAND, cmd) };
    }
}

fn write_data(byte: u8) {
    if wait_write() {
        unsafe { outb(DATA, byte) };
    }
}

fn flush() {
    while unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL != 0 {
        unsafe { inb(DATA) };
    }
}

/// Send a byte to the mouse and wait for its acknowledgement.
fn mouse_command(byte: u8) -> bool {
    command(0xd4); // next data byte goes to the second port
    write_data(byte);
    read() == Some(ACK)
}

/// Enable keyboard and mouse interrupts. Must run with interrupts off.
/// Returns whether a mouse answered.
pub fn init() -> bool {
    command(0xad); // disable keyboard port
    command(0xa7); // disable mouse port
    flush();

    command(0x20); // read configuration byte
    let config = read().unwrap_or(0);
    command(0x60); // write configuration byte
    write_data((config | CONFIG_KEYBOARD_IRQ | CONFIG_MOUSE_IRQ) & !CONFIG_MOUSE_CLOCK_OFF);

    command(0xae); // enable keyboard port
    command(0xa8); // enable mouse port

    let mouse = mouse_command(0xf6); // defaults
    if mouse {
        // the IntelliMouse knock: sample rates 200, 100, 80 turn the wheel
        // on, and the mouse then says it is type 3
        let knock = [200, 100, 80].iter().all(|&r| mouse_command(0xf3) && mouse_command(r));
        if knock && mouse_command(0xf2) && read() == Some(3) {
            WHEEL.store(true, Ordering::Relaxed);
            crate::serial::write_str("ps2: mouse wheel on\n");
        }
    }
    // 200 samples/s for smoother movement, then start sending packets
    let mouse = mouse && mouse_command(0xf3) && mouse_command(200) && mouse_command(0xf4);
    flush();
    mouse
}

pub struct MousePacket {
    pub dx: i32,
    /// Positive means down the screen.
    pub dy: i32,
    pub left: bool,
    pub right: bool,
    /// Wheel clicks, positive when scrolling down.
    pub wheel: i32,
}

/// Collects the bytes of a PS/2 mouse packet: three, or four with a wheel.
pub struct MouseDecoder {
    bytes: [u8; 4],
    count: usize,
    size: usize,
}

impl MouseDecoder {
    /// Call after `init`, which finds out whether the mouse has a wheel.
    pub fn new() -> Self {
        Self {
            bytes: [0; 4],
            count: 0,
            size: if WHEEL.load(Ordering::Relaxed) { 4 } else { 3 },
        }
    }

    pub fn feed(&mut self, byte: u8) -> Option<MousePacket> {
        // Bit 3 of the first byte is always set; use it to resync. A late
        // acknowledgement from init (0xfa) is not a packet either.
        if self.count == 0 && (byte & 0x08 == 0 || byte == ACK) {
            return None;
        }
        self.bytes[self.count] = byte;
        self.count += 1;
        if self.count < self.size {
            return None;
        }
        self.count = 0;
        let [flags, x, y, z] = self.bytes;
        if flags & 0xc0 != 0 {
            return None; // overflow, the movement is garbage
        }
        let dx = x as i32 - if flags & 0x10 != 0 { 256 } else { 0 };
        let dy = y as i32 - if flags & 0x20 != 0 { 256 } else { 0 };
        Some(MousePacket {
            dx,
            dy: -dy,
            left: flags & 0x01 != 0,
            right: flags & 0x02 != 0,
            // the low four bits, signed
            wheel: if self.size == 4 { ((z << 4) as i8 >> 4) as i32 } else { 0 },
        })
    }
}
