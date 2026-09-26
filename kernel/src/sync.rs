//! Synchronisation primitives for a single-CPU kernel.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A spinlock that also disables interrupts while it is held, so an
/// interrupt handler can never deadlock on a lock the interrupted code
/// holds.
pub struct IrqMutex<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for IrqMutex<T> {}

impl<T> IrqMutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> IrqMutexGuard<'_, T> {
        let interrupts_were_on = crate::interrupts::disable();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        IrqMutexGuard {
            mutex: self,
            interrupts_were_on,
        }
    }

    /// Release the lock no matter who holds it. Only for the panic path,
    /// where the holder will never run again.
    ///
    /// # Safety
    /// Nothing may still be using the data through an old guard.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

pub struct IrqMutexGuard<'a, T> {
    mutex: &'a IrqMutex<T>,
    interrupts_were_on: bool,
}

impl<T> Deref for IrqMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for IrqMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for IrqMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.locked.store(false, Ordering::Release);
        if self.interrupts_were_on {
            crate::interrupts::enable();
        }
    }
}

/// Byte queue with one producer (an interrupt handler) and one consumer
/// (the main loop). When it is full, new bytes are dropped.
pub struct ByteQueue {
    buffer: UnsafeCell<[u8; Self::SIZE]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

unsafe impl Sync for ByteQueue {}

impl ByteQueue {
    const SIZE: usize = 256;

    pub const fn new() -> Self {
        Self {
            buffer: UnsafeCell::new([0; Self::SIZE]),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Called by the producer only.
    pub fn push(&self, byte: u8) {
        let tail = self.tail.load(Ordering::Relaxed);
        let next = (tail + 1) % Self::SIZE;
        if next == self.head.load(Ordering::Acquire) {
            return;
        }
        unsafe { (*self.buffer.get())[tail] = byte };
        self.tail.store(next, Ordering::Release);
    }

    /// Called by the consumer only.
    pub fn pop(&self) -> Option<u8> {
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }
        let byte = unsafe { (*self.buffer.get())[head] };
        self.head.store((head + 1) % Self::SIZE, Ordering::Release);
        Some(byte)
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) == self.tail.load(Ordering::Acquire)
    }
}

/// A large zeroed array in .bss that can be borrowed exactly once, for
/// buffers too big for the stack (the kernel has no heap).
pub struct StaticBuffer<const N: usize> {
    taken: AtomicBool,
    data: UnsafeCell<[u32; N]>,
}

unsafe impl<const N: usize> Sync for StaticBuffer<N> {}

impl<const N: usize> StaticBuffer<N> {
    pub const fn new() -> Self {
        Self {
            taken: AtomicBool::new(false),
            data: UnsafeCell::new([0; N]),
        }
    }

    /// Panics if called twice, so there is only ever one `&mut`.
    #[allow(clippy::mut_from_ref)]
    pub fn take(&'static self) -> &'static mut [u32] {
        assert!(
            !self.taken.swap(true, Ordering::AcqRel),
            "buffer taken twice"
        );
        unsafe { &mut *self.data.get() }
    }
}
