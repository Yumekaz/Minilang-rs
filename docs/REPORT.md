# MiniLang Rust Implementation Report

## Overview

MiniLang is a Rust compiler/runtime for a deliberately small imperative
language. The current project is best described as a compiler correctness lab:
it has a normal frontend and bytecode VM, but the strongest evidence comes from
the bytecode verifier, backend comparator, replayable traces, VM/GC trace diff,
and deterministic self-audit fuzzer.

This report describes what is implemented today. It does not claim MiniLang is
a production language or a broad compiler framework.

## Current Implementation Status

| Component | Status | Notes |
|-----------|--------|-------|
| Lexer / parser / AST | Implemented | Recursive-descent parser over the current MiniLang grammar |
| Semantic analyzer | Implemented | Type, scope, function, array, and `main` checks |
| Bytecode compiler | Implemented | Stack-based bytecode plus function/global metadata |
| Reference VM | Implemented | Explicit frames, traps, shared runtime limits |
| GC VM | Implemented | Heap arrays with mark-sweep collection path |
| Optimizer | Implemented | Constant folding, strength reduction, dead-code cleanup |
| Bytecode verifier | Implemented | Stack effects, jump targets, slots, calls, arrays, limits, backend eligibility |
| Backend comparator | Implemented | Compares VM, GC VM, optimized VM, and eligible JIT observable behavior |
| Trace audit | Implemented | JSON traces, reference replay, VM-vs-GC trace diff |
| Self-audit fuzzer | Implemented | Deterministic valid-program generation, shrinking, failure artifacts |
| JIT | Experimental | Linux x86-64 only, linear pure scalar subset with locals |

## Correctness Audit Commands

```bash
cargo run --locked --release -- examples/hello.lang --verify
cargo run --locked --release -- examples/hello.lang --compare-backends
cargo run --locked --release -- examples/hello.lang --trace-json trace.json
cargo run --locked --release -- examples/hello.lang --trace-replay --audit-json trace-replay.audit.json
cargo run --locked --release -- examples/hello.lang --trace-diff --audit-json trace-diff.audit.json
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0x5eed --fuzz-artifacts fuzz-artifacts/seed-5eed --fuzz-json fuzz-summary-5eed.json
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0xc0ffee --fuzz-artifacts fuzz-artifacts/seed-c0ffee --fuzz-json fuzz-summary-c0ffee.json
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0xbadc0de --fuzz-mode optimizer-stress --fuzz-artifacts fuzz-artifacts/optimizer --fuzz-json fuzz-optimizer.json
```

CI runs the standard Rust checks plus a seed/mode fuzz matrix on Linux. The
scheduled nightly workflow runs the same matrix with a larger case count.

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
- Current program support is intentionally narrow: linear, pure, single-function scalar bytecode with local load/store
- Globals, arrays, calls, jumps/control flow, division, and `print` fall back to the VM

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

### 7. Correctness Audit Surface

**Verifier (`src/verifier.rs`):**
- Validates stack effects, control-flow targets, slot bounds, function call
  arity, array metadata, and shared limits before execution.
- Reports backend eligibility, including why the JIT is skipped.

**Backend comparator (`src/compare.rs`):**
- Runs the reference VM, GC VM, optimized VM, and JIT when eligible.
- Compares success/trap status, return value, trap code, and output.

**Trace audit (`src/trace.rs`, `src/audit.rs`):**
- Emits JSON instruction events for VM and GC VM execution.
- Replays the reference VM trace and reports the first trace divergence.
- Compares VM and GC VM traces after normalizing semantic state.

**Self-audit fuzzer (`src/fuzz.rs`):**
- Generates valid terminating programs from deterministic seeds.
- Covers scalar locals/globals, helper calls, bounded loops, prints, and
  in-bounds global/local array reads and writes, including loop-indexed array
  writes and helper calls fed by local-array reads.
- Includes an optimizer-stress generation mode for constant folding, strength
  reduction, jump remapping, dead-code elimination, and stack-effect checks.
- Reports generator feature coverage, can write JSON run summaries, and on
  failure uses AST-aware reductions before line removal, then writes minimized
  source, bytecode, trace JSON, a manifest, and failure text under the selected
  `fuzz-artifacts/` directory.
- Can save minimized failures into `tests/corpus`, whose runner replays every
  saved `.lang` file through verification, backend comparison, trace replay,
  and VM/GC trace diff.

## Source Map

| Component | Purpose |
|-----------|---------|
| `token.rs`, `lexer.rs`, `parser.rs`, `ast.rs` | Frontend syntax pipeline |
| `sema.rs` | Semantic checks for names, types, arrays, functions, and `main` |
| `compiler.rs` | AST to stack bytecode and metadata |
| `limits.rs` | Shared runtime/verifier ceilings |
| `verifier.rs` | Bytecode safety and backend eligibility |
| `compare.rs` | Observable backend comparison |
| `trace.rs`, `audit.rs` | Instruction trace data, replay, and VM/GC trace diff |
| `fuzz.rs` | Deterministic self-audit fuzzer and shrinker |
| `vm.rs`, `gc_vm.rs` | Reference VM and GC-backed VM |
| `optimizer.rs` | Bytecode optimization passes |
| `jit.rs` | Experimental Linux x86-64 JIT subset |
| `alloc.rs`, `gc.rs`, `runtime.rs`, `arena_ast.rs` | Allocator, GC, runtime-value, and arena experiments/support |
| `repl.rs`, `main.rs` | Interactive and CLI entry points |

## Interview Talking Points

### Correctness Lab
"The strongest part of the project is the audit harness: compiled bytecode is
verified structurally, then the same program is compared across the VM, GC VM,
optimized VM, and eligible JIT. Trace replay and VM/GC trace diff make backend
divergence reproducible instead of just saying a test failed."

### Memory Management
"The project includes custom bump/free-list/slab allocators. The compiler currently uses the bump allocator for string interning, while arena-backed AST types are implemented separately but not wired into the parser. The `--gc` VM heap-allocates arrays and traces references from the stack, globals, and call frames."

### JIT Compilation  
"The JIT compiler emits x86-64 machine code with REX prefixes, ModR/M encoding,
and mmap/mprotect executable memory. It follows the System V AMD64 calling
convention, but its supported source subset is deliberately small today: linear
pure scalar code in a single `main` function, including scalar locals only
after verifier and backend-comparator proof gates."

### Optimization
"I implemented classic compiler optimizations - constant folding evaluates expressions like `10 + 20` at compile time, strength reduction replaces expensive operations like `x * 1` with identity, and dead code elimination uses control flow analysis to remove unreachable instructions."

### Profiling Story
"When I profiled the interpreter, I found the hot loop was in the instruction dispatch. The optimizer helped reduce instruction count by 66% for simple programs through constant folding and DCE."

## Limitations

- JIT only works on Linux x86-64
- JIT only handles linear, pure, single-function scalar bytecode
- JIT supports scalar locals but rejects globals, arrays, calls, jumps/control flow, division, and `print` instead of partially compiling unsafe behavior
- Arena-backed AST exists but is not the active parser representation
- `runtime.rs` GC value abstractions are not the default VM value model
- No register allocation in JIT (stack-based)
- GC is stop-the-world (no concurrent collection)
- Fixed cycle limit (100,000)
- No incremental JIT compilation (full recompile)

## Building and Testing

```bash
# Build
cargo build --locked --release

# Run tests
cargo test --locked --all-targets --all-features

# Run with optimizations through Cargo
cargo run --locked --release -- program.lang --opt

# Run the correctness audit surface on one example
cargo run --locked --release -- examples/hello.lang --verify
cargo run --locked --release -- examples/hello.lang --compare-backends
cargo run --locked --release -- examples/hello.lang --trace-replay --audit-json trace-replay.audit.json
cargo run --locked --release -- examples/hello.lang --trace-diff --audit-json trace-diff.audit.json

# Start REPL
cargo run --locked --release -- --repl

# Evaluate expression
cargo run --locked --release -- --eval "2 + 3 * 4"
```
