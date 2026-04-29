# Qydrel

Qydrel is a Rust compiler/VM correctness engine built around a deliberately
small source language. The core story is not language breadth; it is independent
source-level oracle execution, bytecode verification, backend equivalence,
replayable traces, VM/GC trace diffs, metamorphic fuzzing, shrinking, and
evidence reports.

Current surface:
- **Frontend pipeline**: lexer, parser, semantic analyzer, bytecode compiler
- **AST oracle**: independent source interpreter compared against executable backends
- **Bytecode verifier**: stack effects, control-flow targets, slots, calls, limits, and backend eligibility
- **Backend comparison**: reference VM, GC VM, optimized VM, and JIT when eligible
- **Trace audit tooling**: JSON traces, replay checks, and VM-vs-GC instruction diffs
- **Self-audit fuzzer**: deterministic valid-program generation, coverage-guided candidate selection, metamorphic variants, exact failure fingerprints, shrinking, and artifacts
- **Evidence report**: one command writes corpus, fuzz, backend, trace, coverage-dashboard, and bug-museum summaries
- **Bug museum**: checked-in minimized regressions with metadata and an audit runner
- **Runtime systems pieces**: custom allocators, mark-sweep GC primitives, and a narrow x86-64 JIT

This is intentionally not a production language. The language stays small so
compiler/runtime invariants can be checked directly.

## Building

```bash
cargo build --locked --release
```

On Windows with the MSVC Rust toolchain, run from a Visual Studio Developer
Command Prompt or ensure the Visual C++ Build Tools and Windows SDK are
installed. If Git's `usr/bin/link.exe` appears before MSVC's linker on `PATH`,
Rust may fail during linking.

## Core Commands

```bash
# Run with the reference VM
cargo run --locked --release -- examples/fibonacci.lang

# Run with the GC-backed VM
cargo run --locked --release -- examples/arrays.lang --gc

# Show bytecode IR
cargo run --locked --release -- examples/fibonacci.lang --ir

# Verify bytecode safety and backend eligibility
cargo run --locked --release -- examples/fibonacci.lang --verify

# Compare the independent AST oracle with executable backends
cargo run --locked --release -- examples/fibonacci.lang --oracle

# Compare observable behavior across VM backends
cargo run --locked --release -- examples/fibonacci.lang --compare-backends

# Write a replay-oriented JSON trace from the selected VM backend
cargo run --locked --release -- examples/hello.lang --trace-json trace.json

# Check that the reference VM trace replays deterministically
cargo run --locked --release -- examples/hello.lang --trace-replay --audit-json trace-replay.audit.json

# Find the first instruction-level divergence between VM and GC VM
cargo run --locked --release -- examples/hello.lang --trace-diff --audit-json trace-diff.audit.json

# Generate deterministic programs and audit every runtime path
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0x5eed --fuzz-artifacts fuzz-artifacts/seed-5eed --fuzz-json fuzz-summary-5eed.json
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0xc0ffee --fuzz-artifacts fuzz-artifacts/seed-c0ffee --fuzz-json fuzz-summary-c0ffee.json

# Stress the optimizer-specific generator family
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0xbadc0de --fuzz-mode optimizer-stress --fuzz-artifacts fuzz-artifacts/optimizer --fuzz-json fuzz-optimizer.json

# Write a reviewer-facing evidence packet
cargo run --locked --release -- --evidence-report evidence/latest

# Audit checked-in historical/minimized bug repros
cargo test --locked --test bug_museum_tests

# Run with JIT compiler when the bytecode is eligible (Linux x86-64 only)
cargo run --locked --release -- examples/fibonacci.lang --jit

# Benchmark mode
cargo run --locked --release -- examples/fibonacci.lang --bench

# Show allocator/GC stats
cargo run --locked --release -- examples/fibonacci.lang --stats
```

The JIT is deliberately gated. It accepts only a small linear scalar subset
on Linux x86-64, now including local load/store bytecode after verifier and
backend-comparator proof gates. Unsupported bytecode is skipped by the
comparator and falls back to the VM in normal `--jit` execution.

See [`docs/correctness-lab.md`](docs/correctness-lab.md) for the audit workflow
and what each command proves. See [`docs/bug-museum.md`](docs/bug-museum.md)
for the checked-in historical bug cases.

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
├── limits.rs       # Shared runtime/verifier limits
├── verifier.rs     # Structural bytecode verifier and backend eligibility
├── oracle.rs       # Independent AST interpreter oracle
├── compare.rs      # Observable backend comparison
├── trace.rs        # Replay-oriented instruction trace model
├── audit.rs        # Trace replay and backend trace diff reports
├── fuzz.rs         # Deterministic self-audit fuzzer and shrinker
├── evidence.rs     # One-command JSON/Markdown evidence report
├── vm.rs           # Stack-based VM interpreter
├── gc_vm.rs        # GC-integrated VM
├── jit.rs          # x86-64 JIT compiler
├── alloc.rs        # Custom memory allocators
├── gc.rs           # Mark-sweep garbage collector primitives
├── runtime.rs      # GC-managed runtime value helpers
├── arena_ast.rs    # Experimental arena-backed AST types
└── repl.rs         # Interactive REPL
```

## Correctness Lab Surface

### AST Oracle (`src/oracle.rs`)

Independent source-level oracle:
- Executes the parsed AST directly, without bytecode VM execution
- Mirrors source-reachable traps such as divide-by-zero, undefined locals, array bounds, frame limits, and cycle limits
- Powers `--oracle` and is now part of fuzz/corpus/evidence auditing

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
- Generated programs cover initialized scalars, bounded loops, helper functions, prints, in-bounds global/local array reads/writes, loop-indexed array writes, and helper calls fed by local-array reads
- Every case runs compile, verification, AST-oracle comparison, backend comparison, trace replay, VM/GC trace diff, and metamorphic-equivalence checks
- Coverage-guided candidate selection prefers generated programs that add new feature/opcode coverage
- Metamorphic checks now include neutral arithmetic, dead branches, unused local work, branch inversion, identity helper wrapping, and conservative independent-statement reordering
- `--fuzz-mode optimizer-stress` generates programs shaped around constant folding, strength reduction, jump/control-flow remapping, dead-code elimination, and stack-effect preservation
- Reports generator feature coverage and can write a machine-readable run summary with `--fuzz-json <file>`
- On first failure, the AST-aware shrinker tries function, statement, branch, expression, and array-operation reductions before falling back to line removal while preserving the same failure fingerprint
- Failure artifacts include source, bytecode, traces, a manifest, and failure metadata under `fuzz-artifacts/`; `--fuzz-corpus-out <dir>` can also save the minimized repro directly into a regression corpus
- CI runs a seed/mode fuzz matrix on pushes and a larger scheduled nightly fuzz audit

### Evidence Report (`src/evidence.rs`)

Reviewer-facing proof packet:
- `--evidence-report <dir>` writes `report.json` and `report.md`
- Audits every `tests/corpus/*.lang` file through verifier, AST oracle, backend matrix, trace replay, and VM/GC trace diff
- Runs the seed/mode fuzz matrix and renders an aggregate feature/opcode coverage dashboard
- Scans `tests/bugs/` so checked-in historical/minimized bugs are visible in reviewer evidence
- Scans existing local minimized fuzz artifacts separately so machine-local failure packages remain visible without pretending they are durable museum entries

### Bug Museum (`tests/bugs/`)

Checked-in historical/minimized correctness repros:
- Each entry has `metadata.txt`, `repro.lang`, and `README.md`
- `tests/bug_museum_tests.rs` audits documented expected behavior
- The current entry is a fixed JIT proof-gate case: reading an uninitialized local must trap in VM backends and must be skipped by the JIT

## Runtime And Systems Components

### Bytecode VM (`src/vm.rs`)

Stack-based reference interpreter:
- Executes the full current bytecode language
- Uses explicit call frames, runtime traps, and shared hard limits
- Acts as the comparator reference for observable behavior

### Garbage Collector (`src/gc.rs`, `src/gc_vm.rs`)

Mark-sweep GC primitives plus a GC-integrated VM path:
- Object headers with type tags and mark bits
- Root set management from stack, globals, and call frames
- Automatic collection at threshold
- Heap arrays in `--gc` mode

### JIT Compiler (`src/jit.rs`)

x86-64 native code generation:
- Direct machine code emission with mmap/mprotect on Linux x86-64
- Current scope: linear, pure, single-function scalar bytecode with constants, arithmetic/comparison/logical stack ops, and scalar locals
- Unsupported bytecode, including globals, arrays, calls, jumps, division, and `print`, falls back to the VM

### Memory Allocators (`src/alloc.rs`)

Three allocator implementations:

1. **Bump Allocator**: used for compiler string interning/statistics
2. **Free-List Allocator**: implemented and benchmarked as a general-purpose allocator
3. **Slab Allocator**: implemented and benchmarked for fixed-size allocations

## Benchmarking

```bash
# Run benchmarks
cargo bench --locked

# Profile with perf (Linux)
cargo build --locked --release
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
