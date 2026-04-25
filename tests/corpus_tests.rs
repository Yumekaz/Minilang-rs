use std::fs;
use std::path::Path;

use minilang::{compare_backends, compile, diff_vm_gc_traces, replay_vm_trace, Verifier};

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
        let compiled = compile(&source)
            .unwrap_or_else(|err| panic!("{} failed to compile: {}", path.display(), err));

        let verification = Verifier::new().verify(&compiled);
        assert!(
            verification.valid,
            "{} failed verification:\n{}",
            path.display(),
            verification
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
