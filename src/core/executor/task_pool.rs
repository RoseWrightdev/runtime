use std::{
    alloc::Layout,
    cell::RefCell,
};

const MIN_BUCKET_SHIFT: u32 = 5;  // 2^5 = 32 bytes
const MAX_BUCKET_SHIFT: u32 = 15; // 2^15 = 32,768 bytes
pub(crate) const MAX_BUCKET_SIZE: usize = 1 << MAX_BUCKET_SHIFT;

pub(crate) struct Pool {
    // 11 buckets from 32B to 32KB
    buckets: [Vec<*mut u8>; 11],
}

impl Pool {
    pub fn new() -> Self {
        let buckets = [
            Vec::new(), Vec::new(), Vec::new(),
            Vec::new(), Vec::new(), Vec::new(),
            Vec::new(), Vec::new(), Vec::new(),
            Vec::new(), Vec::new(),
        ];
        Self { buckets }
    }

    #[inline(always)]
    pub(crate) fn bucket_index(size: usize) -> Option<usize> {
        if size == 0 {
            return None;
        }
        
        let size = size.max(1 << MIN_BUCKET_SHIFT);
        if size > MAX_BUCKET_SIZE {
            return None;
        }

        let power_of_two = size.next_power_of_two();
        let shift = power_of_two.trailing_zeros();
        
        Some((shift - MIN_BUCKET_SHIFT) as usize)
    }
    
    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        if let Some(index) = Self::bucket_index(layout.size()) {
            if layout.align() <= 16 {
                if let Some(ptr) = self.buckets[index].pop() {
                    return ptr; // Constant O(1) Vector pop!
                }
                
                // If bucket is empty, allocate directly
                let block_size = 1 << (index as u32 + MIN_BUCKET_SHIFT);
                let pool_layout = Layout::from_size_align(block_size, 16).unwrap();
                let ptr = unsafe { std::alloc::alloc(pool_layout) };
                if ptr.is_null() {
                    std::alloc::handle_alloc_error(pool_layout);
                }
                return ptr;
            }
        }
        
        let ptr = unsafe { std::alloc::alloc(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        ptr
    }

    pub fn deallocate(&mut self, ptr: *mut u8, layout: Layout) {
        if let Some(index) = Self::bucket_index(layout.size()) {
            if layout.align() <= 16 {
                self.buckets[index].push(ptr); // Constant O(1) Vector push!
                return;
            }
        }
        unsafe { std::alloc::dealloc(ptr, layout) };
    }
}
