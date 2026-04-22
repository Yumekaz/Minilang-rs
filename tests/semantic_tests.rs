use minilang::compile;

fn compile_expect_err(source: &str, expected: &str) {
    let err = compile(source).expect_err("expected compilation to fail");
    assert!(
        err.contains(expected),
        "expected error containing {:?}, got:\n{}",
        expected,
        err
    );
}

#[test]
fn missing_main_is_rejected() {
    compile_expect_err("func helper() { return 1; }", "main");
}

#[test]
fn main_with_parameters_is_rejected() {
    compile_expect_err(
        "func main(int argc) { return argc; }",
        "must take no parameters",
    );
}

#[test]
fn function_argument_count_is_checked() {
    compile_expect_err(
        "func add(int a, int b) { return a + b; } func main() { return add(1); }",
        "Wrong number of arguments",
    );
}

#[test]
fn function_argument_type_is_checked() {
    compile_expect_err(
        "func id(int x) { return x; } func main() { return id(true); }",
        "Argument 1 type mismatch",
    );
}

#[test]
fn bool_array_index_is_rejected() {
    compile_expect_err(
        "func main() { int arr[2]; bool b = true; return arr[b]; }",
        "Array index must be int",
    );
}

#[test]
fn duplicate_function_names_are_rejected() {
    compile_expect_err(
        "func main() { return 0; } func main() { return 1; }",
        "Duplicate function",
    );
}

#[test]
fn function_and_global_names_share_a_namespace() {
    compile_expect_err(
        "int value = 1; func value() { return 2; } func main() { return value; }",
        "Duplicate top-level symbol",
    );
}

#[test]
fn assignment_to_function_name_is_rejected() {
    compile_expect_err(
        "func f() { return 1; } func main() { f = 3; return 0; }",
        "Cannot assign to function",
    );
}

#[test]
fn nested_local_shadowing_is_rejected() {
    compile_expect_err(
        "func main() { int x = 1; if (1) { int x = 2; } return x; }",
        "shadows existing symbol",
    );
}

#[test]
fn parameter_shadowing_global_is_rejected() {
    compile_expect_err(
        "int x = 1; func f(int x) { return x; } func main() { return f(2); }",
        "shadows existing symbol",
    );
}

#[test]
fn array_initializer_syntax_is_rejected() {
    compile_expect_err("func main() { int a[3] = 7; return 0; }", "Expected ';'");
}
