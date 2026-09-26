//! The kernel heap, so the network stack, the browser and the JavaScript
//! engine can use `Vec`, `String`, `Box` and `malloc`. It is a fixed block
//! of memory in .bss.
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

struct Allocator {
    big: Heap,
    free: [*mut u8; CLASSES.len()],
}

unsafe impl Send for Allocator {}

struct KernelHeap(IrqMutex<Allocator>);

#[global_allocator]
static ALLOCATOR: KernelHeap = KernelHeap(IrqMutex::new(Allocator {
    big: Heap::empty(),
    free: [null_mut(); CLASSES.len()],
}));

fn class_of(layout: &Layout) -> Option<usize> {
    if layout.align() > 16 {
        return None;
    }
    CLASSES.iter().position(|&c| c >= layout.size())
}

impl Allocator {
    unsafe fn alloc_small(&mut self, class: usize) -> *mut u8 {
        if self.free[class].is_null() {
            let size = CLASSES[class];
            let Ok(chunk) = self
                .big
                .allocate_first_fit(Layout::from_size_align_unchecked(CHUNK, 16))
            else {
                return null_mut();
            };
            let base = chunk.as_ptr();
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
            None => a
                .big
                .allocate_first_fit(layout)
                .map_or(null_mut(), |p| p.as_ptr()),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut a = self.0.lock();
        match class_of(&layout) {
            Some(class) => {
                *(ptr as *mut *mut u8) = a.free[class];
                a.free[class] = ptr;
            }
            None => a.big.deallocate(NonNull::new_unchecked(ptr), layout),
        }
    }
}

pub fn init() {
    unsafe {
        let start = addr_of_mut!(HEAP_MEMORY.0) as *mut u8;
        ALLOCATOR.0.lock().big.init(start, HEAP_SIZE);
    }
}

/// Bytes the big allocator has free (blocks on the small free lists are
/// not counted), for deciding whether there is room for more.
pub fn free_bytes() -> usize {
    ALLOCATOR.0.lock().big.free()
}
