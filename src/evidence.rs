//! One-command evidence report for Qydrel.
//!
//! The report aggregates the checks that already exist in the project: corpus
//! audit, AST oracle comparison, backend matrix, trace replay/diff fingerprints,
//! fuzz coverage, and minimized bug artifact discovery.

use crate::ast::Program;
use crate::compare::{BackendRun, BackendRunStatus};
use crate::compiler::{CompiledProgram, Compiler};
use crate::fuzz::{run_fuzzer, FuzzConfig, FuzzMode, FuzzReport};
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::sema::SemanticAnalyzer;
use crate::trace::push_json_string;
use crate::{compare_ast_oracle, diff_vm_gc_traces, replay_vm_trace, Verifier};
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_CORPUS_DIR: &str = "tests/corpus";
const DEFAULT_ARTIFACT_SCAN_DIR: &str = "fuzz-artifacts";
const DEFAULT_FUZZ_CASES: usize = 150;
const DEFAULT_FUZZ_SEEDS: [u64; 4] = [0x5eed, 0xc0ffee, 0xbadc0de, 0x51ced];
const DEFAULT_FUZZ_MODES: [FuzzMode; 2] = [FuzzMode::General, FuzzMode::OptimizerStress];

#[derive(Debug, Clone)]
pub struct EvidenceConfig {
    pub output_dir: PathBuf,
    pub corpus_dir: PathBuf,
    pub artifact_scan_dir: PathBuf,
    pub fuzz_cases: usize,
    pub fuzz_seeds: Vec<u64>,
    pub fuzz_modes: Vec<FuzzMode>,
}

#[derive(Debug, Clone)]
pub struct EvidenceReport {
    pub generated_at_unix: u64,
    pub summary: EvidenceSummary,
    pub corpus: Vec<CorpusEvidence>,
    pub fuzz: Vec<FuzzRunEvidence>,
    pub artifacts: Vec<BugArtifactEvidence>,
}

#[derive(Debug, Clone, Default)]
pub struct EvidenceSummary {
    pub passed: bool,
    pub corpus_files: usize,
    pub corpus_failures: usize,
    pub fuzz_runs: usize,
    pub fuzz_cases_executed: usize,
    pub fuzz_failures: usize,
    pub backend_mismatches: usize,
    pub oracle_mismatches: usize,
    pub trace_failures: usize,
    pub historical_artifacts: usize,
}

#[derive(Debug, Clone)]
pub struct CorpusEvidence {
    pub path: String,
    pub source_hash: u64,
    pub compile_passed: bool,
    pub verify_passed: bool,
    pub oracle_equivalent: bool,
    pub backend_equivalent: bool,
    pub trace_replay_passed: bool,
    pub trace_replay_fingerprint: Option<String>,
    pub trace_diff_passed: bool,
    pub trace_diff_fingerprint: Option<String>,
    pub backend_matrix: Vec<BackendCell>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackendCell {
    pub backend: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct FuzzRunEvidence {
    pub seed: u64,
    pub mode: FuzzMode,
    pub report: FuzzReport,
}

#[derive(Debug, Clone)]
pub struct BugArtifactEvidence {
    pub path: String,
    pub has_manifest: bool,
    pub has_minimized_source: bool,
}

pub fn generate_evidence_report(config: EvidenceConfig) -> io::Result<EvidenceReport> {
    fs::create_dir_all(&config.output_dir)?;

    let corpus = audit_corpus_dir(&config.corpus_dir)?;
    let fuzz = run_fuzz_matrix(&config);
    let artifacts = scan_bug_artifacts(&config.artifact_scan_dir)?;

    let summary = summarize(&corpus, &fuzz, &artifacts);
    let report = EvidenceReport {
        generated_at_unix: current_unix_time(),
        summary,
        corpus,
        fuzz,
        artifacts,
    };

    fs::write(config.output_dir.join("report.json"), report.to_json())?;
    fs::write(config.output_dir.join("report.md"), report.to_markdown())?;
    Ok(report)
}

impl Default for EvidenceConfig {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("evidence/latest"),
            corpus_dir: PathBuf::from(DEFAULT_CORPUS_DIR),
            artifact_scan_dir: PathBuf::from(DEFAULT_ARTIFACT_SCAN_DIR),
            fuzz_cases: DEFAULT_FUZZ_CASES,
            fuzz_seeds: DEFAULT_FUZZ_SEEDS.to_vec(),
            fuzz_modes: DEFAULT_FUZZ_MODES.to_vec(),
        }
    }
}

fn audit_corpus_dir(corpus_dir: &Path) -> io::Result<Vec<CorpusEvidence>> {
    if !corpus_dir.exists() {
        return Ok(Vec::new());
    }

    let mut paths = fs::read_dir(corpus_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("lang"))
        .collect::<Vec<_>>();
    paths.sort();

    let mut entries = Vec::new();
    for path in paths {
        entries.push(audit_source_file(&path));
    }
    Ok(entries)
}

fn audit_source_file(path: &Path) -> CorpusEvidence {
    let path_label = path.display().to_string();
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) => {
            return CorpusEvidence::failed(path_label, 0, format!("read failed: {}", err));
        }
    };
    let source_hash = stable_hash(&source);

    let (program, compiled) = match parse_analyze_compile(&source) {
        Ok(parts) => parts,
        Err(err) => return CorpusEvidence::failed(path_label, source_hash, err),
    };

    let verification = Verifier::new().verify(&compiled);
    let oracle = compare_ast_oracle(&program, &compiled);
    let backend = oracle.backend_report.clone();
    let replay = replay_vm_trace(&compiled);
    let trace_diff = diff_vm_gc_traces(&compiled);

    CorpusEvidence {
        path: path_label,
        source_hash,
        compile_passed: true,
        verify_passed: verification.valid,
        oracle_equivalent: oracle.equivalent,
        backend_equivalent: backend.equivalent,
        trace_replay_passed: replay.replayable,
        trace_replay_fingerprint: Some(replay.fingerprint_hex()),
        trace_diff_passed: trace_diff.equivalent,
        trace_diff_fingerprint: Some(trace_diff.fingerprint_hex()),
        backend_matrix: backend.runs.iter().map(backend_cell).collect(),
        error: if verification.valid
            && oracle.equivalent
            && backend.equivalent
            && replay.replayable
            && trace_diff.equivalent
        {
            None
        } else {
            Some("one or more audit checks failed".to_string())
        },
    }
}

fn parse_analyze_compile(source: &str) -> Result<(Program, CompiledProgram), String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let program = parser
        .parse()
        .map_err(|err| format!("parse failed: {}", err))?;
    SemanticAnalyzer::new()
        .analyze(&program)
        .map_err(|errors| {
            errors
                .iter()
                .map(|err| err.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })?;
    let compiled = Compiler::new().compile(&program).0;
    Ok((program, compiled))
}

fn backend_cell(run: &BackendRun) -> BackendCell {
    match &run.status {
        BackendRunStatus::Executed(outcome) => BackendCell {
            backend: run.name.clone(),
            status: "executed".to_string(),
            detail: format!(
                "success={}, return={}, trap={:?}",
                outcome.success, outcome.return_value, outcome.trap_code
            ),
        },
        BackendRunStatus::Skipped(reason) => BackendCell {
            backend: run.name.clone(),
            status: "skipped".to_string(),
            detail: reason.clone(),
        },
    }
}

fn run_fuzz_matrix(config: &EvidenceConfig) -> Vec<FuzzRunEvidence> {
    let mut runs = Vec::new();
    for mode in &config.fuzz_modes {
        for seed in &config.fuzz_seeds {
            let report = run_fuzzer(FuzzConfig {
                seed: *seed,
                cases: config.fuzz_cases,
                artifact_dir: Some(config.output_dir.join("fuzz-artifacts").join(format!(
                    "{}-{:016x}",
                    mode.as_str(),
                    *seed
                ))),
                mode: *mode,
                ..FuzzConfig::default()
            });
            runs.push(FuzzRunEvidence {
                seed: *seed,
                mode: *mode,
                report,
            });
        }
    }
    runs
}

fn scan_bug_artifacts(root: &Path) -> io::Result<Vec<BugArtifactEvidence>> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut artifacts = Vec::new();
    scan_bug_artifacts_recursive(root, &mut artifacts)?;
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(artifacts)
}

fn scan_bug_artifacts_recursive(
    root: &Path,
    artifacts: &mut Vec<BugArtifactEvidence>,
) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            let manifest = path.join("manifest.txt");
            let minimized = path.join("minimized.lang");
            if manifest.exists() || minimized.exists() {
                artifacts.push(BugArtifactEvidence {
                    path: path.display().to_string(),
                    has_manifest: manifest.exists(),
                    has_minimized_source: minimized.exists(),
                });
            }
            scan_bug_artifacts_recursive(&path, artifacts)?;
        }
    }
    Ok(())
}

fn summarize(
    corpus: &[CorpusEvidence],
    fuzz: &[FuzzRunEvidence],
    artifacts: &[BugArtifactEvidence],
) -> EvidenceSummary {
    let corpus_failures = corpus.iter().filter(|entry| !entry.passed()).count();
    let fuzz_failures = fuzz.iter().filter(|entry| !entry.report.success).count();
    let backend_mismatches = corpus
        .iter()
        .filter(|entry| !entry.backend_equivalent)
        .count();
    let oracle_mismatches = corpus
        .iter()
        .filter(|entry| !entry.oracle_equivalent)
        .count();
    let trace_failures = corpus
        .iter()
        .filter(|entry| !entry.trace_replay_passed || !entry.trace_diff_passed)
        .count();

    EvidenceSummary {
        passed: corpus_failures == 0 && fuzz_failures == 0,
        corpus_files: corpus.len(),
        corpus_failures,
        fuzz_runs: fuzz.len(),
        fuzz_cases_executed: fuzz.iter().map(|entry| entry.report.cases_executed).sum(),
        fuzz_failures,
        backend_mismatches,
        oracle_mismatches,
        trace_failures,
        historical_artifacts: artifacts.len(),
    }
}

impl CorpusEvidence {
    fn failed(path: String, source_hash: u64, error: String) -> Self {
        Self {
            path,
            source_hash,
            compile_passed: false,
            verify_passed: false,
            oracle_equivalent: false,
            backend_equivalent: false,
            trace_replay_passed: false,
            trace_replay_fingerprint: None,
            trace_diff_passed: false,
            trace_diff_fingerprint: None,
            backend_matrix: Vec::new(),
            error: Some(error),
        }
    }

    fn passed(&self) -> bool {
        self.compile_passed
            && self.verify_passed
            && self.oracle_equivalent
            && self.backend_equivalent
            && self.trace_replay_passed
            && self.trace_diff_passed
    }
}

impl EvidenceReport {
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        out.push_str("\"schema_version\":1");
        write!(out, ",\"generated_at_unix\":{}", self.generated_at_unix)
            .expect("write to string cannot fail");
        out.push_str(",\"summary\":");
        push_summary_json(&mut out, &self.summary);
        out.push_str(",\"corpus\":[");
        for (index, entry) in self.corpus.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_corpus_json(&mut out, entry);
        }
        out.push_str("],\"fuzz\":[");
        for (index, entry) in self.fuzz.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_fuzz_json(&mut out, entry);
        }
        out.push_str("],\"artifacts\":[");
        for (index, artifact) in self.artifacts.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_artifact_json(&mut out, artifact);
        }
        out.push_str("]}");
        out
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Qydrel Evidence Report\n\n");
        out.push_str("## Verdict\n\n");
        writeln!(
            out,
            "{}: corpus files={}, fuzz runs={}, fuzz cases executed={}, historical artifacts={}.",
            if self.summary.passed {
                "Passed"
            } else {
                "Failed"
            },
            self.summary.corpus_files,
            self.summary.fuzz_runs,
            self.summary.fuzz_cases_executed,
            self.summary.historical_artifacts
        )
        .expect("write to string cannot fail");

        out.push_str("\n## Corpus Status\n\n");
        out.push_str("| File | Compile | Verify | Oracle | Backends | Replay | VM/GC Diff |\n");
        out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
        for entry in &self.corpus {
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} |",
                entry.path,
                status(entry.compile_passed),
                status(entry.verify_passed),
                status(entry.oracle_equivalent),
                status(entry.backend_equivalent),
                status(entry.trace_replay_passed),
                status(entry.trace_diff_passed)
            )
            .expect("write to string cannot fail");
        }

        out.push_str("\n## Fuzz Coverage\n\n");
        out.push_str(
            "| Seed | Mode | Cases | Status | Branches | Loops | Local Arrays | Opcodes |\n",
        );
        out.push_str("| --- | --- | ---: | --- | ---: | ---: | ---: | ---: |\n");
        for entry in &self.fuzz {
            writeln!(
                out,
                "| {:#018x} | {} | {} | {} | {} | {} | {} | {} |",
                entry.seed,
                entry.mode.as_str(),
                entry.report.cases_executed,
                status(entry.report.success),
                entry.report.coverage.branches,
                entry.report.coverage.loops,
                entry.report.coverage.local_array_reads + entry.report.coverage.local_array_writes,
                entry.report.coverage.opcode_kinds.len()
            )
            .expect("write to string cannot fail");
        }

        out.push_str("\n## Backend Matrix\n\n");
        out.push_str("| Program | VM | GC VM | Optimized VM | JIT |\n");
        out.push_str("| --- | --- | --- | --- | --- |\n");
        for entry in &self.corpus {
            writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                entry.path,
                backend_status(entry, "VM"),
                backend_status(entry, "GC VM"),
                backend_status(entry, "Optimized VM"),
                backend_status(entry, "JIT")
            )
            .expect("write to string cannot fail");
        }

        out.push_str("\n## Historical / Minimized Bugs\n\n");
        out.push_str("| Artifact | Manifest | Minimized Source |\n");
        out.push_str("| --- | --- | --- |\n");
        if self.artifacts.is_empty() {
            out.push_str("| none | no | no |\n");
        } else {
            for artifact in &self.artifacts {
                writeln!(
                    out,
                    "| {} | {} | {} |",
                    artifact.path,
                    status(artifact.has_manifest),
                    status(artifact.has_minimized_source)
                )
                .expect("write to string cannot fail");
            }
        }

        out
    }
}

fn push_summary_json(out: &mut String, summary: &EvidenceSummary) {
    out.push('{');
    write!(out, "\"passed\":{}", summary.passed).expect("write to string cannot fail");
    write!(out, ",\"corpus_files\":{}", summary.corpus_files).expect("write to string cannot fail");
    write!(out, ",\"corpus_failures\":{}", summary.corpus_failures)
        .expect("write to string cannot fail");
    write!(out, ",\"fuzz_runs\":{}", summary.fuzz_runs).expect("write to string cannot fail");
    write!(
        out,
        ",\"fuzz_cases_executed\":{}",
        summary.fuzz_cases_executed
    )
    .expect("write to string cannot fail");
    write!(out, ",\"fuzz_failures\":{}", summary.fuzz_failures)
        .expect("write to string cannot fail");
    write!(
        out,
        ",\"backend_mismatches\":{}",
        summary.backend_mismatches
    )
    .expect("write to string cannot fail");
    write!(out, ",\"oracle_mismatches\":{}", summary.oracle_mismatches)
        .expect("write to string cannot fail");
    write!(out, ",\"trace_failures\":{}", summary.trace_failures)
        .expect("write to string cannot fail");
    write!(
        out,
        ",\"historical_artifacts\":{}",
        summary.historical_artifacts
    )
    .expect("write to string cannot fail");
    out.push('}');
}

fn push_corpus_json(out: &mut String, entry: &CorpusEvidence) {
    out.push('{');
    out.push_str("\"path\":");
    push_json_string(out, &entry.path);
    write!(out, ",\"source_hash\":\"{:016x}\"", entry.source_hash)
        .expect("write to string cannot fail");
    write!(out, ",\"compile_passed\":{}", entry.compile_passed)
        .expect("write to string cannot fail");
    write!(out, ",\"verify_passed\":{}", entry.verify_passed).expect("write to string cannot fail");
    write!(out, ",\"oracle_equivalent\":{}", entry.oracle_equivalent)
        .expect("write to string cannot fail");
    write!(out, ",\"backend_equivalent\":{}", entry.backend_equivalent)
        .expect("write to string cannot fail");
    write!(
        out,
        ",\"trace_replay_passed\":{}",
        entry.trace_replay_passed
    )
    .expect("write to string cannot fail");
    out.push_str(",\"trace_replay_fingerprint\":");
    push_optional_string(out, entry.trace_replay_fingerprint.as_deref());
    write!(out, ",\"trace_diff_passed\":{}", entry.trace_diff_passed)
        .expect("write to string cannot fail");
    out.push_str(",\"trace_diff_fingerprint\":");
    push_optional_string(out, entry.trace_diff_fingerprint.as_deref());
    out.push_str(",\"backend_matrix\":[");
    for (index, cell) in entry.backend_matrix.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_backend_cell_json(out, cell);
    }
    out.push_str("],\"error\":");
    push_optional_string(out, entry.error.as_deref());
    out.push('}');
}

fn push_backend_cell_json(out: &mut String, cell: &BackendCell) {
    out.push('{');
    out.push_str("\"backend\":");
    push_json_string(out, &cell.backend);
    out.push_str(",\"status\":");
    push_json_string(out, &cell.status);
    out.push_str(",\"detail\":");
    push_json_string(out, &cell.detail);
    out.push('}');
}

fn push_fuzz_json(out: &mut String, entry: &FuzzRunEvidence) {
    out.push('{');
    write!(out, "\"seed\":\"{:#018x}\"", entry.seed).expect("write to string cannot fail");
    out.push_str(",\"mode\":");
    push_json_string(out, entry.mode.as_str());
    out.push_str(",\"report\":");
    out.push_str(&entry.report.to_json());
    out.push('}');
}

fn push_artifact_json(out: &mut String, artifact: &BugArtifactEvidence) {
    out.push('{');
    out.push_str("\"path\":");
    push_json_string(out, &artifact.path);
    write!(out, ",\"has_manifest\":{}", artifact.has_manifest)
        .expect("write to string cannot fail");
    write!(
        out,
        ",\"has_minimized_source\":{}",
        artifact.has_minimized_source
    )
    .expect("write to string cannot fail");
    out.push('}');
}

fn push_optional_string(out: &mut String, value: Option<&str>) {
    match value {
        Some(value) => push_json_string(out, value),
        None => out.push_str("null"),
    }
}

fn backend_status(entry: &CorpusEvidence, name: &str) -> String {
    entry
        .backend_matrix
        .iter()
        .find(|cell| cell.backend == name)
        .map(|cell| cell.status.clone())
        .unwrap_or_else(|| "n/a".to_string())
}

fn status(value: bool) -> &'static str {
    if value {
        "passed"
    } else {
        "failed"
    }
}

fn stable_hash(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_report_renders_json_and_markdown() {
        let config = EvidenceConfig {
            output_dir: std::env::temp_dir()
                .join(format!("qydrel-evidence-test-{}", current_unix_time())),
            corpus_dir: PathBuf::from("tests/corpus"),
            fuzz_cases: 1,
            fuzz_seeds: vec![0x5eed],
            fuzz_modes: vec![FuzzMode::General],
            artifact_scan_dir: PathBuf::from("missing-fuzz-artifacts-for-test"),
        };

        let report = generate_evidence_report(config).expect("report should generate");
        assert!(report.summary.corpus_files >= 3);
        assert_eq!(report.summary.fuzz_runs, 1);
        assert!(report.to_json().contains("\"schema_version\":1"));
        assert!(report.to_json().contains("\"oracle_equivalent\""));
        assert!(report.to_markdown().contains("# Qydrel Evidence Report"));
    }
}
