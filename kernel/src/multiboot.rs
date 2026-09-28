//! Reading the multiboot2 information structure GRUB passes to the kernel.

use alloc::string::String;
use alloc::vec::Vec;

use crate::framebuffer::{ColorField, Framebuffer};
use crate::sync::IrqMutex;

const TAG_END: u32 = 0;
const TAG_CMDLINE: u32 = 1;
const TAG_MODULE: u32 = 3;
const TAG_BOOTLOADER_NAME: u32 = 2;
const TAG_BASIC_MEMINFO: u32 = 4;
const TAG_FRAMEBUFFER: u32 = 8;

const FRAMEBUFFER_TYPE_RGB: u8 = 1;

pub struct BootInfo {
    pub framebuffer: Option<Framebuffer>,
    pub bootloader: &'static str,
    /// Memory above 1 MiB, in KiB, as reported by the BIOS.
    pub upper_memory_kib: u32,
}

/// Files GRUB loaded next to the kernel (`module2` in grub.cfg), by
/// name. The live CD brings the files the installer writes this way,
/// so it works from a USB stick too, where RyzikOS can't read the stick.
static MODULES: IrqMutex<Vec<(String, Vec<u8>)>> = IrqMutex::new(Vec::new());
/// Whether GRUB started the kernel with "live" on its command line.
static LIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// A file GRUB loaded, copied.
pub fn module(name: &str) -> Option<Vec<u8>> {
    MODULES.lock().iter().find(|(n, _)| n == name).map(|(_, d)| d.clone())
}

/// The size of a file GRUB loaded, without copying it: the live CD's
/// kernel.gz is megabytes, and the launcher asks on every frame.
pub fn module_len(name: &str) -> Option<usize> {
    MODULES.lock().iter().find(|(n, _)| n == name).map(|(_, d)| d.len())
}

/// Started from the live CD (or USB stick) rather than an installed disk.
pub fn live() -> bool {
    LIVE.load(core::sync::atomic::Ordering::Relaxed)
}

unsafe fn c_string(addr: usize, max: usize) -> &'static str {
    let bytes = core::slice::from_raw_parts(addr as *const u8, max);
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..len]).unwrap_or("")
}

unsafe fn read<T: Copy>(addr: usize) -> T {
    (addr as *const T).read_unaligned()
}

/// # Safety
/// `info` must be the multiboot2 information address GRUB passed in ebx.
pub unsafe fn parse(info: usize) -> BootInfo {
    let mut boot = BootInfo {
        framebuffer: None,
        bootloader: "unknown",
        upper_memory_kib: 0,
    };
    let total_size = read::<u32>(info) as usize;
    let mut tag = info + 8;
    while tag + 8 <= info + total_size {
        let kind = read::<u32>(tag);
        let size = read::<u32>(tag + 4) as usize;
        match kind {
            TAG_END => break,
            TAG_BOOTLOADER_NAME => {
                let bytes = core::slice::from_raw_parts((tag + 8) as *const u8, size - 8);
                let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                boot.bootloader = core::str::from_utf8(&bytes[..len]).unwrap_or("unknown");
            }
            TAG_CMDLINE => {
                let line = c_string(tag + 8, size - 8);
                if line.split_whitespace().any(|w| w == "live") {
                    LIVE.store(true, core::sync::atomic::Ordering::Relaxed);
                }
            }
            TAG_MODULE => {
                // copied into the heap: nothing else keeps that memory free
                let start = read::<u32>(tag + 8) as usize;
                let end = read::<u32>(tag + 12) as usize;
                let name = c_string(tag + 16, size - 16);
                if end > start {
                    let data = core::slice::from_raw_parts(start as *const u8, end - start);
                    MODULES.lock().push((String::from(name), data.to_vec()));
                }
            }
            TAG_BASIC_MEMINFO => boot.upper_memory_kib = read::<u32>(tag + 12),
            TAG_FRAMEBUFFER if read::<u8>(tag + 29) == FRAMEBUFFER_TYPE_RGB => {
                let bpp = read::<u8>(tag + 28);
                if matches!(bpp, 16 | 24 | 32) {
                    let field = |offset: usize| ColorField {
                        position: read::<u8>(tag + offset),
                        size: read::<u8>(tag + offset + 1),
                    };
                    boot.framebuffer = Some(Framebuffer {
                        base: read::<u64>(tag + 8) as usize as *mut u8,
                        pitch: read::<u32>(tag + 16) as usize,
                        width: read::<u32>(tag + 20) as usize,
                        height: read::<u32>(tag + 24) as usize,
                        bytes_per_pixel: bpp as usize / 8,
                        red: field(32),
                        green: field(34),
                        blue: field(36),
                    });
                }
            }
            _ => {}
        }
        tag += (size + 7) & !7;
    }
    boot
}
