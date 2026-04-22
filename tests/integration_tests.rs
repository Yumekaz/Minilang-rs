//! Integration tests for MiniLang VM
//!
//! Tests match the Python implementation behavior exactly.

use minilang::{run, TrapCode};

fn run_expect(source: &str, expected: i64) {
    let result = run(source).expect("compilation failed");
    assert!(
        result.success,
        "VM trap: {} ({:?})",
        result.trap_message, result.trap_code
    );
    assert_eq!(result.return_value, expected, "wrong return value");
}

fn run_expect_trap(source: &str, expected_trap: TrapCode) {
    let result = run(source).expect("compilation failed");
    assert!(!result.success, "expected trap but got success");
    assert_eq!(result.trap_code, expected_trap, "wrong trap code");
}

// ============================================================================
// Basic Tests
// ============================================================================

#[test]
fn test_return_constant() {
    run_expect("func main() { return 42; }", 42);
}

#[test]
fn test_return_zero() {
    run_expect("func main() { return 0; }", 0);
}

#[test]
fn test_return_negative() {
    run_expect("func main() { return -1; }", -1);
}

// ============================================================================
// Arithmetic
// ============================================================================

#[test]
fn test_add() {
    run_expect("func main() { return 10 + 20; }", 30);
}

#[test]
fn test_sub() {
    run_expect("func main() { return 50 - 30; }", 20);
}

#[test]
fn test_mul() {
    run_expect("func main() { return 6 * 7; }", 42);
}

#[test]
fn test_div() {
    run_expect("func main() { return 100 / 10; }", 10);
}

#[test]
fn test_precedence() {
    run_expect("func main() { return 1 + 2 * 3; }", 7);
    run_expect("func main() { return (1 + 2) * 3; }", 9);
}

#[test]
fn test_negation() {
    run_expect("func main() { return -(-5); }", 5);
}

// ============================================================================
// 32-bit Overflow
// ============================================================================

#[test]
fn test_overflow_add() {
    run_expect("func main() { return 2147483647 + 1; }", -2147483648);
}

#[test]
fn test_overflow_sub() {
    run_expect("func main() { return -2147483648 - 1; }", 2147483647);
}

#[test]
fn test_overflow_mul() {
    run_expect("func main() { return 100000 * 100000; }", 1410065408);
}

// ============================================================================
// Comparisons
// ============================================================================

#[test]
fn test_eq() {
    run_expect("func main() { if (5 == 5) { return 1; } return 0; }", 1);
    run_expect("func main() { if (5 == 6) { return 1; } return 0; }", 0);
}

#[test]
fn test_ne() {
    run_expect("func main() { if (5 != 6) { return 1; } return 0; }", 1);
    run_expect("func main() { if (5 != 5) { return 1; } return 0; }", 0);
}

#[test]
fn test_lt() {
    run_expect("func main() { if (3 < 5) { return 1; } return 0; }", 1);
    run_expect("func main() { if (5 < 3) { return 1; } return 0; }", 0);
}

#[test]
fn test_gt() {
    run_expect("func main() { if (5 > 3) { return 1; } return 0; }", 1);
    run_expect("func main() { if (3 > 5) { return 1; } return 0; }", 0);
}

#[test]
fn test_le() {
    run_expect("func main() { if (3 <= 5) { return 1; } return 0; }", 1);
    run_expect("func main() { if (5 <= 5) { return 1; } return 0; }", 1);
    run_expect("func main() { if (6 <= 5) { return 1; } return 0; }", 0);
}

#[test]
fn test_ge() {
    run_expect("func main() { if (5 >= 3) { return 1; } return 0; }", 1);
    run_expect("func main() { if (5 >= 5) { return 1; } return 0; }", 1);
    run_expect("func main() { if (3 >= 5) { return 1; } return 0; }", 0);
}

// ============================================================================
// Logical Operators
// ============================================================================

#[test]
fn test_and() {
    run_expect("func main() { if (1 && 1) { return 1; } return 0; }", 1);
    run_expect("func main() { if (1 && 0) { return 1; } return 0; }", 0);
    run_expect("func main() { if (0 && 1) { return 1; } return 0; }", 0);
}

#[test]
fn test_or() {
    run_expect("func main() { if (1 || 0) { return 1; } return 0; }", 1);
    run_expect("func main() { if (0 || 1) { return 1; } return 0; }", 1);
    run_expect("func main() { if (0 || 0) { return 1; } return 0; }", 0);
}

#[test]
fn test_not() {
    run_expect("func main() { if (!0) { return 1; } return 0; }", 1);
    run_expect("func main() { if (!1) { return 1; } return 0; }", 0);
}

#[test]
fn test_logical_short_circuit_avoids_rhs_traps() {
    run_expect(
        "func main() { if (0 && (10 / 0)) { return 1; } return 2; }",
        2,
    );
    run_expect(
        "func main() { if (1 || (10 / 0)) { return 3; } return 4; }",
        3,
    );
}

// ============================================================================
// Variables
// ============================================================================

#[test]
fn test_local_var() {
    run_expect("func main() { int x = 10; return x; }", 10);
}

#[test]
fn test_local_var_update() {
    run_expect("func main() { int x = 10; x = x + 5; return x; }", 15);
}

#[test]
fn test_multiple_locals() {
    run_expect("func main() { int x = 10; int y = 20; return x + y; }", 30);
}

#[test]
fn test_global_var() {
    run_expect("int g = 100; func main() { return g; }", 100);
}

#[test]
fn test_global_var_update() {
    run_expect("int g = 100; func main() { g = g + 1; return g; }", 101);
}

#[test]
fn test_global_initializer_expression() {
    run_expect("int g = 40 + 2; func main() { return g; }", 42);
}

// ============================================================================
// Control Flow
// ============================================================================

#[test]
fn test_if_true() {
    run_expect("func main() { if (1) { return 42; } return 0; }", 42);
}

#[test]
fn test_if_false() {
    run_expect("func main() { if (0) { return 42; } return 0; }", 0);
}

#[test]
fn test_if_else() {
    run_expect("func main() { if (0) { return 1; } else { return 2; } }", 2);
}

#[test]
fn test_while_loop() {
    run_expect(
        "func main() { int i = 0; int s = 0; while (i < 10) { s = s + i; i = i + 1; } return s; }",
        45,
    );
}

// ============================================================================
// Functions
// ============================================================================

#[test]
fn test_function_call() {
    run_expect(
        "func add(int a, int b) { return a + b; } func main() { return add(3, 4); }",
        7,
    );
}

#[test]
fn test_recursion() {
    run_expect(
        "func fact(int n) { if (n <= 1) { return 1; } return n * fact(n - 1); } func main() { return fact(5); }",
        120
    );
}

#[test]
fn test_fibonacci() {
    run_expect(
        "func fib(int n) { if (n <= 1) { return n; } return fib(n-1) + fib(n-2); } func main() { return fib(10); }",
        55
    );
}

// ============================================================================
// Arrays
// ============================================================================

#[test]
fn test_array_basic() {
    run_expect("func main() { int a[5]; a[0] = 42; return a[0]; }", 42);
}

#[test]
fn test_array_multiple() {
    run_expect(
        "func main() { int a[5]; a[0] = 10; a[1] = 20; return a[0] + a[1]; }",
        30,
    );
}

#[test]
fn test_array_loop() {
    run_expect(
        "func main() { int a[5]; int i = 0; while (i < 5) { a[i] = i; i = i + 1; } return a[0] + a[1] + a[2] + a[3] + a[4]; }",
        10
    );
}

// ============================================================================
// Traps
// ============================================================================

#[test]
fn test_trap_div_zero() {
    run_expect_trap("func main() { return 10 / 0; }", TrapCode::DivideByZero);
}

#[test]
fn test_trap_undefined_local() {
    run_expect_trap("func main() { int x; return x; }", TrapCode::UndefinedLocal);
}

#[test]
fn test_trap_array_oob() {
    run_expect_trap(
        "func main() { int a[5]; return a[10]; }",
        TrapCode::ArrayOutOfBounds,
    );
}

#[test]
fn test_trap_array_negative_index() {
    run_expect_trap(
        "func main() { int a[5]; return a[-1]; }",
        TrapCode::ArrayOutOfBounds,
    );
}

#[test]
fn test_trap_stack_overflow() {
    run_expect_trap(
        "func inf(int n) { return inf(n + 1); } func main() { return inf(0); }",
        TrapCode::StackOverflow,
    );
}
