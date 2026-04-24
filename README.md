# MiniLang - A Systems Programming Language Compiler in Rust

A minimal language compiler demonstrating core systems programming concepts:
- **Bytecode VM** interpreter for the full language
- **Optional GC VM** with heap-allocated arrays
- **Experimental x86-64 JIT compiler** for linear, pure, single-function expression bytecode
- **Custom memory allocators** (bump, free-list, slab), used selectively and benchmarked

## Building

```bash
cargo build --release
```

On Windows with the MSVC Rust toolchain, run from a Visual Studio Developer
Command Prompt or ensure the Visual C++ Build Tools and Windows SDK are
installed. If Git's `usr/bin/link.exe` appears before MSVC's linker on `PATH`,
Rust may fail during linking.

## Usage

```bash
# Run with interpreter
./target/release/minilang examples/fibonacci.lang

# Run with JIT compiler (Linux x86-64 only, limited bytecode subset)
./target/release/minilang examples/fibonacci.lang --jit

# Show bytecode IR
./target/release/minilang examples/fibonacci.lang --ir

# Verify bytecode safety and backend eligibility
./target/release/minilang examples/fibonacci.lang --verify

# Compare observable behavior across VM backends
./target/release/minilang examples/fibonacci.lang --compare-backends

# Write a replay-oriented JSON trace from the selected VM backend
./target/release/minilang examples/hello.lang --trace-json trace.json

# Check that the reference VM trace replays deterministically
./target/release/minilang examples/hello.lang --trace-replay

# Find the first instruction-level divergence between VM and GC VM
./target/release/minilang examples/hello.lang --trace-diff

# Generate 1,000 deterministic programs and audit every runtime path
./target/release/minilang --fuzz 1000 --fuzz-seed 0x5eed

# Benchmark mode
./target/release/minilang examples/fibonacci.lang --bench

# Show allocator/GC stats
./target/release/minilang examples/fibonacci.lang --stats
```

## Project Structure

```
src/
├── lib.rs          # Library exports
├── main.rs         # CLI entry point
├── token.rs        # Token definitions
├── lexer.rs        # Lexical analyzer
├── ast.rs          # AST node definitions
├── parser.rs       # Recursive descent parser
├── sema.rs         # Semantic analyzer (type checking)
├── compiler.rs     # Bytecode compiler
├── optimizer.rs    # Bytecode optimization passes
├── audit.rs        # Trace replay and backend trace diff reports
├── fuzz.rs         # Deterministic self-audit fuzzer and shrinker
├── vm.rs           # Stack-based VM interpreter
├── gc_vm.rs        # GC-integrated VM
├── jit.rs          # x86-64 JIT compiler
├── alloc.rs        # Custom memory allocators
├── gc.rs           # Mark-sweep garbage collector primitives
├── runtime.rs      # GC-managed runtime value helpers
├── arena_ast.rs    # Experimental arena-backed AST types
└── repl.rs         # Interactive REPL
```

## Systems Engineering Features

### Memory Allocators (`src/alloc.rs`)

Three allocator implementations:

1. **Bump Allocator**: O(1) allocation, bulk deallocation
   - Perfect for compiler phases with known lifetimes
   - Zero fragmentation, cache-friendly

2. **Free-List Allocator**: General purpose with coalescing
   - First-fit allocation strategy
   - Adjacent block coalescing on free

3. **Slab Allocator**: Fixed-size object pools
   - Extremely fast for uniform allocations
   - No external fragmentation

### Garbage Collector (`src/gc.rs`, `src/gc_vm.rs`)

Mark-sweep GC primitives plus a GC-integrated VM path:
- Object headers with type tags and mark bits
- Root set management
- Automatic collection at threshold
- Heap arrays in `--gc` mode

### JIT Compiler (`src/jit.rs`)

x86-64 native code generation:
- Direct machine code emission (no LLVM)
- System V AMD64 ABI compliance
- Executable memory via mmap/mprotect
- Current scope: linear, pure, single-function expression bytecode
- Unsupported bytecode, including locals, globals, arrays, calls, jumps, division, and `print`, falls back to the VM

### Bytecode VM (`src/vm.rs`)

Stack-based interpreter:
- 30+ bytecode instructions
- Call stack with frames
- Runtime error trapping

### Bytecode Verifier (`src/verifier.rs`)

Structural verifier for compiled bytecode:
- Checks stack effects, jump targets, local/global slot bounds, function call arity, and array metadata
- Reports maximum stack depth, estimated frame depth, possible runtime traps, and backend eligibility
- Powers the `--verify` CLI mode as the foundation for backend equivalence and trace tooling

### Backend Comparator (`src/compare.rs`)

Self-auditing runtime comparison:
- Runs the same bytecode through the standard VM, GC VM, optimized VM, and JIT when eligible
- Compares observable behavior: success/trap status, return value, trap code, and output
- Powers the `--compare-backends` CLI mode for equivalence checks

### Execution Trace (`src/trace.rs`)

Replay-oriented JSON trace support:
- Records VM and GC VM instruction events with PC, opcode, args, stack before/after, frame depth, next PC, and outcome
- Powers `--trace-json <file>` as the portable artifact format

### Runtime Audit (`src/audit.rs`)

Trace-level determinism and divergence reports:
- `--trace-replay` reruns the reference VM and checks that the generated trace is replayable
- `--trace-diff` compares standard VM and GC VM traces and reports the first differing instruction field

### Self-Audit Fuzzer (`src/fuzz.rs`)

Deterministic generated-program testing:
- `--fuzz <cases>` generates valid, terminating MiniLang programs from a seed
- Generated programs cover initialized scalars, bounded loops, helper functions, prints, and in-bounds global/local array reads/writes
- Every case runs compile, verification, backend comparison, trace replay, and VM/GC trace diff
- On first failure, the fuzzer shrinks the repro and writes source, bytecode, traces, and failure metadata under `fuzz-artifacts/`
- CI runs a fixed-seed fuzz audit on Linux and uploads `fuzz-artifacts/` when the fuzz step fails

## Benchmarking

```bash
# Run benchmarks
cargo bench

# Profile with perf (Linux)
cargo build --release
perf record -g ./target/release/minilang examples/fibonacci.lang --bench
perf report
```

## Language Reference

The current source-of-truth language contract lives in
[`docs/spec.md`](docs/spec.md). The summary below covers the main surface.

### Types
- `int`: 32-bit signed integer
- `bool`: boolean (true/false)

### Operators
- Arithmetic: `+`, `-`, `*`, `/`
- Comparison: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Logical: `&&`, `||`, `!`

### Statements
```
int x = 10;              // Variable declaration
x = x + 1;               // Assignment
if (x > 5) { } else { }  // Conditional
while (x < 100) { }      // Loop
return x;                // Return
print x;                 // Output
```

### Functions
```
func add(int a, int b) {
    return a + b;
}

func main() {
    return add(1, 2);
}
```

### Arrays
```
int arr[10];             // Array declaration
arr[0] = 42;             // Array assignment
return arr[0];           // Array access
```

## Performance Characteristics

| Component | Complexity | Notes |
|-----------|------------|-------|
| Lexer | O(n) | Single pass, no backtracking |
| Parser | O(n) | Recursive descent, predictive |
| Semantic | O(n) | Single pass with symbol table |
| Compiler | O(n) | Direct translation |
| VM | O(cycles) | Stack-based, ~100K cycle limit |
| JIT | O(n) | Linear code generation |

## License

MIT
