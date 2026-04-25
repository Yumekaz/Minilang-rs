//! Deterministic self-audit fuzzer for MiniLang.
//!
//! The generator deliberately stays in a terminating subset with initialized
//! scalars and in-bounds global/local array access. That keeps failures actionable:
//! when the verifier, backend comparator, trace replay, or trace diff reports a
//! problem, it is likely a runtime/compiler bug rather than random invalid
//! input.

use crate::compiler::disassemble;
use crate::gc_vm::GcVm;
use crate::vm::Vm;
use crate::{
    compare_backends, compile, diff_vm_gc_traces, replay_vm_trace, CompiledProgram, Verifier,
};
use std::fmt::{self, Write};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_EXPR_DEPTH: usize = 3;
const DEFAULT_MAX_STATEMENTS: usize = 14;
const DEFAULT_ARTIFACT_DIR: &str = "fuzz-artifacts";
const SHRINK_PASSES: usize = 64;

/// Fuzzer configuration.
#[derive(Debug, Clone)]
pub struct FuzzConfig {
    pub seed: u64,
    pub cases: usize,
    pub max_expr_depth: usize,
    pub max_statements: usize,
    pub artifact_dir: Option<PathBuf>,
    pub shrink: bool,
}

/// Full fuzzer run result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzReport {
    pub seed: u64,
    pub cases_requested: usize,
    pub cases_executed: usize,
    pub success: bool,
    pub coverage: FuzzCoverage,
    pub failure: Option<FuzzFailure>,
}

/// Aggregate generator feature coverage for a fuzzer run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FuzzCoverage {
    pub cases: usize,
    pub helper_functions: usize,
    pub helper_calls: usize,
    pub branches: usize,
    pub loops: usize,
    pub prints: usize,
    pub global_array_reads: usize,
    pub global_array_writes: usize,
    pub local_array_reads: usize,
    pub local_array_writes: usize,
}

/// First failing generated program, with minimized repro when shrinking is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzFailure {
    pub case_index: usize,
    pub case_seed: u64,
    pub reason: FuzzFailureReason,
    pub coverage_at_failure: FuzzCoverage,
    pub original_source: String,
    pub minimized_source: String,
    pub artifacts_dir: Option<PathBuf>,
    pub artifact_error: Option<String>,
}

/// Audit failure category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FuzzFailureReason {
    Compile(String),
    Verification(String),
    BackendComparison(String),
    TraceReplay(String),
    TraceDiff(String),
}

#[derive(Debug, Clone)]
struct HelperSig {
    name: String,
    arity: usize,
}

#[derive(Debug, Clone)]
struct ArrayBinding {
    name: String,
    size: usize,
    scope: ArrayScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArrayScope {
    Global,
    Local,
}

#[derive(Debug, Clone)]
struct ArrayAccess {
    name: String,
    index: usize,
    scope: ArrayScope,
}

#[derive(Debug, Clone, Default)]
struct FeatureSet {
    helper_functions: bool,
    helper_calls: bool,
    branches: bool,
    loops: bool,
    prints: bool,
    global_array_reads: bool,
    global_array_writes: bool,
    local_array_reads: bool,
    local_array_writes: bool,
}

struct GeneratedProgram {
    source: String,
    features: FeatureSet,
}

struct ProgramGenerator {
    rng: Rng,
    max_expr_depth: usize,
    max_statements: usize,
    helpers: Vec<HelperSig>,
    globals: Vec<String>,
    arrays: Vec<ArrayBinding>,
    local_arrays: Vec<ArrayBinding>,
    locals: Vec<String>,
    features: FeatureSet,
    next_var: usize,
}

struct FailureArtifactInput<'a> {
    base_dir: &'a Path,
    run_seed: u64,
    case_index: usize,
    case_seed: u64,
    reason: &'a FuzzFailureReason,
    original_source: &'a str,
    minimized_source: &'a str,
    coverage: &'a FuzzCoverage,
    case_features: &'a FeatureSet,
}

#[derive(Debug, Clone)]
struct Rng {
    state: u64,
}

/// Run deterministic MiniLang audit fuzzing.
pub fn run_fuzzer(config: FuzzConfig) -> FuzzReport {
    let mut rng = Rng::new(config.seed);
    let mut coverage = FuzzCoverage::default();

    for case_index in 0..config.cases {
        let case_seed = rng.next_u64();
        let generated =
            ProgramGenerator::new(case_seed, config.max_expr_depth, config.max_statements)
                .generate();
        coverage.observe(&generated.features);

        if let Some(reason) = audit_source(&generated.source) {
            let minimized_source = if config.shrink {
                shrink_source(&generated.source, reason.tag())
            } else {
                generated.source.clone()
            };
            let coverage_at_failure = coverage.clone();

            let (artifacts_dir, artifact_error) = match &config.artifact_dir {
                Some(base_dir) => match write_failure_artifacts(FailureArtifactInput {
                    base_dir,
                    run_seed: config.seed,
                    case_index,
                    case_seed,
                    reason: &reason,
                    original_source: &generated.source,
                    minimized_source: &minimized_source,
                    coverage: &coverage_at_failure,
                    case_features: &generated.features,
                }) {
                    Ok(path) => (Some(path), None),
                    Err(err) => (None, Some(err.to_string())),
                },
                None => (None, None),
            };

            return FuzzReport {
                seed: config.seed,
                cases_requested: config.cases,
                cases_executed: case_index + 1,
                success: false,
                coverage,
                failure: Some(FuzzFailure {
                    case_index,
                    case_seed,
                    reason,
                    coverage_at_failure,
                    original_source: generated.source,
                    minimized_source,
                    artifacts_dir,
                    artifact_error,
                }),
            };
        }
    }

    FuzzReport {
        seed: config.seed,
        cases_requested: config.cases,
        cases_executed: config.cases,
        success: true,
        coverage,
        failure: None,
    }
}

impl Default for FuzzConfig {
    fn default() -> Self {
        Self {
            seed: 0xC0DE_CAFE_D15E_A5E5,
            cases: 100,
            max_expr_depth: DEFAULT_MAX_EXPR_DEPTH,
            max_statements: DEFAULT_MAX_STATEMENTS,
            artifact_dir: Some(PathBuf::from(DEFAULT_ARTIFACT_DIR)),
            shrink: true,
        }
    }
}

impl FuzzFailureReason {
    fn tag(&self) -> &'static str {
        match self {
            FuzzFailureReason::Compile(_) => "compile",
            FuzzFailureReason::Verification(_) => "verification",
            FuzzFailureReason::BackendComparison(_) => "backend-comparison",
            FuzzFailureReason::TraceReplay(_) => "trace-replay",
            FuzzFailureReason::TraceDiff(_) => "trace-diff",
        }
    }
}

impl FuzzCoverage {
    fn observe(&mut self, features: &FeatureSet) {
        self.cases += 1;
        self.helper_functions += usize::from(features.helper_functions);
        self.helper_calls += usize::from(features.helper_calls);
        self.branches += usize::from(features.branches);
        self.loops += usize::from(features.loops);
        self.prints += usize::from(features.prints);
        self.global_array_reads += usize::from(features.global_array_reads);
        self.global_array_writes += usize::from(features.global_array_writes);
        self.local_array_reads += usize::from(features.local_array_reads);
        self.local_array_writes += usize::from(features.local_array_writes);
    }
}

impl FeatureSet {
    fn record_array_read(&mut self, scope: ArrayScope) {
        match scope {
            ArrayScope::Global => self.global_array_reads = true,
            ArrayScope::Local => self.local_array_reads = true,
        }
    }

    fn record_array_write(&mut self, scope: ArrayScope) {
        match scope {
            ArrayScope::Global => self.global_array_writes = true,
            ArrayScope::Local => self.local_array_writes = true,
        }
    }
}

fn audit_source(source: &str) -> Option<FuzzFailureReason> {
    let compiled = match compile(source) {
        Ok(compiled) => compiled,
        Err(err) => return Some(FuzzFailureReason::Compile(err)),
    };

    let verification = Verifier::new().verify(&compiled);
    if !verification.valid {
        return Some(FuzzFailureReason::Verification(verification.to_string()));
    }

    let comparison = compare_backends(&compiled);
    if !comparison.equivalent {
        return Some(FuzzFailureReason::BackendComparison(comparison.to_string()));
    }

    let replay = replay_vm_trace(&compiled);
    if !replay.replayable {
        return Some(FuzzFailureReason::TraceReplay(replay.to_string()));
    }

    let trace_diff = diff_vm_gc_traces(&compiled);
    if !trace_diff.equivalent {
        return Some(FuzzFailureReason::TraceDiff(trace_diff.to_string()));
    }

    None
}

fn shrink_source(source: &str, reason_tag: &str) -> String {
    let mut current = source.to_string();

    for _ in 0..SHRINK_PASSES {
        let Some(candidate) = find_line_removal_shrink(&current, reason_tag) else {
            break;
        };
        current = candidate;
    }

    current
}

fn find_line_removal_shrink(source: &str, reason_tag: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();

    for index in 0..lines.len() {
        if !is_removable_line(lines[index]) {
            continue;
        }

        let candidate = lines
            .iter()
            .enumerate()
            .filter_map(|(line_index, line)| (line_index != index).then_some(*line))
            .collect::<Vec<_>>()
            .join("\n");
        let candidate = format!("{}\n", candidate);

        if has_same_failure(&candidate, reason_tag) {
            return Some(candidate);
        }
    }

    None
}

fn is_removable_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.ends_with(';') && !trimmed.starts_with("return ")
}

fn has_same_failure(source: &str, reason_tag: &str) -> bool {
    audit_source(source)
        .map(|reason| reason.tag() == reason_tag)
        .unwrap_or(false)
}

fn write_failure_artifacts(input: FailureArtifactInput<'_>) -> io::Result<PathBuf> {
    let case_dir = input.base_dir.join(format!(
        "seed_{:016x}_case_{:04}_case_seed_{:016x}",
        input.run_seed, input.case_index, input.case_seed
    ));
    fs::create_dir_all(&case_dir)?;

    fs::write(case_dir.join("original.lang"), input.original_source)?;
    fs::write(case_dir.join("minimized.lang"), input.minimized_source)?;
    fs::write(case_dir.join("failure.txt"), input.reason.to_string())?;
    fs::write(case_dir.join("manifest.txt"), failure_manifest(&input))?;

    if let Ok(compiled) = compile(input.minimized_source) {
        fs::write(case_dir.join("bytecode.txt"), disassemble(&compiled))?;
        write_trace_artifacts(&case_dir, &compiled)?;
    }

    Ok(case_dir)
}

fn failure_manifest(input: &FailureArtifactInput<'_>) -> String {
    format!(
        "MiniLang Fuzz Failure Manifest\n\
         run_seed: {:#018x}\n\
         case_index: {}\n\
         case_seed: {:#018x}\n\
         reason: {}\n\
         original_source_hash: {:016x}\n\
         minimized_source_hash: {:016x}\n\
         repro_command: minilang --fuzz {} --fuzz-seed {:#018x}\n\
         case_features: {}\n\
         run_coverage_at_failure:\n{}",
        input.run_seed,
        input.case_index,
        input.case_seed,
        input.reason.tag(),
        stable_hash(input.original_source),
        stable_hash(input.minimized_source),
        input.case_index + 1,
        input.run_seed,
        format_feature_set(input.case_features),
        input.coverage
    )
}

fn write_trace_artifacts(case_dir: &Path, compiled: &CompiledProgram) -> io::Result<()> {
    let mut vm = Vm::new(compiled).with_trace();
    let _ = vm.run();
    fs::write(case_dir.join("vm.trace.json"), vm.trace_json())?;

    let mut gc_vm = GcVm::new(compiled).with_trace();
    let _ = gc_vm.run();
    fs::write(case_dir.join("gc_vm.trace.json"), gc_vm.trace_json())?;

    Ok(())
}

impl ProgramGenerator {
    fn new(seed: u64, max_expr_depth: usize, max_statements: usize) -> Self {
        Self {
            rng: Rng::new(seed),
            max_expr_depth,
            max_statements,
            helpers: Vec::new(),
            globals: Vec::new(),
            arrays: Vec::new(),
            local_arrays: Vec::new(),
            locals: Vec::new(),
            features: FeatureSet::default(),
            next_var: 0,
        }
    }

    fn generate(mut self) -> GeneratedProgram {
        let mut source = String::new();
        self.generate_globals(&mut source);
        self.generate_helpers(&mut source);
        self.generate_main(&mut source);
        GeneratedProgram {
            source,
            features: self.features,
        }
    }

    fn generate_globals(&mut self, source: &mut String) {
        let count = self.rng.usize(3);
        for index in 0..count {
            let name = format!("g{}", index);
            self.globals.push(name.clone());
            source.push_str(&format!("int {} = {};\n", name, self.small_literal()));
        }

        let array_count = 1 + self.rng.usize(3);
        for index in 0..array_count {
            let name = format!("ga{}", index);
            let size = 2 + self.rng.usize(5);
            self.arrays.push(ArrayBinding {
                name: name.clone(),
                size,
                scope: ArrayScope::Global,
            });
            source.push_str(&format!("int {}[{}];\n", name, size));
        }

        if count > 0 || array_count > 0 {
            source.push('\n');
        }
    }

    fn generate_helpers(&mut self, source: &mut String) {
        let count = self.rng.usize(3);
        if count > 0 {
            self.features.helper_functions = true;
            self.features.branches = true;
        }
        for index in 0..count {
            let arity = 1 + self.rng.usize(2);
            let name = format!("f{}", index);

            source.push_str(&format!("func {}(", name));
            for param in 0..arity {
                if param > 0 {
                    source.push_str(", ");
                }
                source.push_str(&format!("int p{}", param));
            }
            source.push_str(") {\n");

            let params = (0..arity)
                .map(|param| format!("p{}", param))
                .collect::<Vec<_>>();
            let mut vars = params.clone();
            let arrays = self.arrays.clone();
            let base_expr = self.int_expr_from(&vars, &[], &arrays, self.max_expr_depth);
            source.push_str(&format!("  int h{} = {};\n", index, base_expr));
            vars.push(format!("h{}", index));

            let condition = self.condition_from(&vars, &[], &arrays);
            let then_expr = self.int_expr_from(&vars, &[], &arrays, self.max_expr_depth);
            let else_expr = self.int_expr_from(&vars, &[], &arrays, self.max_expr_depth);
            source.push_str(&format!("  if ({}) {{\n", condition));
            source.push_str(&format!("    h{} = {};\n", index, then_expr));
            source.push_str("  } else {\n");
            source.push_str(&format!("    h{} = {};\n", index, else_expr));
            source.push_str("  }\n");
            source.push_str(&format!("  return h{};\n", index));
            source.push_str("}\n\n");

            self.helpers.push(HelperSig { name, arity });
        }
    }

    fn generate_main(&mut self, source: &mut String) {
        self.locals.clear();
        self.local_arrays.clear();
        self.next_var = 0;

        source.push_str("func main() {\n");
        source.push_str(&format!("  int acc = {};\n", self.small_literal()));
        self.locals.push("acc".to_string());
        self.generate_local_arrays(source);
        self.generate_local_array_smoke(source);

        let statement_count = if self.max_statements <= 4 {
            self.max_statements
        } else {
            4 + self.rng.usize(self.max_statements - 3)
        };
        for _ in 0..statement_count {
            self.generate_main_statement(source);
        }

        let return_expr = self.int_expr();
        source.push_str(&format!("  return {};\n", return_expr));
        source.push_str("}\n");
    }

    fn generate_local_arrays(&mut self, source: &mut String) {
        let count = 1 + self.rng.usize(2);
        for index in 0..count {
            let name = format!("la{}", index);
            let size = 2 + self.rng.usize(5);
            self.local_arrays.push(ArrayBinding {
                name: name.clone(),
                size,
                scope: ArrayScope::Local,
            });
            source.push_str(&format!("  int {}[{}];\n", name, size));
        }
    }

    fn generate_local_array_smoke(&mut self, source: &mut String) {
        if let Some(array) = self.local_arrays.first() {
            let name = array.name.clone();
            let scope = array.scope;
            self.features.record_array_write(scope);
            self.features.record_array_read(scope);
            source.push_str(&format!("  {}[0] = acc;\n", name));
            source.push_str(&format!("  acc = (acc + {}[0]);\n", name));
        }
    }

    fn generate_main_statement(&mut self, source: &mut String) {
        match self.rng.usize(8) {
            0 => self.generate_local_decl(source),
            1 => self.generate_assignment(source),
            2 => self.generate_acc_update(source),
            3 => self.generate_if(source),
            4 => self.generate_bounded_loop(source),
            5 => self.generate_array_store(source),
            6 => self.generate_array_acc_update(source),
            _ => self.generate_print(source),
        }
    }

    fn generate_local_decl(&mut self, source: &mut String) {
        let name = self.fresh_local();
        let expr = self.int_expr();
        source.push_str(&format!("  int {} = {};\n", name, expr));
        self.locals.push(name);
    }

    fn generate_assignment(&mut self, source: &mut String) {
        let target = self.pick_local();
        let expr = self.int_expr();
        source.push_str(&format!("  {} = {};\n", target, expr));
    }

    fn generate_acc_update(&mut self, source: &mut String) {
        let op = ["+", "-", "*"][self.rng.usize(3)];
        let expr = self.int_expr();
        source.push_str(&format!("  acc = (acc {} {});\n", op, expr));
    }

    fn generate_if(&mut self, source: &mut String) {
        self.features.branches = true;
        let condition = self.condition();
        let then_expr = self.int_expr();
        let else_expr = self.int_expr();
        source.push_str(&format!("  if ({}) {{\n", condition));
        source.push_str(&format!("    acc = (acc + {});\n", then_expr));
        source.push_str("  } else {\n");
        source.push_str(&format!("    acc = (acc - {});\n", else_expr));
        source.push_str("  }\n");
    }

    fn generate_bounded_loop(&mut self, source: &mut String) {
        self.features.loops = true;
        let index_name = self.fresh_local();
        let limit = 1 + self.rng.usize(5);
        source.push_str(&format!("  int {} = 0;\n", index_name));
        self.locals.push(index_name.clone());

        let expr = self.int_expr();
        source.push_str(&format!("  while ({} < {}) {{\n", index_name, limit));
        source.push_str(&format!("    acc = (acc + {});\n", expr));
        source.push_str(&format!("    {} = ({} + 1);\n", index_name, index_name));
        source.push_str("  }\n");
    }

    fn generate_print(&mut self, source: &mut String) {
        self.features.prints = true;
        let expr = self.int_expr();
        source.push_str(&format!("  print {};\n", expr));
    }

    fn generate_array_store(&mut self, source: &mut String) {
        let Some(access) = self.array_access() else {
            self.generate_assignment(source);
            return;
        };
        self.features.record_array_write(access.scope);
        let expr = self.int_expr();
        source.push_str(&format!(
            "  {}[{}] = {};\n",
            access.name, access.index, expr
        ));
    }

    fn generate_array_acc_update(&mut self, source: &mut String) {
        let Some(read) = self.array_read_expr() else {
            self.generate_acc_update(source);
            return;
        };
        source.push_str(&format!("  acc = (acc + {});\n", read));
    }

    fn fresh_local(&mut self) -> String {
        let name = format!("v{}", self.next_var);
        self.next_var += 1;
        name
    }

    fn pick_local(&mut self) -> String {
        let index = self.rng.usize(self.locals.len());
        self.locals[index].clone()
    }

    fn int_expr(&mut self) -> String {
        let locals = self.locals.clone();
        let globals = self.globals.clone();
        let arrays = self.arrays_in_scope();
        self.int_expr_from(&locals, &globals, &arrays, self.max_expr_depth)
    }

    fn condition(&mut self) -> String {
        let locals = self.locals.clone();
        let globals = self.globals.clone();
        let arrays = self.arrays_in_scope();
        self.condition_from(&locals, &globals, &arrays)
    }

    fn arrays_in_scope(&self) -> Vec<ArrayBinding> {
        let mut arrays = self.arrays.clone();
        arrays.extend(self.local_arrays.clone());
        arrays
    }

    fn condition_from(
        &mut self,
        vars: &[String],
        globals: &[String],
        arrays: &[ArrayBinding],
    ) -> String {
        let left = self.int_expr_from(vars, globals, arrays, 1);
        let right = self.int_expr_from(vars, globals, arrays, 1);
        let op = ["==", "!=", "<", ">", "<=", ">="][self.rng.usize(6)];
        format!("({} {} {})", left, op, right)
    }

    fn int_expr_from(
        &mut self,
        vars: &[String],
        globals: &[String],
        arrays: &[ArrayBinding],
        depth: usize,
    ) -> String {
        if depth == 0 {
            return self.leaf_expr(vars, globals, arrays);
        }

        match self.rng.usize(8) {
            0 => self.leaf_expr(vars, globals, arrays),
            1 => format!(
                "(-{})",
                self.int_expr_from(vars, globals, arrays, depth - 1)
            ),
            2 => self.binary_expr(vars, globals, arrays, depth, "+"),
            3 => self.binary_expr(vars, globals, arrays, depth, "-"),
            4 => self.binary_expr(vars, globals, arrays, depth, "*"),
            5 => format!(
                "({} / {})",
                self.int_expr_from(vars, globals, arrays, depth - 1),
                self.nonzero_literal()
            ),
            6 if !self.helpers.is_empty() => self.call_expr(vars, globals, arrays, depth),
            _ => self.leaf_expr(vars, globals, arrays),
        }
    }

    fn binary_expr(
        &mut self,
        vars: &[String],
        globals: &[String],
        arrays: &[ArrayBinding],
        depth: usize,
        op: &str,
    ) -> String {
        let left = self.int_expr_from(vars, globals, arrays, depth - 1);
        let right = self.int_expr_from(vars, globals, arrays, depth - 1);
        format!("({} {} {})", left, op, right)
    }

    fn call_expr(
        &mut self,
        vars: &[String],
        globals: &[String],
        arrays: &[ArrayBinding],
        depth: usize,
    ) -> String {
        self.features.helper_calls = true;
        let helper_index = self.rng.usize(self.helpers.len());
        let helper = self.helpers[helper_index].clone();
        let args = (0..helper.arity)
            .map(|_| self.int_expr_from(vars, globals, arrays, depth - 1))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({})", helper.name, args)
    }

    fn leaf_expr(
        &mut self,
        vars: &[String],
        globals: &[String],
        arrays: &[ArrayBinding],
    ) -> String {
        if !arrays.is_empty() && self.rng.chance(1, 4) {
            let index = self.rng.usize(arrays.len());
            let array = &arrays[index];
            self.features.record_array_read(array.scope);
            let element = self.rng.usize(array.size);
            return format!("{}[{}]", array.name, element);
        }

        let total_names = vars.len() + globals.len();
        if total_names > 0 && self.rng.chance(3, 5) {
            let index = self.rng.usize(total_names);
            if index < vars.len() {
                vars[index].clone()
            } else {
                globals[index - vars.len()].clone()
            }
        } else {
            self.small_literal().to_string()
        }
    }

    fn array_read_expr(&mut self) -> Option<String> {
        self.array_access().map(|access| {
            self.features.record_array_read(access.scope);
            format!("{}[{}]", access.name, access.index)
        })
    }

    fn array_access(&mut self) -> Option<ArrayAccess> {
        let arrays = self.arrays_in_scope();
        if arrays.is_empty() {
            return None;
        }

        let array_index = self.rng.usize(arrays.len());
        let array = &arrays[array_index];
        let element_index = self.rng.usize(array.size);
        Some(ArrayAccess {
            name: array.name.clone(),
            index: element_index,
            scope: array.scope,
        })
    }

    fn small_literal(&mut self) -> i32 {
        self.rng.i32_between(-16, 16)
    }

    fn nonzero_literal(&mut self) -> i32 {
        let value = 1 + self.rng.usize(9) as i32;
        if self.rng.chance(1, 2) {
            value
        } else {
            -value
        }
    }
}

impl Rng {
    fn new(seed: u64) -> Self {
        let state = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn usize(&mut self, upper: usize) -> usize {
        if upper == 0 {
            0
        } else {
            (self.next_u64() as usize) % upper
        }
    }

    fn i32_between(&mut self, min: i32, max: i32) -> i32 {
        let width = (max - min + 1) as usize;
        min + self.usize(width) as i32
    }

    fn chance(&mut self, numerator: usize, denominator: usize) -> bool {
        self.usize(denominator) < numerator
    }
}

impl fmt::Display for FuzzReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "MiniLang Fuzz Audit")?;
        writeln!(f, "  seed: {:#018x}", self.seed)?;
        writeln!(f, "  requested: {}", self.cases_requested)?;
        writeln!(f, "  executed: {}", self.cases_executed)?;
        writeln!(
            f,
            "  status: {}",
            if self.success { "passed" } else { "failed" }
        )?;
        writeln!(f)?;
        writeln!(f, "Coverage")?;
        write!(f, "{}", self.coverage)?;

        if let Some(failure) = &self.failure {
            writeln!(f)?;
            writeln!(f, "Failure")?;
            writeln!(f, "  case: {}", failure.case_index)?;
            writeln!(f, "  case seed: {:#018x}", failure.case_seed)?;
            writeln!(f, "  reason: {}", failure.reason)?;
            if let Some(path) = &failure.artifacts_dir {
                writeln!(f, "  artifacts: {}", path.display())?;
            }
            if let Some(error) = &failure.artifact_error {
                writeln!(f, "  artifact error: {}", error)?;
            }
        }

        Ok(())
    }
}

impl FuzzReport {
    /// Serialize the run summary as stable JSON for CI artifacts and demos.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        write!(out, "\"seed\":\"{:#018x}\"", self.seed).expect("write to string cannot fail");
        write!(out, ",\"cases_requested\":{}", self.cases_requested)
            .expect("write to string cannot fail");
        write!(out, ",\"cases_executed\":{}", self.cases_executed)
            .expect("write to string cannot fail");
        write!(out, ",\"success\":{}", self.success).expect("write to string cannot fail");
        out.push_str(",\"coverage\":");
        push_coverage_json(&mut out, &self.coverage);
        out.push_str(",\"failure\":");
        match &self.failure {
            Some(failure) => push_failure_json(&mut out, failure),
            None => out.push_str("null"),
        }
        out.push('}');
        out
    }
}

impl fmt::Display for FuzzCoverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  cases: {}", self.cases)?;
        writeln!(
            f,
            "  cases with helper functions: {}",
            self.helper_functions
        )?;
        writeln!(f, "  cases with helper calls: {}", self.helper_calls)?;
        writeln!(f, "  cases with branches: {}", self.branches)?;
        writeln!(f, "  cases with loops: {}", self.loops)?;
        writeln!(f, "  cases with prints: {}", self.prints)?;
        writeln!(
            f,
            "  cases with global array reads: {}",
            self.global_array_reads
        )?;
        writeln!(
            f,
            "  cases with global array writes: {}",
            self.global_array_writes
        )?;
        writeln!(
            f,
            "  cases with local array reads: {}",
            self.local_array_reads
        )?;
        writeln!(
            f,
            "  cases with local array writes: {}",
            self.local_array_writes
        )
    }
}

impl fmt::Display for FuzzFailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FuzzFailureReason::Compile(msg) => write!(f, "compile failure: {}", msg),
            FuzzFailureReason::Verification(msg) => write!(f, "verification failure:\n{}", msg),
            FuzzFailureReason::BackendComparison(msg) => {
                write!(f, "backend comparison failure:\n{}", msg)
            }
            FuzzFailureReason::TraceReplay(msg) => write!(f, "trace replay failure:\n{}", msg),
            FuzzFailureReason::TraceDiff(msg) => write!(f, "trace diff failure:\n{}", msg),
        }
    }
}

fn push_coverage_json(out: &mut String, coverage: &FuzzCoverage) {
    out.push('{');
    write!(out, "\"cases\":{}", coverage.cases).expect("write to string cannot fail");
    write!(out, ",\"helper_functions\":{}", coverage.helper_functions)
        .expect("write to string cannot fail");
    write!(out, ",\"helper_calls\":{}", coverage.helper_calls)
        .expect("write to string cannot fail");
    write!(out, ",\"branches\":{}", coverage.branches).expect("write to string cannot fail");
    write!(out, ",\"loops\":{}", coverage.loops).expect("write to string cannot fail");
    write!(out, ",\"prints\":{}", coverage.prints).expect("write to string cannot fail");
    write!(
        out,
        ",\"global_array_reads\":{}",
        coverage.global_array_reads
    )
    .expect("write to string cannot fail");
    write!(
        out,
        ",\"global_array_writes\":{}",
        coverage.global_array_writes
    )
    .expect("write to string cannot fail");
    write!(out, ",\"local_array_reads\":{}", coverage.local_array_reads)
        .expect("write to string cannot fail");
    write!(
        out,
        ",\"local_array_writes\":{}",
        coverage.local_array_writes
    )
    .expect("write to string cannot fail");
    out.push('}');
}

fn push_failure_json(out: &mut String, failure: &FuzzFailure) {
    out.push('{');
    write!(out, "\"case_index\":{}", failure.case_index).expect("write to string cannot fail");
    write!(out, ",\"case_seed\":\"{:#018x}\"", failure.case_seed)
        .expect("write to string cannot fail");
    out.push_str(",\"reason_tag\":");
    push_json_string(out, failure.reason.tag());
    out.push_str(",\"reason\":");
    push_json_string(out, &failure.reason.to_string());
    out.push_str(",\"coverage_at_failure\":");
    push_coverage_json(out, &failure.coverage_at_failure);
    out.push_str(",\"original_source_hash\":");
    push_json_string(
        out,
        &format!("{:016x}", stable_hash(&failure.original_source)),
    );
    out.push_str(",\"minimized_source_hash\":");
    push_json_string(
        out,
        &format!("{:016x}", stable_hash(&failure.minimized_source)),
    );
    out.push_str(",\"artifacts_dir\":");
    match &failure.artifacts_dir {
        Some(path) => push_json_string(out, &path.display().to_string()),
        None => out.push_str("null"),
    }
    out.push_str(",\"artifact_error\":");
    match &failure.artifact_error {
        Some(error) => push_json_string(out, error),
        None => out.push_str("null"),
    }
    out.push('}');
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                write!(out, "\\u{:04x}", ch as u32).expect("write to string cannot fail");
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

fn stable_hash(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn format_feature_set(features: &FeatureSet) -> String {
    let mut names = Vec::new();
    if features.helper_functions {
        names.push("helper-functions");
    }
    if features.helper_calls {
        names.push("helper-calls");
    }
    if features.branches {
        names.push("branches");
    }
    if features.loops {
        names.push("loops");
    }
    if features.prints {
        names.push("prints");
    }
    if features.global_array_reads {
        names.push("global-array-reads");
    }
    if features.global_array_writes {
        names.push("global-array-writes");
    }
    if features.local_array_reads {
        names.push("local-array-reads");
    }
    if features.local_array_writes {
        names.push("local-array-writes");
    }

    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_programs_pass_audit_pipeline() {
        let config = FuzzConfig {
            seed: 1234,
            cases: 12,
            artifact_dir: None,
            ..FuzzConfig::default()
        };

        let report = run_fuzzer(config);
        assert!(report.success, "{report:#?}");
        assert_eq!(report.cases_executed, 12);
        assert_eq!(report.coverage.cases, 12);
        assert_eq!(report.coverage.local_array_reads, 12);
        assert_eq!(report.coverage.local_array_writes, 12);
        assert!(report.to_json().contains("\"success\":true"));
        assert!(report.to_json().contains("\"coverage\""));
    }

    #[test]
    fn generated_program_contains_global_and_local_arrays() {
        let generated =
            ProgramGenerator::new(4321, DEFAULT_MAX_EXPR_DEPTH, DEFAULT_MAX_STATEMENTS).generate();

        assert!(generated.source.contains("int ga0["));
        assert!(generated.source.contains("int la0["));
        assert!(generated.source.contains("la0[0] = acc;"));
        assert!(generated.source.contains("acc = (acc + la0[0]);"));
        assert!(generated.features.local_array_reads);
        assert!(generated.features.local_array_writes);
        assert!(
            audit_source(&generated.source).is_none(),
            "{}",
            generated.source
        );
    }

    #[test]
    fn line_shrinker_preserves_matching_failure_kind() {
        let source = concat!(
            "func main() {\n",
            "  int x = 1;\n",
            "  print x;\n",
            "  return missing;\n",
            "}\n"
        );

        let shrunk = shrink_source(source, "compile");
        assert!(!shrunk.contains("print x;"));
        assert!(has_same_failure(&shrunk, "compile"));
    }

    #[test]
    fn failure_manifest_includes_repro_and_hashes() {
        let mut coverage = FuzzCoverage::default();
        let mut features = FeatureSet::default();
        features.record_array_read(ArrayScope::Local);
        features.record_array_write(ArrayScope::Local);
        coverage.observe(&features);

        let reason = FuzzFailureReason::TraceDiff("different stack".to_string());
        let manifest = failure_manifest(&FailureArtifactInput {
            base_dir: Path::new("unused"),
            run_seed: 0x5eed,
            case_index: 3,
            case_seed: 0x1234,
            reason: &reason,
            original_source: "func main() { return 1; }\n",
            minimized_source: "func main() { return 1; }\n",
            coverage: &coverage,
            case_features: &features,
        });

        assert!(manifest.contains("case_index: 3"));
        assert!(manifest.contains("reason: trace-diff"));
        assert!(
            manifest.contains("repro_command: minilang --fuzz 4 --fuzz-seed 0x0000000000005eed")
        );
        assert!(manifest.contains("local-array-reads"));
        assert!(manifest.contains("original_source_hash:"));
    }
}
