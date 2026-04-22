# MiniLang Rust Implementation Report

## Overview

This is a complete rewrite of the MiniLang compiler from Python to Rust, with additional systems programming features demonstrating low-level expertise.

## Feature Parity with Python Version

### ✅ Fully Implemented

| Component | Status | Notes |
|-----------|--------|-------|
| Lexer | ✅ | Single-pass lexer, handles all tokens |
| Parser | ✅ | Recursive descent, no generators |
| AST | ✅ | Rust enums instead of dataclasses |
| Semantic Analyzer | ✅ | Scope, types, main check |
| IR/Bytecode | ✅ | Same opcodes |
| Stack-based VM | ✅ | Explicit frame stack |
| CLI flags | ✅ | --tokens, --ast, --ir, --debug, --bench, --stats, --opt, --repl, --eval |

## Systems Programming Features

### 1. Memory Allocators and Arena Support (`src/alloc.rs`, `src/arena_ast.rs`)

**Current status:**
- The active parser builds the `ast.rs` boxed AST.
- `arena_ast.rs` implements arena-backed AST nodes, arena strings, and arena vectors, but it is not the active parser representation.
- The compiler uses `BumpAllocator` for identifier string interning/statistics.
- `FreeListAllocator` and `SlabAllocator` are implemented and benchmarked, but they are not on the main compile/execute path.

**Benefits:**
- O(1) allocation (bump pointer)
- Zero fragmentation
- Cache-friendly (contiguous memory)
- Bulk deallocation (reset entire arena)

```rust
pub struct AstArena {
    bump: BumpAllocator,
}

// Usage:
let arena = AstArena::new();
let expr = arena.alloc_expr(ArenaExpr::IntLiteral { value: 42, span });
```

### 2. GC-Managed Runtime Values (`src/runtime.rs`, `src/gc_vm.rs`)

**Current status:**
- `GcVm` is the active `--gc` execution path and heap-allocates arrays as `HeapArray` values.
- `runtime.rs` contains a richer `Value`/`GcArray` abstraction for GC-managed runtime values, but it is mostly support/test code at the moment.
- The default `Vm` still stores local arrays in VM-owned slots rather than tracing them through `runtime.rs`.

```rust
pub enum Value {
    Int(i64),           // Unboxed, no GC
    Bool(bool),         // Unboxed, no GC
    Array(GcArray),     // GC-managed, tracked
    Null,
}

pub struct GcArray {
    ptr: NonNull<i64>,  // GC-allocated memory
    len: usize,
}
```

### 3. Optimization Passes (`src/optimizer.rs`)

**Constant Folding:**
- Evaluates `10 + 20` → `30` at compile time
- Handles +, -, *, /, comparisons
- Folds unary operations (negation, not)

**Strength Reduction:**
- `x * 0` → `0`
- `x * 1` → `x`
- `x + 0` → `x`
- `x / 1` → `x`

**Dead Code Elimination:**
- Control flow analysis
- Removes unreachable instructions
- Remaps jump targets

**Results:**
```
Optimization Stats:
  Constants folded:    1
  Dead code removed:   2
  Strength reductions: 0
  Peephole opts:       0
  Instructions: 6 -> 2 (66.7% reduction)
```

### 4. Interactive REPL (`src/repl.rs`)

**Features:**
- Stateful expression evaluation over accumulated definitions
- Expression evaluation
- Function definition persistence
- Statistics tracking

**Usage:**
```bash
$ ./minilang --repl
>>> 2 + 3
= 5
>>> func double(int x) { return x * 2; }
Defined function: double
>>> double(21)
= 42
>>> :stats
Expressions evaluated: 2
>>> :quit
```

### 5. x86-64 JIT Compiler (`src/jit.rs`)

**Machine code generation:**
- Direct x86-64 instruction encoding
- REX prefixes for 64-bit operations
- ModR/M byte encoding
- System V AMD64 ABI compliance

**Memory management:**
- `mmap` for executable memory allocation
- `mprotect` for RWX → RX transition
- Proper cleanup with `munmap`

### 6. Profiling Support

**Built-in timing:**
```bash
$ ./minilang program.lang --bench --opt

=== Timing (Interpreter) ===
  Lexer:           0.014ms
  Parser:          0.015ms
  Semantic:        0.026ms
  Compile:         0.006ms
  Optimize:        0.017ms
  Execute:         0.256ms (15010 cycles)
  Total:           0.460ms
```

**External profiling:**
```bash
# With perf (Linux)
cargo build --release
perf record -g ./target/release/minilang examples/bench.lang
perf report

# With Instruments (macOS)
instruments -t "Time Profiler" ./target/release/minilang examples/bench.lang
```

## Code Statistics

| Component | Lines | Purpose |
|-----------|-------|---------|
| token.rs | 134 | Token definitions |
| lexer.rs | 249 | Lexical analysis |
| ast.rs | 183 | AST node definitions |
| arena_ast.rs | 350 | Arena-allocated AST |
| parser.rs | 627 | Recursive descent parser |
| sema.rs | 486 | Semantic analysis |
| compiler.rs | 586 | Bytecode generation |
| optimizer.rs | 350 | Optimization passes |
| vm.rs | 600 | Bytecode interpreter |
| runtime.rs | 400 | GC-managed values |
| jit.rs | 800 | x86-64 JIT compiler |
| alloc.rs | 573 | Custom allocators |
| gc.rs | 439 | Mark-sweep GC |
| repl.rs | 430 | Interactive REPL |
| **Total** | **~6,200** | |

## Interview Talking Points

### Memory Management
"The project includes custom bump/free-list/slab allocators. The compiler currently uses the bump allocator for string interning, while arena-backed AST types are implemented separately but not wired into the parser. The `--gc` VM heap-allocates arrays and traces references from the stack, globals, and call frames."

### JIT Compilation  
"The JIT compiler emits real x86-64 machine code - I handle REX prefixes for 64-bit operations, ModR/M byte encoding for register operands, and proper memory protection via mmap/mprotect. It follows the System V AMD64 calling convention."

### Optimization
"I implemented classic compiler optimizations - constant folding evaluates expressions like `10 + 20` at compile time, strength reduction replaces expensive operations like `x * 1` with identity, and dead code elimination uses control flow analysis to remove unreachable instructions."

### Profiling Story
"When I profiled the interpreter, I found the hot loop was in the instruction dispatch. The optimizer helped reduce instruction count by 66% for simple programs through constant folding and DCE."

## Limitations

- JIT only works on Linux x86-64
- JIT only handles simple single-function programs; function calls fall back to the interpreter
- JIT array operations do not currently preserve the VM's bounds-check safety
- Arena-backed AST exists but is not the active parser representation
- `runtime.rs` GC value abstractions are not the default VM value model
- No register allocation in JIT (stack-based)
- GC is stop-the-world (no concurrent collection)
- Fixed cycle limit (100,000)
- No incremental JIT compilation (full recompile)

## Building and Testing

```bash
# Build
cargo build --release

# Run tests
cargo test

# Run with optimizations
./target/release/minilang program.lang --opt

# Start REPL
./target/release/minilang --repl

# Evaluate expression
./target/release/minilang --eval "2 + 3 * 4"
```
