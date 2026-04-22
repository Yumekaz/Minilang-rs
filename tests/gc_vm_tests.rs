use minilang::{Compiler, GcVm, Lexer, Parser, SemanticAnalyzer, TrapCode, Vm};

fn compile_program(source: &str) -> minilang::CompiledProgram {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().expect("parse failed");
    let mut analyzer = SemanticAnalyzer::new();
    analyzer
        .analyze(&program)
        .expect("semantic analysis failed");
    Compiler::new().compile(&program).0
}

fn run_both_expect(source: &str, expected: i64) {
    let compiled = compile_program(source);

    let mut vm = Vm::new(&compiled);
    let vm_result = vm.run();
    assert!(
        vm_result.success,
        "standard VM trapped: {:?} {}",
        vm_result.trap_code, vm_result.trap_message
    );

    let mut gc_vm = GcVm::new(&compiled);
    let gc_result = gc_vm.run();
    assert!(
        gc_result.success,
        "GC VM trapped: {:?} {}",
        gc_result.trap_code, gc_result.trap_message
    );

    assert_eq!(vm_result.return_value, expected);
    assert_eq!(gc_result.return_value, expected);
}

#[test]
fn gc_vm_matches_standard_vm_for_global_arrays() {
    run_both_expect(
        "int arr[4]; func main() { arr[0] = 3; arr[1] = 5; arr[2] = 8; arr[3] = 13; return arr[0] + arr[1] + arr[2] + arr[3]; }",
        29,
    );
}

#[test]
fn gc_vm_matches_standard_vm_for_local_arrays() {
    run_both_expect(
        "func main() { int arr[4]; int i = 0; while (i < 4) { arr[i] = i + 1; i = i + 1; } return arr[0] * arr[1] * arr[2] * arr[3]; }",
        24,
    );
}

#[test]
fn gc_vm_reports_array_bounds_traps() {
    let compiled = compile_program("func main() { int arr[2]; return arr[-1]; }");
    let mut gc_vm = GcVm::new(&compiled);
    let result = gc_vm.run();

    assert!(!result.success);
    assert_eq!(result.trap_code, TrapCode::ArrayOutOfBounds);
}

#[test]
fn gc_vm_collects_unreachable_local_arrays() {
    let compiled = compile_program(
        "func make() { int temp[4]; temp[0] = 7; return temp[0]; } \
         func main() { int i = 0; int sum = 0; while (i < 16) { sum = sum + make(); i = i + 1; } return sum; }",
    );
    let mut gc_vm = GcVm::new(&compiled);
    let result = gc_vm.run();

    assert!(result.success, "GC VM trapped: {:?}", result.trap_code);
    assert_eq!(result.return_value, 112);
    assert!(
        result.gc_collections > 0,
        "expected enough local arrays to trigger collection"
    );
}
