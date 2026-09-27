//! Linear framebuffer set up by GRUB, and basic drawing on it.

use crate::font;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// The colour as `0x00RRGGBB`.
    pub fn raw(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    /// Mix two colours; `t` goes from 0 (all `self`) to 255 (all `other`).
    pub fn mix(self, other: Rgb, t: u8) -> Rgb {
        let lerp = |a: u8, b: u8| ((a as u32 * (255 - t as u32) + b as u32 * t as u32) / 255) as u8;
        Rgb::new(
            lerp(self.r, other.r),
            lerp(self.g, other.g),
            lerp(self.b, other.b),
        )
    }
}

/// Where one colour channel sits inside a pixel.
#[derive(Clone, Copy)]
pub struct ColorField {
    pub position: u8,
    pub size: u8,
}

impl ColorField {
    fn encode(self, value: u8) -> u32 {
        ((value as u32) >> (8 - self.size.min(8))) << self.position
    }
}

#[derive(Clone, Copy)]
pub struct Framebuffer {
    pub base: *mut u8,
    pub pitch: usize,
    pub width: usize,
    pub height: usize,
    pub bytes_per_pixel: usize,
    pub red: ColorField,
    pub green: ColorField,
    pub blue: ColorField,
}

// The framebuffer is plain memory owned by the console.
unsafe impl Send for Framebuffer {}

impl Framebuffer {
    /// Raw pixel value for a colour in this framebuffer's format.
    pub fn encode(&self, color: Rgb) -> u32 {
        self.red.encode(color.r) | self.green.encode(color.g) | self.blue.encode(color.b)
    }

    fn pixel_ptr(&self, x: usize, y: usize) -> *mut u8 {
        unsafe { self.base.add(y * self.pitch + x * self.bytes_per_pixel) }
    }

    /// Write a raw pixel value. Out of range coordinates are ignored.
    pub fn put_raw(&self, x: usize, y: usize, value: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let ptr = self.pixel_ptr(x, y);
        unsafe {
            match self.bytes_per_pixel {
                4 => (ptr as *mut u32).write_volatile(value),
                3 => {
                    ptr.write_volatile(value as u8);
                    ptr.add(1).write_volatile((value >> 8) as u8);
                    ptr.add(2).write_volatile((value >> 16) as u8);
                }
                _ => (ptr as *mut u16).write_volatile(value as u16),
            }
        }
    }

    /// Read a raw pixel value.
    pub fn get_raw(&self, x: usize, y: usize) -> u32 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let ptr = self.pixel_ptr(x, y);
        unsafe {
            match self.bytes_per_pixel {
                4 => (ptr as *const u32).read_volatile(),
                3 => {
                    ptr.read_volatile() as u32
                        | (ptr.add(1).read_volatile() as u32) << 8
                        | (ptr.add(2).read_volatile() as u32) << 16
                }
                _ => (ptr as *const u16).read_volatile() as u32,
            }
        }
    }

    pub fn fill_rect(&self, x: usize, y: usize, w: usize, h: usize, color: Rgb) {
        let value = self.encode(color);
        for py in y..(y + h).min(self.height) {
            for px in x..(x + w).min(self.width) {
                self.put_raw(px, py, value);
            }
        }
    }

    /// Fill a rectangle with a gradient from `left` to `right`.
    pub fn horizontal_gradient(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        left: Rgb,
        right: Rgb,
    ) {
        for col in 0..w {
            let t = (col * 255 / w.max(1)) as u8;
            self.fill_rect(x + col, y, 1, h, left.mix(right, t));
        }
    }

    pub fn fill_circle(&self, cx: isize, cy: isize, radius: isize, color: Rgb) {
        let value = self.encode(color);
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let (x, y) = (cx + dx, cy + dy);
                if dx * dx + dy * dy <= radius * radius && x >= 0 && y >= 0 {
                    self.put_raw(x as usize, y as usize, value);
                }
            }
        }
    }

    /// Draw one character. With `bg` set to `None` the background is
    /// left as it is.
    pub fn draw_char(&self, x: usize, y: usize, c: char, fg: Rgb, bg: Option<Rgb>) {
        let fg = self.encode(fg);
        let bg = bg.map(|bg| self.encode(bg));
        for (row, bits) in font::glyph(c).iter().enumerate() {
            for col in 0..font::WIDTH {
                if bits & (0x80 >> col) != 0 {
                    self.put_raw(x + col, y + row, fg);
                } else if let Some(bg) = bg {
                    self.put_raw(x + col, y + row, bg);
                }
            }
        }
    }

    pub fn draw_text(&self, x: usize, y: usize, text: &str, fg: Rgb, bg: Option<Rgb>) {
        for (i, c) in text.chars().enumerate() {
            self.draw_char(x + i * font::WIDTH, y, c, fg, bg);
        }
    }
}

/// Make writes to the framebuffer write-combining. Firmware on real PCs
/// usually leaves video memory uncached, so every pixel is its own trip
/// over the bus and drawing is many times slower than in an emulator.
/// Page attribute slot 1 (the PWT bit alone) becomes write-combining, and
/// the 2 MiB pages that hold the framebuffer use it. Nothing else sets PWT.
pub fn write_combine(fb: &Framebuffer) {
    use core::arch::x86_64::__cpuid;
    const IA32_PAT: u32 = 0x277;
    const PWT: u64 = 1 << 3;
    const HUGE: u64 = 1 << 7;
    const WC: u64 = 0x01;
    if __cpuid(1).edx & (1 << 16) == 0 {
        return; // no page attribute table
    }
    let start = fb.base as u64;
    let end = start + (fb.pitch * fb.height) as u64;
    // the boot code maps the first 4 GiB with 2 MiB pages
    if end > 1 << 32 {
        return;
    }
    unsafe {
        let pat = rdmsr(IA32_PAT);
        wrmsr(IA32_PAT, pat & !(0xff << 8) | WC << 8);
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
        let p4 = (cr3 & !0xfff) as *const u64;
        let p3 = (*p4 & 0x000f_ffff_ffff_f000) as *const u64;
        let mut page = start & !0x1f_ffff;
        while page < end {
            let p3e = *p3.add((page >> 30) as usize);
            let p2 = (p3e & 0x000f_ffff_ffff_f000) as *mut u64;
            let entry = p2.add(((page >> 21) & 0x1ff) as usize);
            if *entry & HUGE != 0 {
                *entry |= PWT;
            }
            page += 0x20_0000;
        }
        // drop cached lines and old translations of those pages
        core::arch::asm!("wbinvd", "mov {0}, cr3", "mov cr3, {0}", out(reg) _, options(nostack));
    }
}

unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    core::arch::asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack));
    (hi as u64) << 32 | lo as u64
}

unsafe fn wrmsr(msr: u32, value: u64) {
    core::arch::asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nostack));
}
