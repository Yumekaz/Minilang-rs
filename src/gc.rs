//! Mark-Sweep Garbage Collector for MiniLang runtime.
//!
//! Implements a simple but complete tracing garbage collector:
//! - Mark phase: Traverse from roots, marking all reachable objects
//! - Sweep phase: Free all unmarked objects
//!
//! This demonstrates GC concepts relevant to systems programming:
//! - Object headers and metadata
//! - Root set management
//! - Memory reclamation without reference counting

use std::alloc::{alloc, dealloc, Layout};
use std::collections::HashSet;
use std::ptr::NonNull;

/// Object header stored before each GC-managed object
#[repr(C)]
struct GcHeader {
    /// Size of the object (not including header)
    size: usize,
    /// Mark bit for GC
    marked: bool,
    /// Type tag for debugging/introspection
    type_tag: TypeTag,
    /// Next object in the all-objects list
    next: *mut GcHeader,
}

/// Type tags for GC-managed objects
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TypeTag {
    /// Integer value (boxed)
    Int = 0,
    /// Boolean value (boxed)
    Bool = 1,
    /// Array of integers
    IntArray = 2,
    /// Closure/function object
    Closure = 3,
    /// Generic blob
    Blob = 4,
}

const HEADER_SIZE: usize = std::mem::size_of::<GcHeader>();
const ALIGNMENT: usize = 8;

/// Align size up to alignment boundary
#[inline]
const fn align_up(size: usize, align: usize) -> usize {
    (size + align - 1) & !(align - 1)
}

/// A GC-managed pointer
#[derive(Debug, Clone, Copy)]
pub struct GcPtr<T: ?Sized> {
    ptr: NonNull<T>,
}

impl<T: ?Sized> GcPtr<T> {
    /// Get the raw pointer
    pub fn as_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// Get a reference to the value
    ///
    /// # Safety
    /// The GC must not have collected this object
    pub unsafe fn as_ref(&self) -> &T {
        self.ptr.as_ref()
    }

    /// Get a mutable reference to the value
    ///
    /// # Safety
    /// The GC must not have collected this object
    pub unsafe fn as_mut(&mut self) -> &mut T {
        self.ptr.as_mut()
    }
}

/// Statistics about GC behavior
#[derive(Debug, Clone, Default)]
pub struct GcStats {
    /// Total bytes allocated
    pub bytes_allocated: usize,
    /// Total bytes freed
    pub bytes_freed: usize,
    /// Number of objects allocated
    pub objects_allocated: usize,
    /// Number of objects freed
    pub objects_freed: usize,
    /// Number of GC cycles run
    pub gc_cycles: usize,
    /// Total time spent in GC (nanoseconds)
    pub gc_time_ns: u64,
}

/// Mark-Sweep Garbage Collector
pub struct GarbageCollector {
    /// Head of the all-objects list
    all_objects: *mut GcHeader,
    /// Root set (stack slots, global variables)
    roots: HashSet<*mut u8>,
    /// Threshold for triggering GC (bytes)
    gc_threshold: usize,
    /// Current bytes allocated
    bytes_allocated: usize,
    /// Statistics
    stats: GcStats,
}

impl GarbageCollector {
    /// Create a new garbage collector
    pub fn new(gc_threshold: usize) -> Self {
        Self {
            all_objects: std::ptr::null_mut(),
            roots: HashSet::new(),
            gc_threshold,
            bytes_allocated: 0,
            stats: GcStats::default(),
        }
    }

    /// Allocate a new GC-managed object
    pub fn alloc(&mut self, size: usize, type_tag: TypeTag) -> Option<NonNull<u8>> {
        // Check if we need to collect
        if self.bytes_allocated + size > self.gc_threshold {
            self.collect();
        }

        let total_size = align_up(HEADER_SIZE + size, ALIGNMENT);
        let layout = Layout::from_size_align(total_size, ALIGNMENT).ok()?;

        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            // Try GC and allocate again
            self.collect();
            let ptr = unsafe { alloc(layout) };
            if ptr.is_null() {
                return None;
            }
        }

        // Initialize header
        let header = ptr as *mut GcHeader;
        unsafe {
            (*header).size = size;
            (*header).marked = false;
            (*header).type_tag = type_tag;
            (*header).next = self.all_objects;
        }

        // Add to all-objects list
        self.all_objects = header;
        self.bytes_allocated += total_size;
        self.stats.bytes_allocated += total_size;
        self.stats.objects_allocated += 1;

        // Return pointer to data (after header)
        NonNull::new(unsafe { ptr.add(HEADER_SIZE) })
    }

    /// Allocate a typed object
    pub fn alloc_typed<T>(&mut self, type_tag: TypeTag) -> Option<GcPtr<T>> {
        let ptr = self.alloc(std::mem::size_of::<T>(), type_tag)?;
        Some(GcPtr { ptr: ptr.cast() })
    }

    /// Allocate an integer array
    pub fn alloc_int_array(&mut self, len: usize) -> Option<GcPtr<[i32]>> {
        let size = len * std::mem::size_of::<i32>();
        let ptr = self.alloc(size, TypeTag::IntArray)?;

        // Zero-initialize
        unsafe {
            std::ptr::write_bytes(ptr.as_ptr(), 0, size);
        }

        // Create fat pointer
        let slice_ptr = std::ptr::slice_from_raw_parts_mut(ptr.as_ptr() as *mut i32, len);
        Some(GcPtr {
            ptr: NonNull::new(slice_ptr as *mut [i32]).unwrap(),
        })
    }

    /// Add a root to the root set
    pub fn add_root(&mut self, ptr: *mut u8) {
        self.roots.insert(ptr);
    }

    /// Remove a root from the root set
    pub fn remove_root(&mut self, ptr: *mut u8) {
        self.roots.remove(&ptr);
    }

    /// Clear all roots
    pub fn clear_roots(&mut self) {
        self.roots.clear();
    }

    /// Run garbage collection
    pub fn collect(&mut self) {
        let start = std::time::Instant::now();

        // Mark phase
        self.mark();

        // Sweep phase
        self.sweep();

        self.stats.gc_cycles += 1;
        self.stats.gc_time_ns += start.elapsed().as_nanos() as u64;
    }

    /// Mark phase: traverse from roots and mark all reachable objects
    fn mark(&mut self) {
        // Collect roots first to avoid borrow conflict
        let roots: Vec<*mut u8> = self.roots.iter().copied().collect();
        // Mark from roots
        for root in roots {
            self.mark_object(root);
        }
    }

    /// Mark a single object and its children
    fn mark_object(&mut self, ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }

        // Get header
        let header = unsafe { (ptr as *mut GcHeader).sub(1) };

        // Verify this is a valid GC object by checking if it's in our list
        if !self.is_gc_object(header) {
            return;
        }

        unsafe {
            // Already marked?
            if (*header).marked {
                return;
            }

            // Mark this object
            (*header).marked = true;

            // Mark children based on type
            match (*header).type_tag {
                TypeTag::IntArray => {
                    // Arrays of ints have no pointers
                }
                TypeTag::Int | TypeTag::Bool | TypeTag::Blob => {
                    // Primitive types have no children
                }
                TypeTag::Closure => {
                    // Closures might reference other objects
                    // For now, we don't have closures in MiniLang
                }
            }
        }
    }

    /// Check if a header is in our all-objects list
    fn is_gc_object(&self, header: *mut GcHeader) -> bool {
        let mut current = self.all_objects;
        while !current.is_null() {
            if current == header {
                return true;
            }
            current = unsafe { (*current).next };
        }
        false
    }

    /// Sweep phase: free all unmarked objects
    fn sweep(&mut self) {
        let mut prev: *mut GcHeader = std::ptr::null_mut();
        let mut current = self.all_objects;

        while !current.is_null() {
            let next = unsafe { (*current).next };

            if unsafe { (*current).marked } {
                // Object is alive, clear mark for next cycle
                unsafe { (*current).marked = false };
                prev = current;
            } else {
                // Object is garbage, free it
                let size = unsafe { (*current).size };
                let total_size = align_up(HEADER_SIZE + size, ALIGNMENT);

                // Remove from list
                if prev.is_null() {
                    self.all_objects = next;
                } else {
                    unsafe { (*prev).next = next };
                }

                // Free memory
                let layout = Layout::from_size_align(total_size, ALIGNMENT).unwrap();
                unsafe { dealloc(current as *mut u8, layout) };

                self.bytes_allocated -= total_size;
                self.stats.bytes_freed += total_size;
                self.stats.objects_freed += 1;
            }

            current = next;
        }
    }

    /// Get current statistics
    pub fn stats(&self) -> &GcStats {
        &self.stats
    }

    /// Get bytes currently allocated
    pub fn bytes_allocated(&self) -> usize {
        self.bytes_allocated
    }

    /// Force a collection regardless of threshold
    pub fn force_collect(&mut self) {
        self.collect();
    }
}

impl Drop for GarbageCollector {
    fn drop(&mut self) {
        // Free all remaining objects
        let mut current = self.all_objects;
        while !current.is_null() {
            let next = unsafe { (*current).next };
            let size = unsafe { (*current).size };
            let total_size = align_up(HEADER_SIZE + size, ALIGNMENT);
            let layout = Layout::from_size_align(total_size, ALIGNMENT).unwrap();
            unsafe { dealloc(current as *mut u8, layout) };
            current = next;
        }
    }
}

impl std::fmt::Display for GcStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GC Statistics:\n\
             \x20 Objects allocated: {}\n\
             \x20 Objects freed:     {}\n\
             \x20 Bytes allocated:   {}\n\
             \x20 Bytes freed:       {}\n\
             \x20 GC cycles:         {}\n\
             \x20 GC time:           {:.2}ms",
            self.objects_allocated,
            self.objects_freed,
            self.bytes_allocated,
            self.bytes_freed,
            self.gc_cycles,
            self.gc_time_ns as f64 / 1_000_000.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_allocation() {
        let mut gc = GarbageCollector::new(1024 * 1024);

        let ptr = gc.alloc(100, TypeTag::Blob).unwrap();
        assert!(!ptr.as_ptr().is_null());
        assert!(gc.bytes_allocated() > 0);
    }

    #[test]
    fn test_gc_with_roots() {
        let mut gc = GarbageCollector::new(1024 * 1024);

        // Allocate some objects
        let ptr1 = gc.alloc(64, TypeTag::Blob).unwrap();
        let ptr2 = gc.alloc(64, TypeTag::Blob).unwrap();
        let ptr3 = gc.alloc(64, TypeTag::Blob).unwrap();

        // Only root ptr1 and ptr3
        gc.add_root(ptr1.as_ptr());
        gc.add_root(ptr3.as_ptr());

        let _ = ptr2; // Silence warning

        // Collect - ptr2 should be freed
        gc.collect();

        assert_eq!(gc.stats().objects_freed, 1);
    }

    #[test]
    fn test_gc_array_allocation() {
        let mut gc = GarbageCollector::new(1024 * 1024);

        let mut arr = gc.alloc_int_array(10).unwrap();

        // Write to array
        unsafe {
            let slice = arr.as_mut();
            for i in 0..10 {
                slice[i] = i as i32;
            }
        }

        // Verify
        unsafe {
            let slice = arr.as_ref();
            for i in 0..10 {
                assert_eq!(slice[i], i as i32);
            }
        }
    }

    #[test]
    fn test_gc_stress() {
        let mut gc = GarbageCollector::new(4096); // Small threshold to trigger GC

        // Allocate many objects, only keeping some rooted
        for i in 0..100 {
            let ptr = gc.alloc(64, TypeTag::Blob).unwrap();
            if i % 3 == 0 {
                gc.add_root(ptr.as_ptr());
            }
        }

        // Should have triggered multiple GC cycles
        assert!(gc.stats().gc_cycles > 0);
    }
}
