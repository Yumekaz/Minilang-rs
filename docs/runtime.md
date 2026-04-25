# MiniLang Runtime Specification

## Virtual Machine Architecture

### Stack-Based Execution
- Operand stack for expression evaluation
- Call stack for function frames (explicit, no host recursion)
- Global variable array
- Local variable slots per frame

### Limits (matching reference implementation)
| Resource | Limit |
|----------|-------|
| Global variables | 256 slots |
| Local variables | 1024 slots |
| Call frames | 100 max |
| Operand stack | 1000 entries |
| Execution cycles | 100,000 max |
| Instructions | 10,000 max |
| GC collection threshold | 8 array allocations |

### Trap Codes
| Code | Name | Description |
|------|------|-------------|
| 1 | TRAP_DIV_ZERO | Division by zero |
| 2 | TRAP_UNDEFINED_LOCAL | Reading uninitialized local variable |
| 3 | TRAP_ARRAY_OOB | Array index out of bounds |
| 4 | TRAP_STACK_OVERFLOW | Call stack exceeded 100 frames |
| 5 | TRAP_CYCLE_LIMIT | Exceeded 100,000 cycles |
| 6 | TRAP_UNDEFINED_FUNCTION | Call to undefined function |
| 7 | TRAP_STACK_UNDERFLOW | Bytecode popped from an empty operand stack |
| 8 | TRAP_INVALID_INSTRUCTION | Bytecode could not be executed by this backend |

### Trap Message Format
```
TRAP: <TrapCode> (code <n>) at PC <pc> after <cycles> cycles
Stack depth: <depth>
Frame depth: <depth>
```

## 32-Bit Arithmetic

All integer values are 32-bit signed two's complement.

### Overflow Behavior
- Overflow wraps around (no trap)
- Uses standard two's complement semantics
- `2147483647 + 1` = `-2147483648`

### Normalization
```rust
fn normalize_i32(value: i64) -> i64 {
    let masked = value & 0xFFFFFFFF;
    if masked > 0x7FFFFFFF {
        masked - 0x100000000
    } else {
        masked
    }
}
```

## Memory Layout

### Global Variables
- 256 slots, indexed 0-255
- All initialized to 0 at program start
- Arrays occupy contiguous slots

### Local Variables
- Allocated per function frame
- Parameters occupy first N slots
- Uninitialized locals tracked (read = trap)
- Frame reset on function return

### Arrays
- Fixed size (compile-time constant)
- Bounds checked at runtime
- Zero-indexed
- Contiguous storage

## Bytecode Instructions

### Stack Operations
| Opcode | Args | Description |
|--------|------|-------------|
| LOAD_CONST | value | Push constant |
| LOAD_LOCAL | slot | Push local variable |
| STORE_LOCAL | slot | Pop and store to local |
| LOAD_GLOBAL | slot | Push global variable |
| STORE_GLOBAL | slot | Pop and store to global |
| POP | - | Discard top of stack |
| DUP | - | Duplicate top of stack |

### Arithmetic
| Opcode | Description |
|--------|-------------|
| ADD | a + b |
| SUB | a - b |
| MUL | a * b |
| DIV | a / b (traps on zero) |
| NEG | -a |

### Comparison
| Opcode | Description |
|--------|-------------|
| EQ | a == b → 0 or 1 |
| NE | a != b → 0 or 1 |
| LT | a < b → 0 or 1 |
| GT | a > b → 0 or 1 |
| LE | a <= b → 0 or 1 |
| GE | a >= b → 0 or 1 |

### Logical
| Opcode | Description |
|--------|-------------|
| AND | a && b for already-evaluated operands |
| OR | a \|\| b for already-evaluated operands |
| NOT | !a → 0 or 1 |

Source-level `&&` and `||` short-circuit through compiler-emitted conditional jumps.

### Control Flow
| Opcode | Args | Description |
|--------|------|-------------|
| JUMP | target | Unconditional jump |
| JUMP_IF_FALSE | target | Jump if top == 0 |
| JUMP_IF_TRUE | target | Jump if top != 0 |
| CALL | func_id, argc | Call function |
| RETURN | - | Return from function |

### Arrays
| Opcode | Args | Description |
|--------|------|-------------|
| ARRAY_LOAD | base, size | Load arr[index] |
| ARRAY_STORE | base, size | Store arr[index] = value |

### I/O
| Opcode | Description |
|--------|-------------|
| PRINT | Output top of stack |
| HALT | Stop execution |

## Calling Convention

1. Caller pushes arguments left-to-right
2. CALL creates new frame
3. Arguments copied to callee's local slots 0..N-1
4. Remaining locals marked uninitialized
5. RETURN pops frame, pushes return value
6. Caller receives return value on stack

## Debug Trace Format

When `--debug` is enabled:
```
[<PC>] <OPCODE> <arg1> <arg2>
```

Example:
```
[   0] LoadConst 100 0
[   1] StoreGlobal 0 0
[   2] LoadGlobal 0 0
[   3] Return 0 0
```

## Verification And Audit Commands

Runtime behavior is checked by several CLI modes that all operate after parse,
semantic analysis, and bytecode compilation:

```bash
cargo run --locked --release -- examples/hello.lang --verify
cargo run --locked --release -- examples/hello.lang --compare-backends
cargo run --locked --release -- examples/hello.lang --trace-replay --audit-json trace-replay.audit.json
cargo run --locked --release -- examples/hello.lang --trace-diff --audit-json trace-diff.audit.json
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0x5eed --fuzz-artifacts fuzz-artifacts/seed-5eed --fuzz-json fuzz-summary-5eed.json
```

`--verify` checks structural bytecode safety and backend eligibility. It can
report possible runtime traps such as divide-by-zero or unbounded recursion,
but it does not execute the program.

`--compare-backends` executes the reference VM, GC VM, optimized VM, and the JIT
only when the verifier marks the JIT eligible. It compares observable behavior:
success/trap status, return value, trap code, and printed output.

`--trace-replay` runs the reference VM twice with tracing enabled and checks
that the trace and final observable result replay deterministically.

`--trace-diff` compares reference VM and GC VM traces at semantic instruction
level. Allocation-specific representation differences are normalized before
the first divergence is reported.

`--audit-json <file>` can be combined with `--trace-replay` or `--trace-diff`
to write stable evidence JSON with trace summaries and fingerprints.

`--fuzz` generates valid, terminating programs from a deterministic seed. Each
case runs compile, verification, backend comparison, trace replay, and VM/GC
trace diff. On failure it writes a minimized repro and trace artifacts when
`--fuzz-artifacts` is provided. `--fuzz-json <file>` writes a machine-readable
run summary with generator feature coverage. `--fuzz-mode optimizer-stress`
switches to optimizer-shaped programs, and `--fuzz-corpus-out <dir>` saves
minimized failures into a regression corpus for future test runs.
