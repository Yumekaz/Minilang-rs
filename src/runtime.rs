//! GC-managed runtime values for MiniLang.
//!
//! This module provides heap-allocated values that are managed by the garbage collector.
//! Demonstrates real memory management where the GC tracks and collects objects.
//!
//! Objects managed by GC:
//! - Arrays (heap-allocated, variable size)
//! - Closures (future: for first-class functions)
//! - Strings (future: for string support)

use crate::gc::{GarbageCollector, GcPtr, TypeTag};
use std::ptr::NonNull;

/// Runtime value that can be either immediate or heap-allocated
#[derive(Clone, Copy, PartialEq)]
pub enum Value {
    /// Immediate integer (unboxed, no GC)
    Int(i64),
    /// Immediate boolean (unboxed, no GC)
    Bool(bool),
    /// Heap-allocated array (GC-managed)
    Array(GcArray),
    /// Null/uninitialized
    Null,
}

impl Value {
    /// Check if this is a GC-managed value
    pub fn is_gc_managed(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    /// Get the raw pointer if GC-managed (for root tracking)
    pub fn gc_ptr(&self) -> Option<*mut u8> {
        match self {
            Value::Array(arr) => Some(arr.ptr.as_ptr() as *mut u8),
            _ => None,
        }
    }

    /// Convert to i64 (for stack operations)
    pub fn to_i64(&self) -> i64 {
        match self {
            Value::Int(i) => *i,
            Value::Bool(b) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            Value::Array(arr) => arr.ptr.as_ptr() as i64, // Pointer as int
            Value::Null => 0,
        }
    }

    /// Convert from i64 (for stack operations)
    pub fn from_i64(v: i64) -> Self {
        Value::Int(v)
    }

    /// Check if truthy (for conditions)
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Int(i) => *i != 0,
            Value::Bool(b) => *b,
            Value::Array(_) => true, // Non-null array is truthy
            Value::Null => false,
        }
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(i) => write!(f, "Int({})", i),
            Value::Bool(b) => write!(f, "Bool({})", b),
            Value::Array(arr) => write!(f, "Array(len={})", arr.len),
            Value::Null => write!(f, "Null"),
        }
    }
}

impl Default for Value {
    fn default() -> Self {
        Value::Null
    }
}

/// GC-managed array header
#[repr(C)]
pub struct ArrayHeader {
    /// Length of the array
    pub len: usize,
    /// Capacity (for future growth support)
    pub cap: usize,
}

/// GC-managed array
#[derive(Clone, Copy, PartialEq)]
pub struct GcArray {
    /// Pointer to array data (after header)
    pub ptr: NonNull<i64>,
    /// Length
    pub len: usize,
}

impl GcArray {
    /// Allocate a new array from the GC
    pub fn new(gc: &mut GarbageCollector, len: usize) -> Option<Self> {
        // Allocate space for header + data
        let header_size = std::mem::size_of::<ArrayHeader>();
        let data_size = len * std::mem::size_of::<i64>();
        let total_size = header_size + data_size;

        let raw = gc.alloc(total_size, TypeTag::IntArray)?;

        // Initialize header
        let header_ptr = raw.as_ptr() as *mut ArrayHeader;
        unsafe {
            (*header_ptr).len = len;
            (*header_ptr).cap = len;
        }

        // Get data pointer (after header)
        let data_ptr = unsafe { raw.as_ptr().add(header_size) as *mut i64 };

        // Zero-initialize the array
        unsafe {
            std::ptr::write_bytes(data_ptr, 0, len);
        }

        Some(Self {
            ptr: NonNull::new(data_ptr).unwrap(),
            len,
        })
    }

    /// Get element at index
    pub fn get(&self, index: usize) -> Option<i64> {
        if index >= self.len {
            return None;
        }
        unsafe { Some(*self.ptr.as_ptr().add(index)) }
    }

    /// Set element at index
    pub fn set(&mut self, index: usize, value: i64) -> bool {
        if index >= self.len {
            return false;
        }
        unsafe {
            *self.ptr.as_ptr().add(index) = value;
        }
        true
    }

    /// Get as slice
    pub fn as_slice(&self) -> &[i64] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// Get as mutable slice
    pub fn as_mut_slice(&mut self) -> &mut [i64] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Get the base pointer for GC root tracking
    pub fn base_ptr(&self) -> *mut u8 {
        // Go back to the header
        let header_size = std::mem::size_of::<ArrayHeader>();
        unsafe { (self.ptr.as_ptr() as *mut u8).sub(header_size) }
    }
}

impl std::fmt::Debug for GcArray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GcArray({:?})", self.as_slice())
    }
}

/// Value stack that tracks GC roots
pub struct ValueStack {
    values: Vec<Value>,
    /// Maximum depth
    max_depth: usize,
}

impl ValueStack {
    pub fn new(max_depth: usize) -> Self {
        Self {
            values: Vec::with_capacity(max_depth.min(1024)),
            max_depth,
        }
    }

    pub fn push(&mut self, value: Value) -> bool {
        if self.values.len() >= self.max_depth {
            return false;
        }
        self.values.push(value);
        true
    }

    pub fn pop(&mut self) -> Option<Value> {
        self.values.pop()
    }

    pub fn peek(&self) -> Option<&Value> {
        self.values.last()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Get all GC-managed pointers for root tracking
    pub fn gc_roots(&self) -> Vec<*mut u8> {
        self.values.iter().filter_map(|v| v.gc_ptr()).collect()
    }

    /// Clear the stack
    pub fn clear(&mut self) {
        self.values.clear();
    }
}

/// Local variable frame with GC root tracking
pub struct LocalFrame {
    /// Local values
    values: Vec<Value>,
    /// Initialization flags
    init_flags: Vec<bool>,
    /// Return PC
    pub return_pc: usize,
    /// Function ID
    pub func_id: usize,
}

impl LocalFrame {
    pub fn new(local_count: usize, return_pc: usize, func_id: usize) -> Self {
        Self {
            values: vec![Value::Null; local_count],
            init_flags: vec![false; local_count],
            return_pc,
            func_id,
        }
    }

    /// Get local value (returns None if uninitialized)
    pub fn get(&self, slot: usize) -> Option<Value> {
        if slot >= self.values.len() || !self.init_flags[slot] {
            return None;
        }
        Some(self.values[slot])
    }

    /// Set local value (marks as initialized)
    pub fn set(&mut self, slot: usize, value: Value) -> bool {
        if slot >= self.values.len() {
            return false;
        }
        self.values[slot] = value;
        self.init_flags[slot] = true;
        true
    }

    /// Get GC roots from this frame
    pub fn gc_roots(&self) -> Vec<*mut u8> {
        self.values
            .iter()
            .zip(self.init_flags.iter())
            .filter(|(_, init)| **init)
            .filter_map(|(v, _)| v.gc_ptr())
            .collect()
    }

    /// Initialize a slot without setting value (for parameters)
    pub fn init_slot(&mut self, slot: usize) {
        if slot < self.init_flags.len() {
            self.init_flags[slot] = true;
        }
    }
}

/// Global variables with GC root tracking
pub struct GlobalStore {
    values: Vec<Value>,
}

impl GlobalStore {
    pub fn new(size: usize) -> Self {
        Self {
            values: vec![Value::Int(0); size], // All initialized to 0
        }
    }

    pub fn get(&self, slot: usize) -> Option<Value> {
        self.values.get(slot).copied()
    }

    pub fn set(&mut self, slot: usize, value: Value) -> bool {
        if slot >= self.values.len() {
            return false;
        }
        self.values[slot] = value;
        true
    }

    /// Get GC roots
    pub fn gc_roots(&self) -> Vec<*mut u8> {
        self.values.iter().filter_map(|v| v.gc_ptr()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_array() {
        let mut gc = GarbageCollector::new(1024 * 1024);

        let mut arr = GcArray::new(&mut gc, 10).unwrap();

        // Set values
        for i in 0..10 {
            assert!(arr.set(i, (i * i) as i64));
        }

        // Get values
        for i in 0..10 {
            assert_eq!(arr.get(i), Some((i * i) as i64));
        }

        // Out of bounds
        assert_eq!(arr.get(10), None);
        assert!(!arr.set(10, 0));
    }

    #[test]
    fn test_value_stack() {
        let mut stack = ValueStack::new(100);

        stack.push(Value::Int(42));
        stack.push(Value::Bool(true));

        assert_eq!(stack.len(), 2);

        if let Some(Value::Bool(b)) = stack.pop() {
            assert!(b);
        } else {
            panic!("expected bool");
        }

        if let Some(Value::Int(i)) = stack.pop() {
            assert_eq!(i, 42);
        } else {
            panic!("expected int");
        }
    }

    #[test]
    fn test_local_frame() {
        let mut frame = LocalFrame::new(5, 0, 0);

        // Uninitialized read should fail
        assert_eq!(frame.get(0), None);

        // Set and read
        frame.set(0, Value::Int(42));
        assert_eq!(frame.get(0), Some(Value::Int(42)));
    }
}
