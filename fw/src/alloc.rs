use core::{
    alloc::Layout,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};

pub struct BumpAllocator<const N: usize> {
    buffer: [u8; N],
    lock: AtomicBool,
    head: AtomicUsize,
}

impl<const N: usize> BumpAllocator<N> {
    pub const fn new() -> Self {
        Self {
            buffer: [0; N],
            lock: AtomicBool::new(false),
            head: AtomicUsize::new(0),
        }
    }

    /// Note: Will panic if called from an ISR context
    pub fn alloc<'a, T>(&'a self, init: T) -> &'a mut T {
        let layout = Layout::new::<T>().pad_to_align();

        if self.lock.load(Ordering::SeqCst) {
            panic!("alloc called in an ISR");
        }

        // Note that we assume a single-core system

        self.lock.store(true, Ordering::SeqCst);

        let head = self.head.load(Ordering::SeqCst);
        let p = &self.buffer[head] as *const u8;
        let head = head + layout.size();
        self.head.store(head, Ordering::SeqCst);

        self.lock.store(false, Ordering::SeqCst);

        unsafe {
            let p = p.add(p.align_offset(layout.align()));
            let p = p as *mut MaybeUninit<T>;
            let p = &mut *p;
            p.write(init)
        }
    }

    pub fn capacity(&self) -> usize {
        N
    }

    pub fn free(&self) -> usize {
        N - self.head.load(Ordering::SeqCst)
    }
}
