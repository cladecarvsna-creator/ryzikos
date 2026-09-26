//! Fibers: code that runs on its own stack and can pause itself, so slow
//! work (downloading and laying out a web page) does not freeze the
//! desktop. There is one CPU and no preemption: the desktop resumes a
//! fiber for a short slice, and the fiber hands the CPU back by calling
//! [`pause`] (or [`pause_if_slice_used`]) at points where it holds no locks.
//!
//! A paused fiber keeps everything on its stack alive, so it can't simply
//! be thrown away. To stop one early, [`Fiber::cancel`] it and keep
//! resuming it: network waits then fail at once and it runs to its end.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use core::ptr::null_mut;

use crate::interrupts;

const STACK_SIZE: usize = 2 * 1024 * 1024;
/// Written at the bottom of every stack; if it changes the stack overflowed.
const CANARY: u64 = 0x5afe_57ac_c0de_f1be;
/// How long a fiber runs before it offers the CPU back, in timer ticks.
const SLICE_TICKS: u64 = 2;

core::arch::global_asm!(
    // switch_stacks(save: *mut usize, to: usize): save the callee-saved
    // registers, the SSE and x87 control words and the stack pointer, then
    // restore the same from the other stack.
    ".global everos_switch_stacks",
    "everos_switch_stacks:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "sub rsp, 8",
    "stmxcsr [rsp]",
    "fnstcw [rsp + 4]",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "ldmxcsr [rsp]",
    "fldcw [rsp + 4]",
    "add rsp, 8",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    // a new fiber starts here, with its Fiber in r12
    ".global everos_fiber_start",
    "everos_fiber_start:",
    "mov rdi, r12",
    "call everos_fiber_main",
    "ud2",
);

extern "C" {
    fn everos_switch_stacks(save: *mut usize, to: usize);
    fn everos_fiber_start();
}

struct Inner {
    stack: Vec<u64>,
    /// The fiber's saved stack pointer while it is paused.
    sp: usize,
    /// The resumer's saved stack pointer while the fiber runs.
    back: usize,
    entry: Option<Box<dyn FnOnce()>>,
    done: bool,
    cancelled: bool,
    /// When the current slice started.
    resumed_at: u64,
}

pub struct Fiber {
    inner: Box<Inner>,
}

/// The fiber running now, or null on the desktop's own stack.
struct Current(Cell<*mut Inner>);
// one CPU, and interrupt handlers never touch it
unsafe impl Sync for Current {}
static CURRENT: Current = Current(Cell::new(null_mut()));

impl Fiber {
    /// A fiber that will run `f` when first resumed.
    pub fn new(f: impl FnOnce() + 'static) -> Fiber {
        let words = STACK_SIZE / 8;
        let mut inner = Box::new(Inner {
            stack: vec![0u64; words],
            sp: 0,
            back: 0,
            entry: Some(Box::new(f)),
            done: false,
            cancelled: false,
            resumed_at: 0,
        });
        inner.stack[0] = CANARY;
        let me: *mut Inner = &mut *inner;
        // the stack as everos_switch_stacks leaves it: control words, six
        // registers (r12 holds the Fiber) and the return address. The
        // return address sits 8 bytes off a 16-byte boundary, so the
        // `call` in everos_fiber_start sees an aligned stack.
        let base = inner.stack.as_ptr() as usize;
        let mut top = (base + STACK_SIZE - 64) & !15;
        top -= 8; // return address slot, 8 mod 16
        let slot = top;
        let frame_start = slot - 7 * 8;
        let words_at = |addr: usize| (addr - base) / 8;
        let s = &mut inner.stack;
        s[words_at(slot)] = everos_fiber_start as *const () as usize as u64;
        // popped in order r15, r14, r13, r12, rbx, rbp
        s[words_at(frame_start + 8)] = 0; // r15
        s[words_at(frame_start + 16)] = 0; // r14
        s[words_at(frame_start + 24)] = 0; // r13
        s[words_at(frame_start + 32)] = me as u64; // r12
        s[words_at(frame_start + 40)] = 0; // rbx
        s[words_at(frame_start + 48)] = 0; // rbp
                                           // mxcsr (all exceptions masked) and the x87 control word
        s[words_at(frame_start)] = 0x1f80 | (0x037f << 32);
        inner.sp = frame_start;
        Fiber { inner }
    }

    /// Run the fiber until it pauses or ends. Returns true once it ended.
    pub fn resume(&mut self) -> bool {
        if self.inner.done {
            return true;
        }
        assert!(
            CURRENT.0.get().is_null(),
            "fiber: resumed from inside another fiber"
        );
        let me: *mut Inner = &mut *self.inner;
        CURRENT.0.set(me);
        unsafe {
            (*me).resumed_at = interrupts::ticks();
            everos_switch_stacks(&mut (*me).back, (*me).sp);
        }
        CURRENT.0.set(null_mut());
        if self.inner.stack[0] != CANARY {
            panic!("fiber: stack overflow");
        }
        self.inner.done
    }

    pub fn done(&self) -> bool {
        self.inner.done
    }

    /// Ask the fiber to stop: network waits fail from now on.
    pub fn cancel(&mut self) {
        self.inner.cancelled = true;
    }
}

impl Drop for Fiber {
    fn drop(&mut self) {
        if !self.inner.done {
            // its stack still holds live values: keep it rather than free
            // memory that is in use
            crate::serial::write_str("\nfiber: dropped before it ended, leaking its stack\n");
            let inner = core::mem::replace(
                &mut self.inner,
                Box::new(Inner {
                    stack: Vec::new(),
                    sp: 0,
                    back: 0,
                    entry: None,
                    done: true,
                    cancelled: true,
                    resumed_at: 0,
                }),
            );
            core::mem::forget(inner);
        }
    }
}

#[no_mangle]
extern "C" fn everos_fiber_main(me: *mut Inner) -> ! {
    unsafe {
        if let Some(f) = (*me).entry.take() {
            f();
        }
        (*me).done = true;
        let mut dead = 0usize;
        everos_switch_stacks(&mut dead, (*me).back);
    }
    unreachable!("fiber: resumed after it ended");
}

/// Whether the code running now is inside a fiber.
pub fn inside() -> bool {
    !CURRENT.0.get().is_null()
}

/// Whether the fiber running now was cancelled.
pub fn cancelled() -> bool {
    let me = CURRENT.0.get();
    !me.is_null() && unsafe { (*me).cancelled }
}

/// Hand the CPU back to whoever resumed this fiber. Outside a fiber this
/// does nothing. Never call it while holding a lock.
pub fn pause() {
    let me = CURRENT.0.get();
    if me.is_null() {
        return;
    }
    // the JavaScript engine keeps the running script in globals; the
    // desktop may run other pages' scripts while we are paused
    let js = crate::js::save_state();
    let before = interrupts::ticks();
    unsafe {
        CURRENT.0.set(null_mut());
        everos_switch_stacks(&mut (*me).sp, (*me).back);
        CURRENT.0.set(me);
        (*me).resumed_at = interrupts::ticks();
        crate::js::restore_state(js, interrupts::ticks() - before);
    }
}

/// Pause if this fiber has run for a whole slice.
pub fn pause_if_slice_used() {
    let me = CURRENT.0.get();
    if !me.is_null() && interrupts::ticks() >= unsafe { (*me).resumed_at } + SLICE_TICKS {
        pause();
    }
}
