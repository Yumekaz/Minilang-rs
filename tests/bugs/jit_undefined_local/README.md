# JIT Undefined Local Proof Gate

Status: fixed.

This minimized source is valid MiniLang, but it reads `x` before `x` is
initialized:

```text
func main() {
  int x;
  return x;
}
```

The reference VM and GC VM must trap with `UndefinedLocal`. The JIT must not
execute this program as native code, because native execution has no safe
undefined-local trap path for this case.

The fix is the verifier proof gate: it keeps the bytecode structurally valid,
records `UndefinedLocal` as a possible runtime trap, and rejects JIT eligibility
for bytecode with possible traps on Linux x86-64. On non-JIT targets the JIT is
also skipped by the target gate, so the museum test checks the common behavior
plus the Linux-specific trap-free reason when that backend is available.

