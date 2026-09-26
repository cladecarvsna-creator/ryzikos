//! VMware's absolute mouse ("vmmouse"), which QEMU also offers on its
//! default PC machine. The pointer then follows the host cursor exactly,
//! with no mouse grab and no lag. The PS/2 mouse keeps raising IRQ 12;
//! the positions are read from the VMware backdoor port.

const MAGIC: u32 = 0x564d_5868;
const PORT: u16 = 0x5658;

const GET_VERSION: u32 = 10;
const DATA: u32 = 39;
const STATUS: u32 = 40;
const COMMAND: u32 = 41;

const READ_ID: u32 = 0x4541_4552;
const REQUEST_ABSOLUTE: u32 = 0x5342_4152;
const VERSION_ID: u32 = 0x3442_554a;
/// Status meaning the device fell over and must be enabled again.
const STATUS_ERROR: u32 = 0xffff_0000;

pub const LEFT: u32 = 0x20;
pub const RIGHT: u32 = 0x10;

/// One backdoor call. Returns (eax, ebx, ecx, edx).
fn call(command: u32, arg: u32) -> (u32, u32, u32, u32) {
    let (eax, ecx, edx): (u32, u32, u32);
    let mut rbx = arg as u64;
    // LLVM reserves rbx, so swap the argument through another register
    unsafe {
        core::arch::asm!(
            "xchg {b}, rbx",
            "in eax, dx",
            "xchg {b}, rbx",
            b = inout(reg) rbx,
            inout("eax") MAGIC => eax,
            inout("ecx") command => ecx,
            inout("edx") PORT as u32 => edx,
            options(nostack, nomem),
        );
    }
    (eax, rbx as u32, ecx, edx)
}

/// Switch the mouse to absolute mode. Returns false on real hardware or
/// when the emulator has no vmmouse. Call after the PS/2 mouse is on.
pub fn init() -> bool {
    if call(GET_VERSION, 0).1 != MAGIC {
        return false;
    }
    call(COMMAND, READ_ID);
    if call(STATUS, 0).0 & 0xffff == 0 || call(DATA, 1).0 != VERSION_ID {
        return false;
    }
    call(COMMAND, REQUEST_ABSOLUTE);
    true
}

pub struct Event {
    pub buttons: u32,
    /// 0 to 65535 across the screen.
    pub x: u32,
    pub y: u32,
    /// Wheel clicks, positive when scrolling down.
    pub wheel: i32,
}

/// The next queued event, if any.
pub fn poll() -> Option<Event> {
    let status = call(STATUS, 0).0;
    if status == STATUS_ERROR {
        call(COMMAND, REQUEST_ABSOLUTE);
        return None;
    }
    if status & 0xffff < 4 {
        return None;
    }
    let (buttons, x, y, wheel) = call(DATA, 4);
    Some(Event {
        buttons,
        x,
        y,
        wheel: wheel as u8 as i8 as i32,
    })
}
