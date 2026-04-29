use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use minilang::{compare_backends, compile, BackendRunStatus, TrapCode, Verifier};

#[derive(Debug)]
struct BugCase {
    id: String,
    root: PathBuf,
    status: String,
    expected: String,
    proof_gate: String,
    source: String,
}

#[test]
fn bug_museum_entries_are_documented() {
    let cases = load_bug_cases();
    assert!(
        !cases.is_empty(),
        "expected at least one checked-in bug case"
    );

    for case in cases {
        assert!(
            case.root.join("README.md").exists(),
            "{} should have a short README",
            case.id
        );
        assert!(
            !case.status.is_empty(),
            "{} should document fixed/current status",
            case.id
        );
        assert!(
            !case.expected.is_empty(),
            "{} should document expected behavior",
            case.id
        );
        assert!(
            !case.proof_gate.is_empty(),
            "{} should document the proof gate or checker",
            case.id
        );
        assert!(
            !case.source.trim().is_empty(),
            "{} should include a minimized source repro",
            case.id
        );
    }
}

#[test]
fn bug_museum_expected_behaviors_are_enforced() {
    for case in load_bug_cases() {
        match case.expected.as_str() {
            "vm_trap_undefined_local_jit_skipped" => {
                assert_undefined_local_jit_gate(&case);
            }
            other => panic!(
                "{} documents expected behavior '{}' but the museum test runner does not know how to audit it",
                case.id, other
            ),
        }
    }
}

fn assert_undefined_local_jit_gate(case: &BugCase) {
    assert_eq!(
        case.status.as_str(),
        "fixed",
        "{} should be labelled fixed only when the proof gate is in place",
        case.id
    );

    let compiled =
        compile(&case.source).unwrap_or_else(|err| panic!("{} did not compile: {}", case.id, err));
    let verification = Verifier::new().verify(&compiled);
    assert!(
        verification.valid,
        "{} should remain structurally valid bytecode:\n{}",
        case.id, verification
    );
    assert!(
        verification
            .possible_traps
            .contains(&TrapCode::UndefinedLocal),
        "{} should be reported as a possible UndefinedLocal trap:\n{}",
        case.id,
        verification
    );
    assert!(
        !verification.backend_eligibility.jit.eligible,
        "{} must not be JIT eligible:\n{}",
        case.id, verification
    );

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let reason = verification
            .backend_eligibility
            .jit
            .reason
            .as_deref()
            .unwrap_or("");
        assert!(
            reason.contains("trap-free") && reason.contains("UndefinedLocal"),
            "{} should be blocked by the trap-free JIT proof gate, got '{}'",
            case.id,
            reason
        );
    }

    let comparison = compare_backends(&compiled);
    assert!(
        comparison.equivalent,
        "{} backend comparison should agree on the trap:\n{}",
        case.id, comparison
    );

    let mut executed_backends = 0usize;
    for run in &comparison.runs {
        if run.name == "JIT" {
            assert!(
                matches!(&run.status, BackendRunStatus::Skipped(reason) if !reason.is_empty()),
                "{} JIT must be skipped, got {:?}",
                case.id,
                &run.status
            );
            continue;
        }

        match &run.status {
            BackendRunStatus::Executed(outcome) => {
                executed_backends += 1;
                assert!(
                    !outcome.success,
                    "{} {} should trap, got success",
                    case.id, run.name
                );
                assert_eq!(
                    outcome.trap_code,
                    TrapCode::UndefinedLocal,
                    "{} {} should trap with UndefinedLocal",
                    case.id,
                    run.name
                );
            }
            BackendRunStatus::Skipped(reason) => {
                panic!("{} {} unexpectedly skipped: {}", case.id, run.name, reason)
            }
        }
    }

    assert!(
        executed_backends >= 3,
        "{} should audit VM, GC VM, and optimized VM",
        case.id
    );
}

fn load_bug_cases() -> Vec<BugCase> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/bugs");
    assert!(root.exists(), "tests/bugs should exist");

    let mut cases = Vec::new();
    collect_bug_cases(&root, &root, &mut cases);
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    cases
}

fn collect_bug_cases(root: &Path, current: &Path, cases: &mut Vec<BugCase>) {
    if current != root
        && (current.join("metadata.txt").exists()
            || current.join("README.md").exists()
            || current.join("repro.lang").exists())
    {
        cases.push(load_bug_case(current));
    }

    for entry in fs::read_dir(current).expect("bug museum directory should be readable") {
        let path = entry.expect("bug museum entry should be readable").path();
        if path.is_dir() {
            collect_bug_cases(root, &path, cases);
        }
    }
}

fn load_bug_case(root: &Path) -> BugCase {
    let metadata = parse_metadata(
        &fs::read_to_string(root.join("metadata.txt")).expect("metadata should be readable"),
    );
    let source_path = root.join("repro.lang");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("{} should be readable: {}", source_path.display(), err));
    let id = required_metadata(root, &metadata, "id");

    BugCase {
        id,
        root: root.to_path_buf(),
        status: required_metadata(root, &metadata, "status"),
        expected: required_metadata(root, &metadata, "expected"),
        proof_gate: required_metadata(root, &metadata, "proof_gate"),
        source,
    }
}

fn parse_metadata(source: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        fields.insert(key.trim().to_string(), value.trim().to_string());
    }
    fields
}

fn required_metadata(root: &Path, metadata: &BTreeMap<String, String>, key: &str) -> String {
    metadata
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| panic!("{} missing metadata field '{}'", root.display(), key))
}
