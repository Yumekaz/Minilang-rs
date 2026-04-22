//! Custom memory allocator for MiniLang runtime.
//!
//! Implements multiple allocation strategies:
//! - Bump allocator: Fast, sequential allocation (no individual frees)
//! - Free-list allocator: General purpose with free support
//! - Slab allocator: Fixed-size object pools
//!
//! This demonstrates low-level memory management without relying on
//! Rust's standard allocator.

use std::alloc::{alloc, dealloc, Layout};
use std::cell::UnsafeCell;
use std::ptr::NonNull;

/// Alignment for all allocations (8 bytes for 64-bit systems)
const ALIGNMENT: usize = 8;

/// Align a size up to the given alignment
#[inline]
const fn align_up(size: usize, align: usize) -> usize {
    (size + align - 1) & !(align - 1)
}

// ============================================================================
// Bump Allocator
// ============================================================================

/// Fast bump allocator - allocates sequentially, frees all at once.
///
/// Perfect for:
/// - Compiler phases (parse, analyze, compile)
/// - Temporary allocations with known lifetime
/// - Arena-style allocation patterns
///
/// Performance: O(1) allocation, no fragmentation, cache-friendly
pub struct BumpAllocator {
    /// Start of the memory region
    start: *mut u8,
    /// Current allocation pointer
    ptr: UnsafeCell<*mut u8>,
    /// End of the memory region
    end: *mut u8,
    /// Total capacity in bytes
    capacity: usize,
    /// Number of allocations made
    allocation_count: UnsafeCell<usize>,
    /// Total bytes allocated
    bytes_allocated: UnsafeCell<usize>,
}

impl BumpAllocator {
    /// Create a new bump allocator with the given capacity
    pub fn new(capacity: usize) -> Self {
        let layout = Layout::from_size_align(capacity, ALIGNMENT).unwrap();
        let start = unsafe { alloc(layout) };
        if start.is_null() {
            panic!("Failed to allocate {} bytes for bump allocator", capacity);
        }

        Self {
            start,
            ptr: UnsafeCell::new(start),
            end: unsafe { start.add(capacity) },
            capacity,
            allocation_count: UnsafeCell::new(0),
            bytes_allocated: UnsafeCell::new(0),
        }
    }

    /// Allocate `size` bytes with default alignment
    #[inline]
    pub fn alloc(&self, size: usize) -> Option<NonNull<u8>> {
        self.alloc_aligned(size, ALIGNMENT)
    }

    /// Allocate `size` bytes with specified alignment
    pub fn alloc_aligned(&self, size: usize, align: usize) -> Option<NonNull<u8>> {
        let ptr = unsafe { *self.ptr.get() };

        // Align the current pointer
        let aligned = align_up(ptr as usize, align) as *mut u8;
        let new_ptr = unsafe { aligned.add(size) };

        if new_ptr > self.end {
            return None; // Out of memory
        }

        unsafe {
            *self.ptr.get() = new_ptr;
            *self.allocation_count.get() += 1;
            *self.bytes_allocated.get() += size;
        }

        NonNull::new(aligned)
    }

    /// Allocate and zero-initialize memory
    pub fn alloc_zeroed(&self, size: usize) -> Option<NonNull<u8>> {
        let ptr = self.alloc(size)?;
        unsafe {
            std::ptr::write_bytes(ptr.as_ptr(), 0, size);
        }
        Some(ptr)
    }

    /// Allocate space for a typed value
    pub fn alloc_typed<T>(&self) -> Option<NonNull<T>> {
        let ptr = self.alloc_aligned(std::mem::size_of::<T>(), std::mem::align_of::<T>())?;
        Some(ptr.cast())
    }

    /// Reset the allocator, freeing all allocations at once
    pub fn reset(&self) {
        unsafe {
            *self.ptr.get() = self.start;
            *self.allocation_count.get() = 0;
            *self.bytes_allocated.get() = 0;
        }
    }

    /// Get statistics about allocator usage
    pub fn stats(&self) -> AllocatorStats {
        let used = unsafe { (*self.ptr.get()) as usize - self.start as usize };
        AllocatorStats {
            capacity: self.capacity,
            used,
            free: self.capacity - used,
            allocation_count: unsafe { *self.allocation_count.get() },
            fragmentation: 0.0, // Bump allocator has no fragmentation
        }
    }
}

impl Drop for BumpAllocator {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.capacity, ALIGNMENT).unwrap();
        unsafe {
            dealloc(self.start, layout);
        }
    }
}

// Safety: BumpAllocator uses UnsafeCell for interior mutability but
// maintains correct aliasing through single-threaded access patterns
unsafe impl Send for BumpAllocator {}

// ============================================================================
// Free-List Allocator
// ============================================================================

/// Header for each allocated block
#[repr(C)]
struct BlockHeader {
    /// Size of this block (including header)
    size: usize,
    /// Pointer to next free block (null if allocated)
    next: *mut BlockHeader,
}

const HEADER_SIZE: usize = std::mem::size_of::<BlockHeader>();
const MIN_BLOCK_SIZE: usize = HEADER_SIZE + ALIGNMENT;

/// Free-list allocator with first-fit strategy.
///
/// Supports individual allocations and frees.
/// Good for general-purpose allocation with varying lifetimes.
pub struct FreeListAllocator {
    /// Start of the memory region
    start: *mut u8,
    /// Size of the memory region
    size: usize,
    /// Head of the free list
    free_list: UnsafeCell<*mut BlockHeader>,
    /// Statistics
    allocation_count: UnsafeCell<usize>,
    bytes_allocated: UnsafeCell<usize>,
}

impl FreeListAllocator {
    /// Create a new free-list allocator with the given size
    pub fn new(size: usize) -> Self {
        let layout = Layout::from_size_align(size, ALIGNMENT).unwrap();
        let start = unsafe { alloc(layout) };
        if start.is_null() {
            panic!("Failed to allocate {} bytes for free-list allocator", size);
        }

        // Initialize with one large free block
        let header = start as *mut BlockHeader;
        unsafe {
            (*header).size = size;
            (*header).next = std::ptr::null_mut();
        }

        Self {
            start,
            size,
            free_list: UnsafeCell::new(header),
            allocation_count: UnsafeCell::new(0),
            bytes_allocated: UnsafeCell::new(0),
        }
    }

    /// Allocate `size` bytes using first-fit strategy
    pub fn alloc(&self, size: usize) -> Option<NonNull<u8>> {
        let aligned_size = align_up(size + HEADER_SIZE, ALIGNMENT);
        let min_size = aligned_size.max(MIN_BLOCK_SIZE);

        unsafe {
            let mut prev: *mut BlockHeader = std::ptr::null_mut();
            let mut current = *self.free_list.get();

            // First-fit search
            while !current.is_null() {
                if (*current).size >= min_size {
                    // Found a suitable block
                    let remaining = (*current).size - min_size;

                    if remaining >= MIN_BLOCK_SIZE {
                        // Split the block
                        let new_block = (current as *mut u8).add(min_size) as *mut BlockHeader;
                        (*new_block).size = remaining;
                        (*new_block).next = (*current).next;
                        (*current).size = min_size;

                        // Update free list
                        if prev.is_null() {
                            *self.free_list.get() = new_block;
                        } else {
                            (*prev).next = new_block;
                        }
                    } else {
                        // Use the whole block
                        if prev.is_null() {
                            *self.free_list.get() = (*current).next;
                        } else {
                            (*prev).next = (*current).next;
                        }
                    }

                    (*current).next = std::ptr::null_mut(); // Mark as allocated

                    *self.allocation_count.get() += 1;
                    *self.bytes_allocated.get() += (*current).size;

                    // Return pointer after header
                    let data_ptr = (current as *mut u8).add(HEADER_SIZE);
                    return NonNull::new(data_ptr);
                }

                prev = current;
                current = (*current).next;
            }

            None // No suitable block found
        }
    }

    /// Free a previously allocated block
    ///
    /// # Safety
    /// The pointer must have been returned by `alloc` on this allocator
    pub unsafe fn free(&self, ptr: NonNull<u8>) {
        let header = (ptr.as_ptr() as *mut BlockHeader).sub(1);

        *self.allocation_count.get() -= 1;
        *self.bytes_allocated.get() -= (*header).size;

        // Insert into free list (sorted by address for coalescing)
        let mut prev: *mut BlockHeader = std::ptr::null_mut();
        let mut current = *self.free_list.get();

        while !current.is_null() && current < header {
            prev = current;
            current = (*current).next;
        }

        // Try to coalesce with next block
        let header_end = (header as *mut u8).add((*header).size) as *mut BlockHeader;
        if header_end == current {
            (*header).size += (*current).size;
            (*header).next = (*current).next;
        } else {
            (*header).next = current;
        }

        // Try to coalesce with previous block
        if !prev.is_null() {
            let prev_end = (prev as *mut u8).add((*prev).size) as *mut BlockHeader;
            if prev_end == header {
                (*prev).size += (*header).size;
                (*prev).next = (*header).next;
                return;
            }
        }

        // Insert into list
        if prev.is_null() {
            *self.free_list.get() = header;
        } else {
            (*prev).next = header;
        }
    }

    /// Get statistics about allocator usage
    pub fn stats(&self) -> AllocatorStats {
        let mut free_bytes = 0usize;
        let mut free_blocks = 0usize;

        unsafe {
            let mut current = *self.free_list.get();
            while !current.is_null() {
                free_bytes += (*current).size;
                free_blocks += 1;
                current = (*current).next;
            }
        }

        let used = self.size - free_bytes;
        let fragmentation = if free_blocks > 1 {
            1.0 - (1.0 / free_blocks as f64)
        } else {
            0.0
        };

        AllocatorStats {
            capacity: self.size,
            used,
            free: free_bytes,
            allocation_count: unsafe { *self.allocation_count.get() },
            fragmentation,
        }
    }
}

impl Drop for FreeListAllocator {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.size, ALIGNMENT).unwrap();
        unsafe {
            dealloc(self.start, layout);
        }
    }
}

unsafe impl Send for FreeListAllocator {}

// ============================================================================
// Slab Allocator
// ============================================================================

/// Slab allocator for fixed-size objects.
///
/// Extremely fast for allocating many objects of the same size.
/// Perfect for AST nodes, IR instructions, etc.
pub struct SlabAllocator {
    /// Size of each object
    object_size: usize,
    /// Number of objects per slab
    objects_per_slab: usize,
    /// List of slabs
    slabs: UnsafeCell<Vec<*mut u8>>,
    /// Free list head
    free_list: UnsafeCell<*mut *mut u8>,
    /// Statistics
    allocation_count: UnsafeCell<usize>,
}

impl SlabAllocator {
    /// Create a new slab allocator for objects of the given size
    pub fn new(object_size: usize, objects_per_slab: usize) -> Self {
        let aligned_size = align_up(object_size.max(std::mem::size_of::<*mut u8>()), ALIGNMENT);

        Self {
            object_size: aligned_size,
            objects_per_slab,
            slabs: UnsafeCell::new(Vec::new()),
            free_list: UnsafeCell::new(std::ptr::null_mut()),
            allocation_count: UnsafeCell::new(0),
        }
    }

    /// Allocate a new slab
    fn allocate_slab(&self) {
        let slab_size = self.object_size * self.objects_per_slab;
        let layout = Layout::from_size_align(slab_size, ALIGNMENT).unwrap();
        let slab = unsafe { alloc(layout) };
        if slab.is_null() {
            panic!("Failed to allocate slab of {} bytes", slab_size);
        }

        unsafe {
            (*self.slabs.get()).push(slab);

            // Initialize free list within slab
            for i in 0..self.objects_per_slab {
                let obj = slab.add(i * self.object_size) as *mut *mut u8;
                if i < self.objects_per_slab - 1 {
                    *obj = slab.add((i + 1) * self.object_size);
                } else {
                    *obj = (*self.free_list.get()) as *mut u8;
                }
            }
            *self.free_list.get() = slab as *mut *mut u8;
        }
    }

    /// Allocate an object
    pub fn alloc(&self) -> Option<NonNull<u8>> {
        unsafe {
            if (*self.free_list.get()).is_null() {
                self.allocate_slab();
            }

            let obj = *self.free_list.get();
            if obj.is_null() {
                return None;
            }

            *self.free_list.get() = *obj as *mut *mut u8;
            *self.allocation_count.get() += 1;

            NonNull::new(obj as *mut u8)
        }
    }

    /// Free an object
    ///
    /// # Safety
    /// The pointer must have been returned by `alloc` on this allocator
    pub unsafe fn free(&self, ptr: NonNull<u8>) {
        let obj = ptr.as_ptr() as *mut *mut u8;
        *obj = (*self.free_list.get()) as *mut u8;
        *self.free_list.get() = obj;
        *self.allocation_count.get() -= 1;
    }

    /// Get statistics
    pub fn stats(&self) -> AllocatorStats {
        let total_objects = unsafe { (*self.slabs.get()).len() * self.objects_per_slab };
        let allocated = unsafe { *self.allocation_count.get() };
        let capacity = total_objects * self.object_size;
        let used = allocated * self.object_size;

        AllocatorStats {
            capacity,
            used,
            free: capacity - used,
            allocation_count: allocated,
            fragmentation: 0.0, // Slab allocator has no external fragmentation
        }
    }
}

impl Drop for SlabAllocator {
    fn drop(&mut self) {
        let slab_size = self.object_size * self.objects_per_slab;
        let layout = Layout::from_size_align(slab_size, ALIGNMENT).unwrap();

        unsafe {
            for slab in (*self.slabs.get()).iter() {
                dealloc(*slab, layout);
            }
        }
    }
}

unsafe impl Send for SlabAllocator {}

// ============================================================================
// Statistics
// ============================================================================

/// Allocator statistics for profiling and debugging
#[derive(Debug, Clone)]
pub struct AllocatorStats {
    /// Total capacity in bytes
    pub capacity: usize,
    /// Bytes currently in use
    pub used: usize,
    /// Bytes available
    pub free: usize,
    /// Number of active allocations
    pub allocation_count: usize,
    /// Fragmentation ratio (0.0 = none, 1.0 = severe)
    pub fragmentation: f64,
}

impl std::fmt::Display for AllocatorStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Allocator Stats:\n\
             \x20 Capacity:     {} bytes\n\
             \x20 Used:         {} bytes ({:.1}%)\n\
             \x20 Free:         {} bytes\n\
             \x20 Allocations:  {}\n\
             \x20 Fragmentation: {:.1}%",
            self.capacity,
            self.used,
            (self.used as f64 / self.capacity as f64) * 100.0,
            self.free,
            self.allocation_count,
            self.fragmentation * 100.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bump_allocator() {
        let bump = BumpAllocator::new(1024);

        let p1 = bump.alloc(100).unwrap();
        let p2 = bump.alloc(200).unwrap();
        let p3 = bump.alloc(300).unwrap();

        // Allocations should be sequential
        assert!(p2.as_ptr() > p1.as_ptr());
        assert!(p3.as_ptr() > p2.as_ptr());

        let stats = bump.stats();
        assert_eq!(stats.allocation_count, 3);
        assert!(stats.used >= 600);

        // Reset and reuse
        bump.reset();
        let stats = bump.stats();
        assert_eq!(stats.allocation_count, 0);
        assert_eq!(stats.used, 0);
    }

    #[test]
    fn test_free_list_allocator() {
        let fl = FreeListAllocator::new(4096);

        let p1 = fl.alloc(100).unwrap();
        let p2 = fl.alloc(200).unwrap();
        let p3 = fl.alloc(100).unwrap();

        // Free middle allocation
        unsafe { fl.free(p2) };

        // Allocate again - should reuse freed space
        let p4 = fl.alloc(150).unwrap();

        let stats = fl.stats();
        assert_eq!(stats.allocation_count, 3);
    }

    #[test]
    fn test_slab_allocator() {
        let slab = SlabAllocator::new(64, 16);

        let mut ptrs = Vec::new();
        for _ in 0..32 {
            ptrs.push(slab.alloc().unwrap());
        }

        let stats = slab.stats();
        assert_eq!(stats.allocation_count, 32);

        // Free half
        for ptr in ptrs.drain(..16) {
            unsafe { slab.free(ptr) };
        }

        let stats = slab.stats();
        assert_eq!(stats.allocation_count, 16);
    }
}
