//! PS/2 controller (i8042): turns on keyboard and mouse interrupts and
//! decodes mouse packets.

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

    // defaults, 200 samples/s for smoother movement, then start sending
    // packets
    let mouse =
        mouse_command(0xf6) && mouse_command(0xf3) && mouse_command(200) && mouse_command(0xf4);
    flush();
    mouse
}

pub struct MousePacket {
    pub dx: i32,
    /// Positive means down the screen.
    pub dy: i32,
    pub left: bool,
    pub right: bool,
}

/// Collects the three bytes of a standard PS/2 mouse packet.
pub struct MouseDecoder {
    bytes: [u8; 3],
    count: usize,
}

impl MouseDecoder {
    pub const fn new() -> Self {
        Self {
            bytes: [0; 3],
            count: 0,
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
        if self.count < 3 {
            return None;
        }
        self.count = 0;
        let [flags, x, y] = self.bytes;
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
        })
    }
}
