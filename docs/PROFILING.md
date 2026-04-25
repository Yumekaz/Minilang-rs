# MiniLang Performance Analysis

## Profiling Summary

### Benchmark: Hot Loop (5000 iterations)
```lang
func main() {
    int i = 0;
    int sum = 0;
    while (i < 5000) {
        sum = sum + i;
        i = i + 1;
    }
    return sum;
}
```

### Historical Results

| Execution Mode | Time (ms) | Speedup |
|----------------|-----------|---------|
| Interpreter    | 0.75-0.80 | 1.0x    |
| Interpreter + Opt | 0.74-0.78 | ~1.03x |
| JIT            | N/A       | N/A     |

The current JIT intentionally rejects loops, globals, calls, arrays, division,
and other bytecode that it cannot execute with the VM's safety semantics.
Scalar local-variable load/store is supported, but this loop benchmark still
requires control-flow lowering. The older 4x JIT number is not a valid claim for
this benchmark until control-flow lowering is implemented and re-measured.

### Hot Path Analysis

The interpreter's main loop executes ~65,000 cycles for this benchmark.

**Hot path breakdown:**
1. Cycle limit check
2. PC bounds check  
3. Instruction fetch
4. Opcode dispatch (match statement)
5. Stack push/pop

**Key findings:**
- The `match` statement on Opcode is the primary bottleneck
- Stack operations are efficient (Vec push/pop are O(1))
- A future safe JIT can eliminate dispatch overhead, but the current JIT does
  not run this loop benchmark

### Optimization Attempts

1. **`#[inline(always)]` on pop()** - Marginal improvement
2. **`unsafe` get_unchecked()** - No measurable improvement (branch predictor handles bounds checks well)
3. **Constant folding (--opt)** - 9.5% instruction reduction, ~3% runtime improvement

### Why a Fuller JIT Could Be Faster

The JIT compiler can eliminate:
- Opcode dispatch overhead (no match statement)
- Indirect function calls
- Most bounds checks
- Stack simulation (uses native x86 stack)

### GC Impact

GC collection adds negligible overhead for typical programs:
- Collection time: < 0.1ms per collection
- Collection frequency: Once per ~8 array allocations

### Recommendations

For compute-intensive code:
1. Use `--jit` only for the current linear expression subset on Linux x86-64
2. Use `--opt` for 5-10% improvement from constant folding
3. Minimize function calls in hot loops (inline manually)

For memory-intensive code:
1. Use `--gc` for proper array lifecycle management
2. Large arrays are heap-allocated automatically
3. GC threshold is 8 arrays (configurable in source)
