//! Output to the COM1 serial port, so headless runs (tests, CI) can see
//! what the kernel prints.

use crate::port::{inb, outb};

const COM1: u16 = 0x3f8;

pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // enable DLAB to set the baud rate divisor
        outb(COM1, 0x03); // divisor 3: 38400 baud
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03); // 8 bits, no parity, one stop bit
        outb(COM1 + 2, 0xc7); // enable and clear FIFO
        outb(COM1 + 4, 0x0b); // RTS/DSR set
    }
}

fn write_byte(byte: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {}
        outb(COM1, byte);
    }
}

pub fn write_str(text: &str) {
    for byte in text.bytes() {
        if byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(byte);
    }
}
