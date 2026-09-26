//! VGA text mode (80x25, buffer at 0xb8000). Used when GRUB did not give
//! us a graphics framebuffer.

use crate::port::outb;

const BUFFER: *mut u16 = 0xb8000 as *mut u16;
pub const WIDTH: usize = 80;
pub const HEIGHT: usize = 25;

/// Code page 437 byte for a character, '?' if it has none.
fn cp437(c: char) -> u8 {
    match c {
        ' '..='~' => c as u8,
        '─' => 0xc4,
        '│' => 0xb3,
        '┌' => 0xda,
        '┐' => 0xbf,
        '└' => 0xc0,
        '┘' => 0xd9,
        '█' => 0xdb,
        '░' => 0xb0,
        '▒' => 0xb1,
        '▓' => 0xb2,
        _ => b'?',
    }
}

pub fn put(row: usize, col: usize, c: char, fg: u8, bg: u8) {
    let cell = ((bg as u16) << 12) | ((fg as u16) << 8) | cp437(c) as u16;
    unsafe { BUFFER.add(row * WIDTH + col).write_volatile(cell) };
}

/// Show the hardware cursor as an underline (scanlines 14-15).
pub fn enable_cursor() {
    unsafe {
        outb(0x3d4, 0x0a);
        outb(0x3d5, 14);
        outb(0x3d4, 0x0b);
        outb(0x3d5, 15);
    }
}

/// Move the blinking hardware cursor.
pub fn set_cursor(row: usize, col: usize) {
    let pos = (row * WIDTH + col) as u16;
    unsafe {
        outb(0x3d4, 0x0f);
        outb(0x3d5, pos as u8);
        outb(0x3d4, 0x0e);
        outb(0x3d5, (pos >> 8) as u8);
    }
}
