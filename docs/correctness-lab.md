# Qydrel Correctness Engine

Qydrel is useful as a compiler/runtime correctness engine because the same
source program can be checked from several angles: an independent AST oracle,
structural bytecode verification, observable backend comparison, replayable
traces, trace-level backend diffs, deterministic fuzzing, metamorphic variants,
shrinking, and reviewer-facing evidence reports.

The reference path is:

```text
source -> lexer -> parser -> semantic analyzer -> AST oracle
                                     |
                                     v
                                  compiler -> verifier -> VM
```

The audit paths reuse the same compiled bytecode:

```text
compiled bytecode
  -> AST oracle vs backend comparison
  -> verifier
  -> VM / GC VM / optimized VM / eligible JIT comparison
  -> VM trace replay
  -> VM vs GC VM trace diff
  -> deterministic/metamorphic fuzzer cases
  -> evidence report
```

## What Each Check Means

| Check | Command | What it proves |
| --- | --- | --- |
| AST oracle | `--oracle` | A source-level interpreter that does not use bytecode agrees with executable backends on success/trap status, return value, trap code, and output. |
| Bytecode verifier | `--verify` | The compiled program has structurally valid control flow, stack effects, slot references, function calls, array metadata, limits, and backend eligibility. |
| Backend comparison | `--compare-backends` | Executed backends agree on success/trap status, return value, trap code, and printed output. The JIT is skipped when the verifier marks it ineligible. |
| Trace JSON | `--trace-json <file>` | The selected VM backend can emit replay-oriented instruction events with PC, opcode, stack state, frame depth, next PC, and outcome. |
| Trace replay | `--trace-replay` | The reference VM produces the same trace and observable result across two runs of the same bytecode. |
| Trace diff | `--trace-diff` | The reference VM and GC VM agree at semantic instruction-trace level and observable result level. |
| Audit JSON | `--audit-json <file>` | With trace replay or trace diff, writes stable machine-readable evidence including trace summaries and fingerprints. |
| Fuzz audit | `--fuzz <cases>` | Generated valid programs pass compile, verification, AST oracle, backend comparison, trace replay, VM/GC trace diff, and metamorphic-equivalence checks. |
| Fuzz JSON | `--fuzz-json <file>` | Writes a machine-readable fuzz summary with seed, pass/fail status, and generator feature coverage. |
| Evidence report | `--evidence-report <dir>` | Writes a corpus/fuzz/backend/trace/historical-artifact report as JSON and Markdown. |

These checks do not prove the language is complete or production-ready. They
make the current compiler/runtime contracts executable and reproducible.

## Local Commands

Build once:

```bash
cargo build --locked --release
```

Run the focused audit commands:

```bash
cargo run --locked --release -- examples/hello.lang --verify
cargo run --locked --release -- examples/hello.lang --oracle
cargo run --locked --release -- examples/hello.lang --compare-backends
cargo run --locked --release -- examples/hello.lang --trace-json trace.json
cargo run --locked --release -- examples/hello.lang --trace-replay --audit-json trace-replay.audit.json
cargo run --locked --release -- examples/hello.lang --trace-diff --audit-json trace-diff.audit.json
```

Run deterministic fuzz audits with the same seed shape used by CI:

```bash
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0x5eed --fuzz-artifacts fuzz-artifacts/seed-5eed --fuzz-json fuzz-summary-5eed.json
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0xc0ffee --fuzz-artifacts fuzz-artifacts/seed-c0ffee --fuzz-json fuzz-summary-c0ffee.json
cargo run --locked --release -- --fuzz 150 --fuzz-seed 0xbadc0de --fuzz-mode optimizer-stress --fuzz-artifacts fuzz-artifacts/optimizer --fuzz-json fuzz-optimizer.json
cargo run --locked --release -- --evidence-report evidence/latest
```

Run the broader Rust checks:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo bench --no-run --locked
```

On Windows, Rust may pick up Git's `usr/bin/link.exe` before MSVC's linker if
the PATH is misconfigured. In that case, run from a Visual Studio Developer
Command Prompt or rely on CI for the Linux/Windows compile signal.

## Failure Artifacts

When `--fuzz` finds a failing case and `--fuzz-artifacts <dir>` is set, it
writes a minimized repro package under that directory:

```text
original.lang
minimized.lang
manifest.txt
failure.txt
bytecode.txt
vm.trace.json
gc_vm.trace.json
```

Those artifacts are meant to make a backend mismatch or trace divergence
debuggable without rerunning a large random search. The manifest records the
run seed, case seed, source hashes, case feature coverage, and the shortest
fuzzer command that should reproduce the same failing case.

The shrinker is AST-aware before it falls back to line removal. It tries to
remove helper functions, reduce statement bodies, collapse branches, simplify
expressions, and replace array operations with smaller equivalent candidates
while preserving the same failure fingerprint.

The fuzzer also runs metamorphic variants of generated programs. These variants
add neutral arithmetic, dead branches, and unused local work, then require the
AST oracle observable result to stay unchanged while each variant still passes
the normal verifier/backend/trace audit pipeline.

Use `--fuzz-corpus-out tests/corpus` when you intentionally want a minimized
fuzzer failure to become a checked-in regression input. The corpus runner in
`tests/corpus_tests.rs` sends every `tests/corpus/*.lang` file through
verification, backend comparison, trace replay, and VM/GC trace diff.

CI runs a seed/mode fuzz matrix on pushes. Scheduled runs use the same matrix
with a larger case count so the cheap push signal stays quick while the nightly
search gets broader coverage.

## Evidence Reports

`--evidence-report <dir>` creates:

```text
report.json
report.md
fuzz-artifacts/
```

The JSON is meant for automation. The Markdown is meant for reviewers: it shows
the corpus status, fuzz matrix coverage, backend matrix, and any minimized bug
artifacts discovered under `fuzz-artifacts/`.

## Backend Boundaries

The bytecode VM is the reference backend. The GC VM is expected to match it for
programs that do not depend on backend allocation internals. The optimized VM
must preserve observable behavior after bytecode optimization.

The JIT is intentionally narrow. On Linux x86-64 it accepts only linear, pure,
single-function scalar bytecode with constants, arithmetic/comparison/logical
stack operations, and scalar local load/store. Globals, arrays, calls,
control-flow jumps, division, `print`, and multiple functions are rejected for
JIT execution and handled by the VM path.
