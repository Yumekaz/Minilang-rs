//! Arena-allocated AST nodes.
//!
//! This module provides AST nodes that are allocated from a bump allocator (arena).
//! This demonstrates real systems programming where we control memory layout
//! and allocation patterns.
//!
//! Benefits:
//! - Cache-friendly allocation (nodes are contiguous in memory)
//! - Fast allocation (O(1) bump allocation)
//! - Fast deallocation (free entire arena at once)
//! - No individual node deallocation overhead

use crate::alloc::BumpAllocator;
use crate::token::Span;
use std::ptr::NonNull;

/// Type information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Void,
    Array(usize), // Array with fixed size
    Error,
}

/// Binary operators
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

/// Unary operators
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

/// Arena-allocated string (pointer + length into arena)
#[derive(Clone, Copy)]
pub struct ArenaStr {
    ptr: NonNull<u8>,
    len: usize,
}

impl ArenaStr {
    /// Create a new arena string from a Rust string
    pub fn new(arena: &BumpAllocator, s: &str) -> Option<Self> {
        let len = s.len();
        if len == 0 {
            // Use a dangling pointer for empty strings
            return Some(Self {
                ptr: NonNull::dangling(),
                len: 0,
            });
        }

        let ptr = arena.alloc(len)?;
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), ptr.as_ptr(), len);
        }
        Some(Self { ptr, len })
    }

    /// Get as a string slice
    pub fn as_str(&self) -> &str {
        if self.len == 0 {
            return "";
        }
        unsafe {
            let slice = std::slice::from_raw_parts(self.ptr.as_ptr(), self.len);
            std::str::from_utf8_unchecked(slice)
        }
    }

    /// Get length
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl std::fmt::Debug for ArenaStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl PartialEq for ArenaStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for ArenaStr {}

impl PartialEq<str> for ArenaStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

/// Arena-allocated vector (pointer + length + capacity)
pub struct ArenaVec<T> {
    ptr: NonNull<T>,
    len: usize,
    cap: usize,
    _marker: std::marker::PhantomData<T>,
}

impl<T> ArenaVec<T> {
    /// Create a new empty arena vec with given capacity
    pub fn with_capacity(arena: &BumpAllocator, cap: usize) -> Option<Self> {
        if cap == 0 {
            return Some(Self {
                ptr: NonNull::dangling(),
                len: 0,
                cap: 0,
                _marker: std::marker::PhantomData,
            });
        }

        let size = cap * std::mem::size_of::<T>();
        let align = std::mem::align_of::<T>();
        let ptr = arena.alloc_aligned(size, align)?;

        Some(Self {
            ptr: ptr.cast(),
            len: 0,
            cap,
            _marker: std::marker::PhantomData,
        })
    }

    /// Push an item (panics if full - arena vecs are fixed capacity)
    pub fn push(&mut self, item: T) {
        assert!(self.len < self.cap, "ArenaVec capacity exceeded");
        unsafe {
            self.ptr.as_ptr().add(self.len).write(item);
        }
        self.len += 1;
    }

    /// Get as slice
    pub fn as_slice(&self) -> &[T] {
        if self.len == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// Get length
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterate
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.as_slice().iter()
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for ArenaVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

/// Expression node (arena-allocated)
#[derive(Debug)]
pub enum ArenaExpr {
    IntLiteral {
        value: i32,
        span: Span,
    },
    BoolLiteral {
        value: bool,
        span: Span,
    },
    Identifier {
        name: ArenaStr,
        span: Span,
    },
    Binary {
        op: BinaryOp,
        left: NonNull<ArenaExpr>,
        right: NonNull<ArenaExpr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        operand: NonNull<ArenaExpr>,
        span: Span,
    },
    Call {
        name: ArenaStr,
        args: ArenaVec<NonNull<ArenaExpr>>,
        span: Span,
    },
    ArrayIndex {
        array_name: ArenaStr,
        index: NonNull<ArenaExpr>,
        span: Span,
    },
}

impl ArenaExpr {
    /// Get the span of this expression
    pub fn span(&self) -> Span {
        match self {
            ArenaExpr::IntLiteral { span, .. } => *span,
            ArenaExpr::BoolLiteral { span, .. } => *span,
            ArenaExpr::Identifier { span, .. } => *span,
            ArenaExpr::Binary { span, .. } => *span,
            ArenaExpr::Unary { span, .. } => *span,
            ArenaExpr::Call { span, .. } => *span,
            ArenaExpr::ArrayIndex { span, .. } => *span,
        }
    }
}

/// Statement node (arena-allocated)
#[derive(Debug)]
pub enum ArenaStmt {
    VarDecl {
        name: ArenaStr,
        ty: Type,
        array_size: Option<u32>,
        init_expr: Option<NonNull<ArenaExpr>>,
        span: Span,
    },
    Assign {
        target: ArenaStr,
        index_expr: Option<NonNull<ArenaExpr>>,
        value: NonNull<ArenaExpr>,
        span: Span,
    },
    If {
        condition: NonNull<ArenaExpr>,
        then_body: ArenaVec<NonNull<ArenaStmt>>,
        else_body: Option<ArenaVec<NonNull<ArenaStmt>>>,
        span: Span,
    },
    While {
        condition: NonNull<ArenaExpr>,
        body: ArenaVec<NonNull<ArenaStmt>>,
        span: Span,
    },
    Return {
        value: NonNull<ArenaExpr>,
        span: Span,
    },
    Print {
        value: NonNull<ArenaExpr>,
        span: Span,
    },
    ExprStmt {
        expr: NonNull<ArenaExpr>,
        span: Span,
    },
}

/// Parameter (arena-allocated)
#[derive(Debug, Clone, Copy)]
pub struct ArenaParam {
    pub name: ArenaStr,
    pub ty: Type,
    pub span: Span,
}

/// Function definition (arena-allocated)
#[derive(Debug)]
pub struct ArenaFunction {
    pub name: ArenaStr,
    pub params: ArenaVec<ArenaParam>,
    pub body: ArenaVec<NonNull<ArenaStmt>>,
    pub span: Span,
}

/// Global variable declaration (arena-allocated)
#[derive(Debug)]
pub struct ArenaGlobal {
    pub name: ArenaStr,
    pub ty: Type,
    pub array_size: Option<u32>,
    pub init_expr: Option<NonNull<ArenaExpr>>,
    pub span: Span,
}

/// Complete program (arena-allocated)
#[derive(Debug)]
pub struct ArenaProgram {
    pub globals: ArenaVec<ArenaGlobal>,
    pub functions: ArenaVec<ArenaFunction>,
}

/// Arena for AST allocation
pub struct AstArena {
    bump: BumpAllocator,
}

impl AstArena {
    /// Create a new AST arena with default size (1MB)
    pub fn new() -> Self {
        Self {
            bump: BumpAllocator::new(1024 * 1024),
        }
    }

    /// Create with specified size
    pub fn with_capacity(size: usize) -> Self {
        Self {
            bump: BumpAllocator::new(size),
        }
    }

    /// Allocate an expression
    pub fn alloc_expr(&self, expr: ArenaExpr) -> NonNull<ArenaExpr> {
        let ptr = self
            .bump
            .alloc_typed::<ArenaExpr>()
            .expect("AST arena out of memory");
        unsafe {
            ptr.as_ptr().write(expr);
        }
        ptr
    }

    /// Allocate a statement
    pub fn alloc_stmt(&self, stmt: ArenaStmt) -> NonNull<ArenaStmt> {
        let ptr = self
            .bump
            .alloc_typed::<ArenaStmt>()
            .expect("AST arena out of memory");
        unsafe {
            ptr.as_ptr().write(stmt);
        }
        ptr
    }

    /// Allocate a string
    pub fn alloc_str(&self, s: &str) -> ArenaStr {
        ArenaStr::new(&self.bump, s).expect("AST arena out of memory")
    }

    /// Create a vector with capacity
    pub fn alloc_vec<T: Copy>(&self, cap: usize) -> ArenaVec<T> {
        ArenaVec::with_capacity(&self.bump, cap).expect("AST arena out of memory")
    }

    /// Get allocator stats
    pub fn stats(&self) -> crate::alloc::AllocatorStats {
        self.bump.stats()
    }

    /// Reset the arena (invalidates all pointers!)
    pub fn reset(&self) {
        self.bump.reset();
    }
}

impl Default for AstArena {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arena_str() {
        let arena = BumpAllocator::new(4096);
        let s = ArenaStr::new(&arena, "hello").unwrap();
        assert_eq!(s.as_str(), "hello");
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn test_arena_vec() {
        let arena = BumpAllocator::new(4096);
        let mut vec: ArenaVec<i32> = ArenaVec::with_capacity(&arena, 10).unwrap();
        vec.push(1);
        vec.push(2);
        vec.push(3);
        assert_eq!(vec.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn test_ast_arena() {
        let arena = AstArena::new();

        // Allocate an expression
        let expr = arena.alloc_expr(ArenaExpr::IntLiteral {
            value: 42,
            span: Span { line: 1, column: 1 },
        });

        unsafe {
            match expr.as_ref() {
                ArenaExpr::IntLiteral { value, .. } => assert_eq!(*value, 42),
                _ => panic!("wrong type"),
            }
        }
    }
}
