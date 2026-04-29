# Qydrel Bug Museum

The bug museum is a checked-in set of small historical or minimized correctness
cases under `tests/bugs/`. Each entry keeps the repro source, a short README,
and `metadata.txt` fields that the test runner can audit.

Required entry shape:

```text
tests/bugs/<case-id>/
  metadata.txt
  repro.lang
  README.md
```

Required metadata fields:

```text
id: stable-case-id
status: fixed
expected: vm_trap_undefined_local_jit_skipped
proof_gate: verifier_possible_trap_blocks_jit
```

`status` should be honest. Use `fixed` only when a test or proof gate now
prevents the bug class from silently regressing. Use the README to explain what
the old risk was, what behavior is expected now, and what is still not claimed.

## Current Entries

| Entry | Status | Why it matters |
| --- | --- | --- |
| `tests/bugs/jit_undefined_local/` | fixed | A valid source program can read an uninitialized local. VM backends must trap with `UndefinedLocal`, and the JIT must skip it instead of compiling native code without that trap path. |

## Audit Command

```bash
cargo test --locked --test bug_museum_tests
```

The test runner checks that every museum entry is documented and that each
known `expected` behavior has an executable audit. Today it verifies the
undefined-local JIT proof gate by compiling the repro, checking verifier
possible traps, checking JIT ineligibility, and confirming VM/GC/optimized VM
agree on the `UndefinedLocal` trap.

