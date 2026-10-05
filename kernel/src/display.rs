//! Changing the screen resolution. QEMU, VirtualBox and Bochs share one
//! simple virtual graphics card (the "Bochs graphics adapter"): a mode
//! is a width, a height and a colour depth written to two I/O ports, and
//! the picture stays at the address GRUB found. A virtual machine shown
//! full screen stretches the picture to the monitor, so a resolution of
//! the monitor's shape keeps circles round. Real graphics cards keep the
//! mode GRUB chose.

use crate::framebuffer::Framebuffer;
use crate::fs;
use crate::port::{inw, outw};
use alloc::format;

const INDEX: u16 = 0x01ce;
const DATA: u16 = 0x01cf;

const REG_ID: u16 = 0;
const REG_XRES: u16 = 1;
const REG_YRES: u16 = 2;
const REG_BPP: u16 = 3;
const REG_ENABLE: u16 = 4;
const REG_VIRT_WIDTH: u16 = 6;
const REG_X_OFFSET: u16 = 8;
const REG_Y_OFFSET: u16 = 9;
const REG_VIDEO_MEMORY_64K: u16 = 10;

const ENABLED: u16 = 0x01;
const LFB_ENABLED: u16 = 0x40;

/// The resolutions offered, widest first: 16:10, 16:9, 5:4 and 4:3
/// monitors. None is bigger than the desktop's buffers (1920x1200).
pub const MODES: [(usize, usize); 11] = [
    (1920, 1200),
    (1920, 1080),
    (1680, 1050),
    (1600, 900),
    (1440, 900),
    (1360, 768),
    (1280, 1024),
    (1280, 800),
    (1280, 720),
    (1152, 864),
    (1024, 768),
];

/// Where the chosen resolution is kept, for the next start.
const SAVED: &str = "/$screen.txt";

fn read(reg: u16) -> u16 {
    unsafe {
        outw(INDEX, reg);
        inw(DATA)
    }
}

fn write(reg: u16, value: u16) {
    unsafe {
        outw(INDEX, reg);
        outw(DATA, value);
    }
}

/// Whether the resolution can be changed: the screen is the virtual
/// card's, in the mode it says it is in.
pub fn can_change(fb: &Framebuffer) -> bool {
    let id = read(REG_ID);
    (0xb0c0..=0xb0cf).contains(&id)
        && read(REG_ENABLE) & ENABLED != 0
        && read(REG_XRES) as usize == fb.width
        && read(REG_YRES) as usize == fb.height
        && fb.bytes_per_pixel == 4
}

/// Switch to `width` x `height`. Returns the screen in its new mode, or
/// None (and the old mode still on) when the card can't show it.
pub fn set_mode(fb: &Framebuffer, width: usize, height: usize) -> Option<Framebuffer> {
    if !can_change(fb) || width > u16::MAX as usize || height > u16::MAX as usize {
        return None;
    }
    // newer cards say how much video memory they have
    if read(REG_ID) >= 0xb0c5 {
        let memory = read(REG_VIDEO_MEMORY_64K) as usize * 65536;
        if memory > 0 && width * height * 4 > memory {
            return None;
        }
    }
    let program = |w: usize, h: usize| {
        write(REG_ENABLE, 0);
        write(REG_XRES, w as u16);
        write(REG_YRES, h as u16);
        write(REG_BPP, 32);
        write(REG_X_OFFSET, 0);
        write(REG_Y_OFFSET, 0);
        write(REG_ENABLE, ENABLED | LFB_ENABLED);
    };
    program(width, height);
    if read(REG_XRES) as usize != width || read(REG_YRES) as usize != height {
        program(fb.width, fb.height);
        return None;
    }
    let new = Framebuffer {
        width,
        height,
        pitch: read(REG_VIRT_WIDTH) as usize * 4,
        ..*fb
    };
    crate::framebuffer::write_combine(&new);
    Some(new)
}

/// Remember the resolution for the next start.
pub fn save(width: usize, height: usize) {
    let _ = fs::write(SAVED, format!("{}x{}\r\n", width, height).as_bytes());
}

/// The resolution chosen before, if any.
pub fn saved() -> Option<(usize, usize)> {
    let data = fs::read(SAVED).ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    let (w, h) = text.trim().split_once('x')?;
    let mode = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    MODES.contains(&mode).then_some(mode)
}
