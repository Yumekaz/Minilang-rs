//! One-command evidence report for Qydrel.
//!
//! The report aggregates the checks that already exist in the project: corpus
//! audit, AST oracle comparison, backend matrix, trace replay/diff fingerprints,
//! fuzz coverage, and minimized bug artifact discovery.

use crate::ast::Program;
use crate::compare::{BackendRun, BackendRunStatus};
use crate::compiler::{CompiledProgram, Compiler, Opcode};
use crate::fuzz::{run_fuzzer, FuzzConfig, FuzzCoverage, FuzzMode, FuzzReport};
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::sema::SemanticAnalyzer;
use crate::trace::push_json_string;
use crate::{compare_ast_oracle, diff_vm_gc_traces, replay_vm_trace, Verifier};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_CORPUS_DIR: &str = "tests/corpus";
const DEFAULT_ARTIFACT_SCAN_DIR: &str = "fuzz-artifacts";
const DEFAULT_BUG_MUSEUM_DIR: &str = "tests/bugs";
const DEFAULT_FUZZ_CASES: usize = 150;
const DEFAULT_FUZZ_SEEDS: [u64; 4] = [0x5eed, 0xc0ffee, 0xbadc0de, 0x51ced];
const DEFAULT_FUZZ_MODES: [FuzzMode; 2] = [FuzzMode::General, FuzzMode::OptimizerStress];
const ALL_OPCODE_NAMES: [&str; 34] = [
    "LoadConst",
    "LoadLocal",
    "StoreLocal",
    "LoadGlobal",
    "StoreGlobal",
    "Add",
    "Sub",
    "Mul",
    "Div",
    "Neg",
    "Eq",
    "Ne",
    "Lt",
    "Gt",
    "Le",
    "Ge",
    "And",
    "Or",
    "Not",
    "Jump",
    "JumpIfFalse",
    "JumpIfTrue",
    "Call",
    "Return",
    "ArrayLoad",
    "ArrayStore",
    "ArrayNew",
    "LocalArrayLoad",
    "LocalArrayStore",
    "AllocArray",
    "Print",
    "Pop",
    "Dup",
    "Halt",
];

#[derive(Debug, Clone)]
pub struct EvidenceConfig {
    pub output_dir: PathBuf,
    pub corpus_dir: PathBuf,
    pub artifact_scan_dir: PathBuf,
    pub bug_museum_dir: PathBuf,
    pub fuzz_cases: usize,
    pub fuzz_seeds: Vec<u64>,
    pub fuzz_modes: Vec<FuzzMode>,
}

#[derive(Debug, Clone)]
pub struct EvidenceReport {
    pub generated_at_unix: u64,
    pub summary: EvidenceSummary,
    pub coverage: CoverageDashboard,
    pub corpus: Vec<CorpusEvidence>,
    pub fuzz: Vec<FuzzRunEvidence>,
    pub artifacts: Vec<BugArtifactEvidence>,
    pub bug_museum: Vec<BugMuseumEvidence>,
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
    pub bug_museum_entries: usize,
    pub bug_museum_incomplete: usize,
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
    pub opcode_kinds: Vec<String>,
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

#[derive(Debug, Clone)]
pub struct CoverageDashboard {
    pub fuzz_cases: usize,
    pub oracle_comparisons: usize,
    pub metamorphic_variants: usize,
    pub opcodes_seen: usize,
    pub opcodes_total: usize,
    pub feature_rows: Vec<FeatureCoverageRow>,
    pub opcode_rows: Vec<OpcodeCoverageRow>,
}

#[derive(Debug, Clone)]
pub struct FeatureCoverageRow {
    pub feature: String,
    pub cases: usize,
    pub total_cases: usize,
}

#[derive(Debug, Clone)]
pub struct OpcodeCoverageRow {
    pub opcode: String,
    pub corpus_seen: bool,
    pub fuzz_seen: bool,
}

#[derive(Debug, Clone)]
pub struct BugMuseumEvidence {
    pub id: String,
    pub path: String,
    pub status: String,
    pub expected: String,
    pub proof_gate: String,
    pub has_metadata: bool,
    pub has_readme: bool,
    pub has_repro_source: bool,
    pub repro_source_hash: Option<String>,
}

pub fn generate_evidence_report(config: EvidenceConfig) -> io::Result<EvidenceReport> {
    fs::create_dir_all(&config.output_dir)?;

    let corpus = audit_corpus_dir(&config.corpus_dir)?;
    let fuzz = run_fuzz_matrix(&config);
    let artifacts = scan_bug_artifacts(&config.artifact_scan_dir)?;
    let bug_museum = scan_bug_museum(&config.bug_museum_dir)?;
    let coverage = coverage_dashboard(&corpus, &fuzz);

    let summary = summarize(&corpus, &fuzz, &artifacts, &bug_museum);
    let report = EvidenceReport {
        generated_at_unix: current_unix_time(),
        summary,
        coverage,
        corpus,
        fuzz,
        artifacts,
        bug_museum,
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
            bug_museum_dir: PathBuf::from(DEFAULT_BUG_MUSEUM_DIR),
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
    let opcode_kinds = program_opcode_kinds(&compiled);

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
        opcode_kinds,
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

fn scan_bug_museum(root: &Path) -> io::Result<Vec<BugMuseumEvidence>> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    scan_bug_museum_recursive(root, root, &mut entries)?;
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(entries)
}

fn scan_bug_museum_recursive(
    root: &Path,
    current: &Path,
    entries: &mut Vec<BugMuseumEvidence>,
) -> io::Result<()> {
    let metadata = current.join("metadata.txt");
    let readme = current.join("README.md");
    let repro = current.join("repro.lang");
    let minimized = current.join("minimized.lang");
    let repro_path = if repro.exists() { repro } else { minimized };

    if current != root && (metadata.exists() || readme.exists() || repro_path.exists()) {
        let fields = if metadata.exists() {
            parse_metadata(&fs::read_to_string(&metadata)?)
        } else {
            BTreeMap::new()
        };
        let id = fields
            .get("id")
            .cloned()
            .or_else(|| {
                current
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| current.display().to_string());
        let repro_source_hash = if repro_path.exists() {
            Some(format!(
                "{:016x}",
                stable_hash(&fs::read_to_string(&repro_path)?)
            ))
        } else {
            None
        };

        entries.push(BugMuseumEvidence {
            id,
            path: current.display().to_string(),
            status: metadata_value(&fields, "status"),
            expected: metadata_value(&fields, "expected"),
            proof_gate: metadata_value(&fields, "proof_gate"),
            has_metadata: metadata.exists(),
            has_readme: readme.exists(),
            has_repro_source: repro_path.exists(),
            repro_source_hash,
        });
    }

    for entry in fs::read_dir(current)? {
        let path = entry?.path();
        if path.is_dir() {
            scan_bug_museum_recursive(root, &path, entries)?;
        }
    }

    Ok(())
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

fn metadata_value(fields: &BTreeMap<String, String>, key: &str) -> String {
    fields
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| "unspecified".to_string())
}

fn summarize(
    corpus: &[CorpusEvidence],
    fuzz: &[FuzzRunEvidence],
    artifacts: &[BugArtifactEvidence],
    bug_museum: &[BugMuseumEvidence],
) -> EvidenceSummary {
    let corpus_failures = corpus.iter().filter(|entry| !entry.passed()).count();
    let fuzz_failures = fuzz.iter().filter(|entry| !entry.report.success).count();
    let bug_museum_incomplete = bug_museum
        .iter()
        .filter(|entry| !entry.is_complete())
        .count();
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
        passed: corpus_failures == 0 && fuzz_failures == 0 && bug_museum_incomplete == 0,
        corpus_files: corpus.len(),
        corpus_failures,
        fuzz_runs: fuzz.len(),
        fuzz_cases_executed: fuzz.iter().map(|entry| entry.report.cases_executed).sum(),
        fuzz_failures,
        backend_mismatches,
        oracle_mismatches,
        trace_failures,
        historical_artifacts: artifacts.len(),
        bug_museum_entries: bug_museum.len(),
        bug_museum_incomplete,
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
            opcode_kinds: Vec::new(),
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

impl BugMuseumEvidence {
    fn is_complete(&self) -> bool {
        self.has_metadata && self.has_readme && self.has_repro_source
    }
}

fn coverage_dashboard(corpus: &[CorpusEvidence], fuzz: &[FuzzRunEvidence]) -> CoverageDashboard {
    let coverage = aggregate_fuzz_coverage(fuzz);
    let corpus_opcodes = corpus
        .iter()
        .flat_map(|entry| entry.opcode_kinds.iter().cloned())
        .collect::<BTreeSet<_>>();
    let fuzz_opcodes = coverage.opcode_kinds.clone();

    let opcode_rows = ALL_OPCODE_NAMES
        .iter()
        .map(|opcode| OpcodeCoverageRow {
            opcode: (*opcode).to_string(),
            corpus_seen: corpus_opcodes.contains(*opcode),
            fuzz_seen: fuzz_opcodes.contains(*opcode),
        })
        .collect::<Vec<_>>();
    let opcodes_seen = opcode_rows
        .iter()
        .filter(|row| row.corpus_seen || row.fuzz_seen)
        .count();

    CoverageDashboard {
        fuzz_cases: coverage.cases,
        oracle_comparisons: coverage.oracle_comparisons,
        metamorphic_variants: coverage.metamorphic_variants,
        opcodes_seen,
        opcodes_total: ALL_OPCODE_NAMES.len(),
        feature_rows: feature_coverage_rows(&coverage),
        opcode_rows,
    }
}

fn aggregate_fuzz_coverage(fuzz: &[FuzzRunEvidence]) -> FuzzCoverage {
    let mut coverage = FuzzCoverage::default();
    for run in fuzz {
        let run_coverage = &run.report.coverage;
        coverage.cases += run_coverage.cases;
        coverage.coverage_guided_cases += run_coverage.coverage_guided_cases;
        coverage.oracle_comparisons += run_coverage.oracle_comparisons;
        coverage.metamorphic_variants += run_coverage.metamorphic_variants;
        coverage.metamorphic_return_neutral += run_coverage.metamorphic_return_neutral;
        coverage.metamorphic_dead_branch += run_coverage.metamorphic_dead_branch;
        coverage.metamorphic_unused_local += run_coverage.metamorphic_unused_local;
        coverage.metamorphic_algebraic_neutral += run_coverage.metamorphic_algebraic_neutral;
        coverage.metamorphic_branch_inversion += run_coverage.metamorphic_branch_inversion;
        coverage.metamorphic_helper_wrapping += run_coverage.metamorphic_helper_wrapping;
        coverage.metamorphic_statement_reordering += run_coverage.metamorphic_statement_reordering;
        coverage.optimizer_stress_cases += run_coverage.optimizer_stress_cases;
        coverage.helper_functions += run_coverage.helper_functions;
        coverage.helper_calls += run_coverage.helper_calls;
        coverage.branches += run_coverage.branches;
        coverage.loops += run_coverage.loops;
        coverage.prints += run_coverage.prints;
        coverage.global_array_reads += run_coverage.global_array_reads;
        coverage.global_array_writes += run_coverage.global_array_writes;
        coverage.local_array_reads += run_coverage.local_array_reads;
        coverage.local_array_writes += run_coverage.local_array_writes;
        coverage.loop_indexed_array_writes += run_coverage.loop_indexed_array_writes;
        coverage.helper_array_interactions += run_coverage.helper_array_interactions;
        coverage.constant_fold_patterns += run_coverage.constant_fold_patterns;
        coverage.dead_code_shapes += run_coverage.dead_code_shapes;
        coverage
            .opcode_kinds
            .extend(run_coverage.opcode_kinds.iter().cloned());
    }
    coverage
}

fn feature_coverage_rows(coverage: &FuzzCoverage) -> Vec<FeatureCoverageRow> {
    let total = coverage.cases;
    vec![
        feature_row(
            "coverage-guided selected cases",
            coverage.coverage_guided_cases,
            total,
        ),
        feature_row(
            "optimizer stress cases",
            coverage.optimizer_stress_cases,
            total,
        ),
        feature_row("helper functions", coverage.helper_functions, total),
        feature_row("helper calls", coverage.helper_calls, total),
        feature_row("branches", coverage.branches, total),
        feature_row("loops", coverage.loops, total),
        feature_row("print statements", coverage.prints, total),
        feature_row("global array reads", coverage.global_array_reads, total),
        feature_row("global array writes", coverage.global_array_writes, total),
        feature_row("local array reads", coverage.local_array_reads, total),
        feature_row("local array writes", coverage.local_array_writes, total),
        feature_row(
            "loop-indexed array writes",
            coverage.loop_indexed_array_writes,
            total,
        ),
        feature_row(
            "helper/array interactions",
            coverage.helper_array_interactions,
            total,
        ),
        feature_row(
            "constant-fold patterns",
            coverage.constant_fold_patterns,
            total,
        ),
        feature_row("dead-code shapes", coverage.dead_code_shapes, total),
        feature_row(
            "metamorphic return-neutral variants",
            coverage.metamorphic_return_neutral,
            coverage.metamorphic_variants,
        ),
        feature_row(
            "metamorphic dead-branch variants",
            coverage.metamorphic_dead_branch,
            coverage.metamorphic_variants,
        ),
        feature_row(
            "metamorphic unused-local variants",
            coverage.metamorphic_unused_local,
            coverage.metamorphic_variants,
        ),
        feature_row(
            "metamorphic algebraic-neutral variants",
            coverage.metamorphic_algebraic_neutral,
            coverage.metamorphic_variants,
        ),
        feature_row(
            "metamorphic branch-inversion variants",
            coverage.metamorphic_branch_inversion,
            coverage.metamorphic_variants,
        ),
        feature_row(
            "metamorphic helper-wrapping variants",
            coverage.metamorphic_helper_wrapping,
            coverage.metamorphic_variants,
        ),
        feature_row(
            "metamorphic statement-reordering variants",
            coverage.metamorphic_statement_reordering,
            coverage.metamorphic_variants,
        ),
    ]
}

fn feature_row(feature: &str, cases: usize, total_cases: usize) -> FeatureCoverageRow {
    FeatureCoverageRow {
        feature: feature.to_string(),
        cases,
        total_cases,
    }
}

impl EvidenceReport {
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        out.push_str("\"schema_version\":2");
        write!(out, ",\"generated_at_unix\":{}", self.generated_at_unix)
            .expect("write to string cannot fail");
        out.push_str(",\"summary\":");
        push_summary_json(&mut out, &self.summary);
        out.push_str(",\"coverage\":");
        push_coverage_dashboard_json(&mut out, &self.coverage);
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
        out.push_str("],\"bug_museum\":[");
        for (index, entry) in self.bug_museum.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_bug_museum_json(&mut out, entry);
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
            "{}: corpus files={}, fuzz runs={}, fuzz cases executed={}, bug museum entries={}, local fuzz artifacts={}.",
            if self.summary.passed {
                "Passed"
            } else {
                "Failed"
            },
            self.summary.corpus_files,
            self.summary.fuzz_runs,
            self.summary.fuzz_cases_executed,
            self.summary.bug_museum_entries,
            self.summary.historical_artifacts
        )
        .expect("write to string cannot fail");

        out.push_str("\n## Coverage Dashboard\n\n");
        writeln!(
            out,
            "Observed coverage from checked-in corpus programs plus the deterministic evidence fuzz matrix. This is execution evidence, not a completeness proof. Opcodes observed: {}/{}. AST oracle comparisons: {}. Metamorphic variants checked: {}.",
            self.coverage.opcodes_seen,
            self.coverage.opcodes_total,
            self.coverage.oracle_comparisons,
            self.coverage.metamorphic_variants
        )
        .expect("write to string cannot fail");

        out.push_str("\n### Feature Coverage\n\n");
        out.push_str("| Feature | Cases | Share |\n");
        out.push_str("| --- | ---: | ---: |\n");
        for row in &self.coverage.feature_rows {
            writeln!(
                out,
                "| {} | {} / {} | {} |",
                row.feature,
                row.cases,
                row.total_cases,
                percent(row.cases, row.total_cases)
            )
            .expect("write to string cannot fail");
        }

        out.push_str("\n### Opcode Coverage\n\n");
        out.push_str("| Opcode | Corpus | Fuzz | Status |\n");
        out.push_str("| --- | --- | --- | --- |\n");
        for row in &self.coverage.opcode_rows {
            writeln!(
                out,
                "| {} | {} | {} | {} |",
                row.opcode,
                yes_no(row.corpus_seen),
                yes_no(row.fuzz_seen),
                if row.corpus_seen || row.fuzz_seen {
                    "observed"
                } else {
                    "missing"
                }
            )
            .expect("write to string cannot fail");
        }

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

        out.push_str("\n## Fuzz Matrix Detail\n\n");
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

        out.push_str("\n## Bug Museum\n\n");
        out.push_str("| Entry | Status | Expected Behavior | Proof Gate | Repro | Docs |\n");
        out.push_str("| --- | --- | --- | --- | --- | --- |\n");
        if self.bug_museum.is_empty() {
            out.push_str("| none | n/a | n/a | n/a | no | no |\n");
        } else {
            for entry in &self.bug_museum {
                writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} |",
                    entry.id,
                    entry.status,
                    entry.expected,
                    entry.proof_gate,
                    yes_no(entry.has_repro_source),
                    yes_no(entry.has_readme)
                )
                .expect("write to string cannot fail");
            }
        }

        out.push_str("\n## Local Fuzz Artifacts\n\n");
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
    write!(
        out,
        ",\"bug_museum_entries\":{}",
        summary.bug_museum_entries
    )
    .expect("write to string cannot fail");
    write!(
        out,
        ",\"bug_museum_incomplete\":{}",
        summary.bug_museum_incomplete
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
    out.push_str(",\"opcode_kinds\":[");
    for (index, opcode) in entry.opcode_kinds.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_json_string(out, opcode);
    }
    out.push(']');
    out.push('}');
}

fn push_coverage_dashboard_json(out: &mut String, coverage: &CoverageDashboard) {
    out.push('{');
    write!(out, "\"fuzz_cases\":{}", coverage.fuzz_cases).expect("write to string cannot fail");
    write!(
        out,
        ",\"oracle_comparisons\":{}",
        coverage.oracle_comparisons
    )
    .expect("write to string cannot fail");
    write!(
        out,
        ",\"metamorphic_variants\":{}",
        coverage.metamorphic_variants
    )
    .expect("write to string cannot fail");
    write!(out, ",\"opcodes_seen\":{}", coverage.opcodes_seen)
        .expect("write to string cannot fail");
    write!(out, ",\"opcodes_total\":{}", coverage.opcodes_total)
        .expect("write to string cannot fail");
    out.push_str(",\"features\":[");
    for (index, row) in coverage.feature_rows.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_feature_coverage_json(out, row);
    }
    out.push_str("],\"opcodes\":[");
    for (index, row) in coverage.opcode_rows.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_opcode_coverage_json(out, row);
    }
    out.push_str("]}");
}

fn push_feature_coverage_json(out: &mut String, row: &FeatureCoverageRow) {
    out.push('{');
    out.push_str("\"feature\":");
    push_json_string(out, &row.feature);
    write!(out, ",\"cases\":{}", row.cases).expect("write to string cannot fail");
    write!(out, ",\"total_cases\":{}", row.total_cases).expect("write to string cannot fail");
    out.push('}');
}

fn push_opcode_coverage_json(out: &mut String, row: &OpcodeCoverageRow) {
    out.push('{');
    out.push_str("\"opcode\":");
    push_json_string(out, &row.opcode);
    write!(out, ",\"corpus_seen\":{}", row.corpus_seen).expect("write to string cannot fail");
    write!(out, ",\"fuzz_seen\":{}", row.fuzz_seen).expect("write to string cannot fail");
    write!(out, ",\"observed\":{}", row.corpus_seen || row.fuzz_seen)
        .expect("write to string cannot fail");
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

fn push_bug_museum_json(out: &mut String, entry: &BugMuseumEvidence) {
    out.push('{');
    out.push_str("\"id\":");
    push_json_string(out, &entry.id);
    out.push_str(",\"path\":");
    push_json_string(out, &entry.path);
    out.push_str(",\"status\":");
    push_json_string(out, &entry.status);
    out.push_str(",\"expected\":");
    push_json_string(out, &entry.expected);
    out.push_str(",\"proof_gate\":");
    push_json_string(out, &entry.proof_gate);
    write!(out, ",\"has_metadata\":{}", entry.has_metadata).expect("write to string cannot fail");
    write!(out, ",\"has_readme\":{}", entry.has_readme).expect("write to string cannot fail");
    write!(out, ",\"has_repro_source\":{}", entry.has_repro_source)
        .expect("write to string cannot fail");
    out.push_str(",\"repro_source_hash\":");
    push_optional_string(out, entry.repro_source_hash.as_deref());
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

fn program_opcode_kinds(program: &CompiledProgram) -> Vec<String> {
    program
        .instructions
        .iter()
        .map(|instruction| opcode_name(instruction.opcode).to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn opcode_name(opcode: Opcode) -> &'static str {
    match opcode {
        Opcode::LoadConst => "LoadConst",
        Opcode::LoadLocal => "LoadLocal",
        Opcode::StoreLocal => "StoreLocal",
        Opcode::LoadGlobal => "LoadGlobal",
        Opcode::StoreGlobal => "StoreGlobal",
        Opcode::Add => "Add",
        Opcode::Sub => "Sub",
        Opcode::Mul => "Mul",
        Opcode::Div => "Div",
        Opcode::Neg => "Neg",
        Opcode::Eq => "Eq",
        Opcode::Ne => "Ne",
        Opcode::Lt => "Lt",
        Opcode::Gt => "Gt",
        Opcode::Le => "Le",
        Opcode::Ge => "Ge",
        Opcode::And => "And",
        Opcode::Or => "Or",
        Opcode::Not => "Not",
        Opcode::Jump => "Jump",
        Opcode::JumpIfFalse => "JumpIfFalse",
        Opcode::JumpIfTrue => "JumpIfTrue",
        Opcode::Call => "Call",
        Opcode::Return => "Return",
        Opcode::ArrayLoad => "ArrayLoad",
        Opcode::ArrayStore => "ArrayStore",
        Opcode::ArrayNew => "ArrayNew",
        Opcode::LocalArrayLoad => "LocalArrayLoad",
        Opcode::LocalArrayStore => "LocalArrayStore",
        Opcode::AllocArray => "AllocArray",
        Opcode::Print => "Print",
        Opcode::Pop => "Pop",
        Opcode::Dup => "Dup",
        Opcode::Halt => "Halt",
    }
}

fn percent(count: usize, total: usize) -> String {
    if total == 0 {
        return "n/a".to_string();
    }
    format!("{:.1}%", (count as f64 / total as f64) * 100.0)
}

fn status(value: bool) -> &'static str {
    if value {
        "passed"
    } else {
        "failed"
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
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
            bug_museum_dir: PathBuf::from("tests/bugs"),
        };

        let report = generate_evidence_report(config).expect("report should generate");
        assert!(report.summary.corpus_files >= 3);
        assert_eq!(report.summary.fuzz_runs, 1);
        assert!(report.to_json().contains("\"schema_version\":2"));
        assert!(report.to_json().contains("\"coverage\""));
        assert!(report.to_json().contains("\"bug_museum\""));
        assert!(report.to_json().contains("\"oracle_equivalent\""));
        assert!(report.to_markdown().contains("# Qydrel Evidence Report"));
        assert!(report.to_markdown().contains("## Coverage Dashboard"));
    }
}
