//! The kernel heap, so the network stack, the browser and the JavaScript
//! engine can use `Vec`, `String`, `Box` and `malloc`. It starts as a
//! fixed block of memory in .bss; once the BIOS memory map is read, the
//! rest of the computer's RAM (below 4 GiB, where the page tables map
//! everything) is added to it, so big web pages have room.
//!
//! Small blocks come from per-size free lists, which makes the many tiny
//! allocations of JavaScript objects and strings cheap. Everything else
//! goes to a first-fit linked list allocator.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{addr_of_mut, null_mut, NonNull};

use linked_list_allocator::Heap;

use crate::sync::IrqMutex;

const HEAP_SIZE: usize = 128 * 1024 * 1024;

#[repr(align(4096))]
struct HeapMemory([u8; HEAP_SIZE]);

static mut HEAP_MEMORY: HeapMemory = HeapMemory([0; HEAP_SIZE]);

/// Block sizes served from free lists. All are multiples of 16, so every
/// block is 16-byte aligned.
const CLASSES: [usize; 16] = [
    16, 32, 48, 64, 80, 96, 128, 160, 192, 256, 320, 384, 512, 768, 1024, 2048,
];
/// Small blocks are carved out of chunks this big.
const CHUNK: usize = 64 * 1024;

/// RAM areas added after boot, each its own heap.
const MAX_EXTRA: usize = 4;
/// Areas smaller than this are not worth a heap.
const MIN_EXTRA: usize = 4 * 1024 * 1024;
/// The page tables map the first 4 GiB.
const MAPPED: u64 = 4 << 30;

struct Allocator {
    big: Heap,
    extra: [Heap; MAX_EXTRA],
    free: [*mut u8; CLASSES.len()],
}

unsafe impl Send for Allocator {}

struct KernelHeap(IrqMutex<Allocator>);

#[global_allocator]
static ALLOCATOR: KernelHeap = KernelHeap(IrqMutex::new(Allocator {
    big: Heap::empty(),
    extra: [Heap::empty(), Heap::empty(), Heap::empty(), Heap::empty()],
    free: [null_mut(); CLASSES.len()],
}));

fn class_of(layout: &Layout) -> Option<usize> {
    if layout.align() > 16 {
        return None;
    }
    CLASSES.iter().position(|&c| c >= layout.size())
}

impl Allocator {
    /// A block from the boot heap, or else from the RAM added later.
    fn alloc_big(&mut self, layout: Layout) -> *mut u8 {
        if let Ok(p) = self.big.allocate_first_fit(layout) {
            return p.as_ptr();
        }
        for h in self.extra.iter_mut() {
            if h.size() > 0 {
                if let Ok(p) = h.allocate_first_fit(layout) {
                    return p.as_ptr();
                }
            }
        }
        null_mut()
    }

    unsafe fn dealloc_big(&mut self, ptr: *mut u8, layout: Layout) {
        let addr = ptr as usize;
        for h in self.extra.iter_mut() {
            if h.size() > 0 && addr >= h.bottom() as usize && addr < h.top() as usize {
                h.deallocate(NonNull::new_unchecked(ptr), layout);
                return;
            }
        }
        self.big.deallocate(NonNull::new_unchecked(ptr), layout);
    }

    unsafe fn alloc_small(&mut self, class: usize) -> *mut u8 {
        if self.free[class].is_null() {
            let size = CLASSES[class];
            let base = self.alloc_big(Layout::from_size_align_unchecked(CHUNK, 16));
            if base.is_null() {
                return null_mut();
            }
            // thread the new blocks onto the free list
            let n = CHUNK / size;
            for i in 0..n {
                let block = base.add(i * size);
                let next = if i + 1 < n {
                    base.add((i + 1) * size)
                } else {
                    null_mut()
                };
                *(block as *mut *mut u8) = next;
            }
            self.free[class] = base;
        }
        let block = self.free[class];
        self.free[class] = *(block as *mut *mut u8);
        block
    }
}

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut a = self.0.lock();
        match class_of(&layout) {
            Some(class) => a.alloc_small(class),
            None => a.alloc_big(layout),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut a = self.0.lock();
        match class_of(&layout) {
            Some(class) => {
                *(ptr as *mut *mut u8) = a.free[class];
                a.free[class] = ptr;
            }
            None => a.dealloc_big(ptr, layout),
        }
    }
}

pub fn init() {
    unsafe {
        let start = addr_of_mut!(HEAP_MEMORY.0) as *mut u8;
        ALLOCATOR.0.lock().big.init(start, HEAP_SIZE);
    }
}

extern "C" {
    /// The end of the kernel's image and .bss, from linker.ld.
    static _kernel_end: u8;
}

/// Add the free RAM from the BIOS memory map to the heap, leaving out the
/// kernel, everything below 1 MiB and the multiboot information (`keep`,
/// as start and length), which the boot info still points into.
/// GRUB's modules have been copied by now, so their memory is free.
pub fn grow(ram: &[(u64, u64)], keep: (usize, usize)) {
    let kernel_end = core::ptr::addr_of!(_kernel_end) as u64;
    let (keep_start, keep_end) = (keep.0 as u64, (keep.0 + keep.1) as u64);
    let mut a = ALLOCATOR.0.lock();
    let mut n = 0;
    let mut add = |start: u64, end: u64, a: &mut Allocator| {
        let start = (start + 4095) & !4095;
        let end = end & !4095;
        if n < MAX_EXTRA && end > start && (end - start) as usize >= MIN_EXTRA {
            unsafe { a.extra[n].init(start as *mut u8, (end - start) as usize) };
            n += 1;
        }
    };
    for &(base, len) in ram {
        let start = base.max(kernel_end).max(1 << 20);
        let end = (base + len).min(MAPPED);
        if end <= start {
            continue;
        }
        // around the multiboot information
        if keep_end > start && keep_start < end {
            add(start, keep_start, &mut a);
            add(keep_end, end, &mut a);
        } else {
            add(start, end, &mut a);
        }
    }
}

/// The size of the kernel heap.
pub fn total_bytes() -> usize {
    let a = ALLOCATOR.0.lock();
    a.big.size() + a.extra.iter().map(|h| h.size()).sum::<usize>()
}

/// Bytes the big allocator has free (blocks on the small free lists are
/// not counted), for deciding whether there is room for more.
pub fn free_bytes() -> usize {
    let a = ALLOCATOR.0.lock();
    a.big.free() + a.extra.iter().map(|h| h.free()).sum::<usize>()
}
