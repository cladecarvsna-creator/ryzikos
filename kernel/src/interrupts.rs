//! Interrupts: the IDT, the 8259 PIC and the PIT timer.
//!
//! The entry stubs live in boot/interrupts.asm. They save the registers
//! and call `interrupt_dispatch` with a pointer to the saved frame.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::port::{inb, io_wait, outb};
use crate::sync::ByteQueue;

/// First vector used by hardware interrupts after remapping the PIC.
const IRQ_BASE: u8 = 32;
const IRQ_TIMER: u8 = IRQ_BASE;
const IRQ_KEYBOARD: u8 = IRQ_BASE + 1;
const IRQ_MOUSE: u8 = IRQ_BASE + 12;

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xa0;
const PIC2_DATA: u16 = 0xa1;
const PIC_EOI: u8 = 0x20;

pub const TIMER_HZ: u64 = 100;

/// Timer ticks since boot.
static TICKS: AtomicU64 = AtomicU64::new(0);
/// Raw scancodes from the keyboard interrupt.
pub static KEYBOARD_BYTES: ByteQueue = ByteQueue::new();
/// Raw packet bytes from the mouse interrupt.
pub static MOUSE_BYTES: ByteQueue = ByteQueue::new();

/// Registers saved by the entry stub, in stack order.
#[repr(C)]
pub struct InterruptFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const EMPTY: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn new(handler: u64) -> Self {
        Self {
            offset_low: handler as u16,
            selector: 0x08, // the 64-bit code segment from boot.asm
            ist: 0,
            type_attr: 0x8e, // present, ring 0, interrupt gate
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

#[repr(C, align(16))]
struct Idt([IdtEntry; 256]);

static IDT: crate::sync::IrqMutex<Idt> = crate::sync::IrqMutex::new(Idt([IdtEntry::EMPTY; 256]));

extern "C" {
    static isr_stub_table: [u64; 48];
}

const EXCEPTION_NAMES: [&str; 32] = [
    "division error",
    "debug",
    "non-maskable interrupt",
    "breakpoint",
    "overflow",
    "bound range exceeded",
    "invalid opcode",
    "device not available",
    "double fault",
    "coprocessor segment overrun",
    "invalid TSS",
    "segment not present",
    "stack-segment fault",
    "general protection fault",
    "page fault",
    "reserved",
    "x87 floating-point exception",
    "alignment check",
    "machine check",
    "SIMD floating-point exception",
    "virtualization exception",
    "control protection exception",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "hypervisor injection exception",
    "VMM communication exception",
    "security exception",
    "reserved",
];

/// Set up the IDT, the PIC and the timer. Interrupts stay disabled until
/// `enable` is called.
pub fn init() {
    {
        let mut idt = IDT.lock();
        for (vector, &stub) in unsafe { isr_stub_table.iter() }.enumerate() {
            idt.0[vector] = IdtEntry::new(stub);
        }
        let pointer = IdtPointer {
            limit: (core::mem::size_of::<Idt>() - 1) as u16,
            base: &idt.0 as *const _ as u64,
        };
        unsafe { core::arch::asm!("lidt [{}]", in(reg) &pointer, options(readonly, nostack)) };
    }
    init_pic();
    init_timer();
}

/// Remap the PIC so IRQs 0-15 use vectors 32-47 instead of clashing with
/// CPU exceptions, and unmask the timer, keyboard and mouse.
fn init_pic() {
    unsafe {
        outb(PIC1_COMMAND, 0x11); // start initialisation, expect ICW4
        io_wait();
        outb(PIC2_COMMAND, 0x11);
        io_wait();
        outb(PIC1_DATA, IRQ_BASE); // vector offsets
        io_wait();
        outb(PIC2_DATA, IRQ_BASE + 8);
        io_wait();
        outb(PIC1_DATA, 4); // the slave PIC is on IRQ 2
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();
        outb(PIC1_DATA, 0x01); // 8086 mode
        io_wait();
        outb(PIC2_DATA, 0x01);
        io_wait();
        // a set bit masks the IRQ: keep 0 (timer), 1 (keyboard), 2 (cascade)
        // on the master and 12 (mouse) on the slave
        outb(PIC1_DATA, !0b0000_0111);
        outb(PIC2_DATA, !0b0001_0000);
    }
}

/// Program PIT channel 0 to fire TIMER_HZ times a second.
fn init_timer() {
    let divisor = (1_193_182 / TIMER_HZ) as u16;
    unsafe {
        outb(0x43, 0x36); // channel 0, lobyte/hibyte, square wave
        outb(0x40, divisor as u8);
        outb(0x40, (divisor >> 8) as u8);
    }
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn enable() {
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
}

/// Disable interrupts and return whether they were enabled before.
pub fn disable() -> bool {
    let flags: u64;
    unsafe { core::arch::asm!("pushfq; pop {}; cli", out(reg) flags, options(nomem)) };
    flags & (1 << 9) != 0
}

/// Sleep until the next interrupt unless `has_work` says there is
/// already something to do. Checking and halting with interrupts off
/// means an interrupt cannot slip in between and be missed.
pub fn wait_for_interrupt(has_work: impl Fn() -> bool) {
    disable();
    if has_work() {
        enable();
    } else {
        // sti takes effect after the next instruction, so no interrupt
        // can arrive between it and hlt
        unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
    }
}

fn end_of_interrupt(vector: u8) {
    unsafe {
        if vector >= IRQ_BASE + 8 {
            outb(PIC2_COMMAND, PIC_EOI);
        }
        outb(PIC1_COMMAND, PIC_EOI);
    }
}

/// Called from `isr_common` in boot/interrupts.asm.
#[no_mangle]
extern "C" fn interrupt_dispatch(frame: &mut InterruptFrame) {
    let vector = frame.vector as u8;
    match vector {
        0..=31 => exception(frame),
        IRQ_TIMER => {
            let now = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
            crate::sound::on_tick(now);
        }
        IRQ_KEYBOARD => KEYBOARD_BYTES.push(unsafe { inb(0x60) }),
        IRQ_MOUSE => MOUSE_BYTES.push(unsafe { inb(0x60) }),
        _ => {}
    }
    if vector >= IRQ_BASE {
        end_of_interrupt(vector);
    }
}

fn exception(frame: &InterruptFrame) -> ! {
    let name = EXCEPTION_NAMES[frame.vector as usize];
    if frame.vector == 14 {
        let cr2: u64;
        unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack)) };
        panic!(
            "CPU exception: {} at rip={:#x}, address={:#x}, error={:#x}",
            name, frame.rip, cr2, frame.error_code
        );
    }
    panic!(
        "CPU exception: {} (vector {}) at rip={:#x}, error={:#x}, rsp={:#x}",
        name, frame.vector, frame.rip, frame.error_code, frame.rsp
    );
}
