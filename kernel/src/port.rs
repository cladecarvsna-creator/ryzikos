//! x86 I/O port access.

/// # Safety
/// Writing to an I/O port can reconfigure hardware.
pub unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
}

/// # Safety
/// Writing to an I/O port can reconfigure hardware.
pub unsafe fn outw(port: u16, value: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
}

/// # Safety
/// Reading some I/O ports has side effects (for example it pops a byte
/// from the PS/2 controller).
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    value
}

/// # Safety
/// Writing to an I/O port can reconfigure hardware.
pub unsafe fn outl(port: u16, value: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
}

/// # Safety
/// Reading some I/O ports has side effects.
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    core::arch::asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack));
    value
}

/// Give slow devices (the PIC) time to process the previous write.
pub fn io_wait() {
    unsafe { outb(0x80, 0) };
}

/// # Safety
/// Reading some I/O ports has side effects (the ATA data port hands out
/// the next word of a sector).
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    core::arch::asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack));
    value
}
