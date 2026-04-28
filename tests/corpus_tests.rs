use std::fs;
use std::path::Path;

use minilang::{
    compare_ast_oracle, compare_backends, compile, diff_vm_gc_traces, replay_vm_trace, Compiler,
    Lexer, Parser, SemanticAnalyzer, Verifier,
};

#[test]
fn corpus_programs_pass_full_audit_pipeline() {
    let corpus_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let entries = fs::read_dir(&corpus_dir).expect("tests/corpus should exist");
    let mut checked = 0;

    for entry in entries {
        let path = entry.expect("corpus entry should be readable").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("lang") {
            continue;
        }

        let source = fs::read_to_string(&path).expect("corpus source should be readable");
        let mut lexer = Lexer::new(&source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser
            .parse()
            .unwrap_or_else(|err| panic!("{} failed to parse: {}", path.display(), err));
        SemanticAnalyzer::new()
            .analyze(&program)
            .unwrap_or_else(|errors| {
                panic!(
                    "{} failed semantic analysis:\n{}",
                    path.display(),
                    errors
                        .iter()
                        .map(|err| err.to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            });
        let compiled_from_ast = Compiler::new().compile(&program).0;
        let compiled = compile(&source)
            .unwrap_or_else(|err| panic!("{} failed to compile: {}", path.display(), err));
        assert_eq!(
            compiled.instructions.len(),
            compiled_from_ast.instructions.len(),
            "{} compile helper diverged from direct AST compile",
            path.display()
        );

        let verification = Verifier::new().verify(&compiled);
        assert!(
            verification.valid,
            "{} failed verification:\n{}",
            path.display(),
            verification
        );

        let oracle = compare_ast_oracle(&program, &compiled);
        assert!(
            oracle.equivalent,
            "{} AST oracle mismatch:\n{}",
            path.display(),
            oracle
        );

        let comparison = compare_backends(&compiled);
        assert!(
            comparison.equivalent,
            "{} backend mismatch:\n{}",
            path.display(),
            comparison
        );

        let replay = replay_vm_trace(&compiled);
        assert!(
            replay.replayable,
            "{} trace replay failed:\n{}",
            path.display(),
            replay
        );

        let trace_diff = diff_vm_gc_traces(&compiled);
        assert!(
            trace_diff.equivalent,
            "{} VM/GC trace diff failed:\n{}",
            path.display(),
            trace_diff
        );

        checked += 1;
    }

    assert!(checked >= 3, "expected at least three corpus programs");
}
