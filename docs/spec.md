# MiniLang Specification

This document describes the behavior MiniLang commits to today. If code and
older docs disagree with this file, this file should win.

## Program Shape

A program is a sequence of global variable declarations and function
declarations. Exactly one `main` function must exist, and it must take no
parameters. All functions currently return `int`; there are no declared return
types in the source syntax.

## Lexical Rules

Identifiers match `[a-zA-Z_][a-zA-Z0-9_]*`.

Integer literals are base-10 non-negative literals. Negative values are parsed
as unary negation applied to an integer literal.

Line comments start with `//` and continue to the end of the line.

Keywords are:

```text
int bool if else while return print func true false
```

## Types

MiniLang has two source-level scalar types:

```text
int
bool
```

Arrays are fixed-size declarations of `int` or `bool` elements. Array sizes are
compile-time integer literals.

## Scope And Names

Names must be declared before use.

MiniLang currently rejects shadowing: a local variable or parameter may not use
the same name as another visible local, parameter, global, or function. This
keeps the language aligned with the current bytecode compiler, which assigns
function-local slots by name.

Function names share the top-level namespace with global variables.

## Expressions

Arithmetic operators require `int` operands and produce `int`:

```text
+ - * /
```

Comparison operators require `int` operands and produce `bool`:

```text
< > <= >=
```

Equality operators require both operands to have the same type and produce
`bool`:

```text
== !=
```

Logical operators accept `int` or `bool` operands and produce `bool`:

```text
&& || !
```

`&&` and `||` short-circuit. Conditions treat `0` as false and non-zero
integers as true.

## Integers

Integer arithmetic uses signed 32-bit two's-complement wrapping for `+`, `-`,
and `*`. Division by zero traps at runtime.

## Variables And Arrays

Globals are initialized to zero before `main`. Global initializer expressions
are evaluated before the body of `main` executes.

Local scalar variables without initializers are uninitialized. Reading one traps
with `TRAP_UNDEFINED_LOCAL`.

Arrays are zero-indexed and bounds-checked. Out-of-bounds access traps with
`TRAP_ARRAY_OOB`. The default VM stores arrays in VM-owned storage; the `--gc`
VM stores arrays as heap objects and traces references from the stack, globals,
and call frames.

## Functions

Function arguments are evaluated left-to-right. Calls must match the callee's
parameter count and parameter types.

Recursion is supported by the VM. Exceeding the frame limit traps with
`TRAP_STACK_OVERFLOW`.

## Runtime Traps

| Code | Name | Meaning |
| ---: | ---- | ------- |
| 1 | `TRAP_DIV_ZERO` | Division by zero |
| 2 | `TRAP_UNDEFINED_LOCAL` | Read of an uninitialized local |
| 3 | `TRAP_ARRAY_OOB` | Array index out of bounds |
| 4 | `TRAP_STACK_OVERFLOW` | Call stack exceeded the frame limit |
| 5 | `TRAP_CYCLE_LIMIT` | Execution exceeded the cycle limit |
| 6 | `TRAP_UNDEFINED_FUNCTION` | Bytecode references a missing function |
| 7 | `TRAP_STACK_UNDERFLOW` | Bytecode popped from an empty operand stack |
| 8 | `TRAP_INVALID_INSTRUCTION` | Bytecode could not be executed by this backend |

The public `VmResult` and `GcVmResult` values are the stable programmatic trap
interface. CLI wording may change as diagnostics improve.

## Execution Backends

The bytecode VM is the reference backend.

The GC VM must match the reference VM for programs that do not depend on backend
allocation details.

The hand-written x86-64 JIT is experimental. It currently targets Linux x86-64
and simple single-function programs; unsupported programs fall back to the VM.
