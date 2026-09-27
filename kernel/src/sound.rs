//! Sound through the Ensoniq AudioPCI ES1370, the card QEMU emulates with
//! `-device ES1370`. Short sounds are copied into one buffer that the
//! card reads by DMA through its second DAC; the timer interrupt stops the
//! card when the sound is over, so nothing plays twice.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::interrupts;
use crate::pci;
use crate::port::{inl, outl};
use crate::serial;

/// Samples per second, for each of the two channels.
pub const RATE: u32 = 22050;

// registers, from the I/O base in BAR0
const CTRL: u16 = 0x00;
const STATUS: u16 = 0x04;
const MEMPAGE: u16 = 0x0c;
const CODEC: u16 = 0x10;
const SCTRL: u16 = 0x20;
const DAC2_COUNT: u16 = 0x28;
/// On memory page 0x0c: where DAC2's buffer is, and its size.
const DAC2_FRAME_ADDR: u16 = 0x38;
const DAC2_FRAME_SIZE: u16 = 0x3c;

const CTRL_DAC2_EN: u32 = 1 << 5;
const CTRL_CDC_EN: u32 = 1 << 1;
const CTRL_PCLKDIV_SHIFT: u32 = 16;
const STATUS_CODEC_BUSY: u32 = 1 << 10;
/// DAC2 plays 16-bit stereo.
const SCTRL_P2_16BIT_STEREO: u32 = 0x0c;

/// The DMA buffer: at 22050 Hz, 16-bit stereo, about three seconds. The
/// card counts it in 32-bit words, at most 65536 of them.
const BUFFER_BYTES: usize = 256 * 1024;

#[repr(C, align(4096))]
struct Buffer([u8; BUFFER_BYTES]);
static mut BUFFER: Buffer = Buffer([0; BUFFER_BYTES]);

static PRESENT: AtomicBool = AtomicBool::new(false);
static BASE: AtomicU16 = AtomicU16::new(0);
/// CTRL with DAC2 off, written by the timer interrupt to stop.
static CTRL_IDLE: AtomicU32 = AtomicU32::new(0);
/// Timer tick at which the playing sound is over; 0 when silent.
static STOP_AT: AtomicU64 = AtomicU64::new(0);
/// 0 to 100, like the volume slider.
static VOLUME: AtomicU32 = AtomicU32::new(60);

fn reg_read(reg: u16) -> u32 {
    unsafe { inl(BASE.load(Ordering::Relaxed) + reg) }
}

fn reg_write(reg: u16, value: u32) {
    unsafe { outl(BASE.load(Ordering::Relaxed) + reg, value) }
}

/// Set a register of the AK4531 mixer chip on the card.
fn codec(reg: u8, value: u8) {
    for _ in 0..100_000 {
        if reg_read(STATUS) & STATUS_CODEC_BUSY == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    reg_write(CODEC, (reg as u32) << 8 | value as u32);
}

/// Look for the sound card and get it ready. Returns whether there is one.
pub fn init() -> bool {
    let Some(dev) = pci::find(&[(0x1274, 0x5000)]) else {
        serial::write_str("sound: no sound card\n");
        return false;
    };
    let bar = dev.read(0x10);
    if bar & 1 == 0 {
        serial::write_str("sound: the ES1370 has no I/O ports\n");
        return false;
    }
    BASE.store((bar & !3) as u16, Ordering::Relaxed);
    // I/O ports and DMA
    let command = dev.read(0x04);
    dev.write(0x04, command | 0x05);

    let divider = 1_411_200 / RATE - 2;
    let ctrl = divider << CTRL_PCLKDIV_SHIFT | CTRL_CDC_EN;
    reg_write(CTRL, ctrl);
    CTRL_IDLE.store(ctrl, Ordering::Relaxed);
    reg_write(SCTRL, SCTRL_P2_16BIT_STEREO);

    // the mixer: out of reset, the DAC to the output, nothing muted
    codec(0x16, 0x03); // reset off
    codec(0x17, 0x00); // clock from the card
    codec(0x00, 0x00); // master left: loudest
    codec(0x01, 0x00); // master right
    codec(0x02, 0x06); // voice (the DAC) left: 0 dB
    codec(0x03, 0x06); // voice right
    codec(0x10, 0x00);
    codec(0x11, 0x0c); // voice left and right to the output

    PRESENT.store(true, Ordering::Relaxed);
    serial::write_str("sound: ES1370 ready\n");
    true
}

pub fn available() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

pub fn set_volume(percent: i32) {
    VOLUME.store(percent.clamp(0, 100) as u32, Ordering::Relaxed);
}

/// Play mono samples at `RATE`, louder or quieter with the volume. A sound
/// that is playing stops first.
pub fn play(samples: &[i16]) {
    if !available() || samples.is_empty() {
        return;
    }
    let volume = VOLUME.load(Ordering::Relaxed) as i32;
    if volume == 0 {
        return;
    }
    // the ear hears loudness roughly as the square
    let gain = volume * volume; // up to 10000
    STOP_AT.store(0, Ordering::Relaxed);
    let idle = CTRL_IDLE.load(Ordering::Relaxed);
    reg_write(CTRL, idle);

    let frames = samples.len().min(BUFFER_BYTES / 4);
    // silence after the sound, in case the card reads on a little
    let used = (frames + RATE as usize / 5).min(BUFFER_BYTES / 4);
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER.0) };
    for (i, frame) in buf[..used * 4].chunks_exact_mut(4).enumerate() {
        let s = samples.get(i).map_or(0, |&s| (s as i32 * gain / 10_000) as i16);
        let b = s.to_le_bytes();
        frame.copy_from_slice(&[b[0], b[1], b[0], b[1]]);
    }

    reg_write(MEMPAGE, 0x0c);
    // memory is mapped one to one, so the address is the physical one
    reg_write(DAC2_FRAME_ADDR, buf.as_ptr() as u32);
    reg_write(DAC2_FRAME_SIZE, (used as u32) - 1);
    reg_write(DAC2_COUNT, (used as u32) - 1);
    reg_write(SCTRL, SCTRL_P2_16BIT_STEREO);
    reg_write(CTRL, idle | CTRL_DAC2_EN);

    let ticks = (frames as u64 * interrupts::TIMER_HZ).div_ceil(RATE as u64) + 3;
    STOP_AT.store(interrupts::ticks() + ticks, Ordering::Relaxed);
}

/// From the timer interrupt: stop the card when the sound is over.
pub fn on_tick(now: u64) {
    let stop = STOP_AT.load(Ordering::Relaxed);
    if stop != 0 && now >= stop {
        STOP_AT.store(0, Ordering::Relaxed);
        reg_write(CTRL, CTRL_IDLE.load(Ordering::Relaxed));
    }
}

/// The sound for a volume change: a short, soft bell, like the ones
/// Windows and macOS play.
pub fn volume_chime() -> alloc::vec::Vec<i16> {
    bell(&[(1318.5, 0.0, 1.0), (2637.0, 0.0, 0.22), (3955.5, 0.0, 0.06)], 0.20, 0.09)
}

/// Sine partials (frequency, start in seconds, loudness) that fade out
/// over `decay` seconds, `length` seconds in all.
fn bell(partials: &[(f32, f32, f32)], length: f32, decay: f32) -> alloc::vec::Vec<i16> {
    let n = (length * RATE as f32) as usize;
    let mut out = alloc::vec![0i16; n];
    let total: f32 = partials.iter().map(|p| p.2).sum::<f32>().max(1.0);
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / RATE as f32;
        let mut v = 0.0;
        for &(freq, start, level) in partials {
            let t = t - start;
            if t < 0.0 {
                continue;
            }
            // 4 ms to rise, so it does not click, then an even fade
            let rise = (t / 0.004).min(1.0);
            let fade = libm::expf(-t / decay * 2.3);
            v += level * rise * fade * libm::sinf(2.0 * core::f32::consts::PI * freq * t);
        }
        // the very end goes to zero smoothly
        let tail = ((length - t) / 0.02).clamp(0.0, 1.0);
        *s = (v / total * tail * 26_000.0) as i16;
    }
    out
}
