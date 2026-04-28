//! Deterministic self-audit fuzzer for MiniLang.
//!
//! The generator deliberately stays in a terminating subset with initialized
//! scalars and in-bounds global/local array access. That keeps failures actionable:
//! when the verifier, backend comparator, trace replay, or trace diff reports a
//! problem, it is likely a runtime/compiler bug rather than random invalid
//! input.

use crate::ast::{BinaryOp, Expr, Function, GlobalVar, Param, Program, Stmt, Type, UnaryOp};
use crate::compiler::{disassemble, Compiler, Opcode};
use crate::gc_vm::GcVm;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::token::Span;
use crate::vm::Vm;
use crate::{
    compare_ast_oracle, compile, diff_vm_gc_traces, replay_vm_trace, run_ast_oracle,
    CompiledProgram, OracleOutcome, SemanticAnalyzer, Verifier,
};
use std::collections::BTreeSet;
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
    pub corpus_dir: Option<PathBuf>,
    pub shrink: bool,
    pub mode: FuzzMode,
    pub coverage_guided: bool,
}

/// Program family used by the deterministic generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuzzMode {
    General,
    OptimizerStress,
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
    pub coverage_guided_cases: usize,
    pub oracle_comparisons: usize,
    pub metamorphic_variants: usize,
    pub optimizer_stress_cases: usize,
    pub helper_functions: usize,
    pub helper_calls: usize,
    pub branches: usize,
    pub loops: usize,
    pub prints: usize,
    pub global_array_reads: usize,
    pub global_array_writes: usize,
    pub local_array_reads: usize,
    pub local_array_writes: usize,
    pub loop_indexed_array_writes: usize,
    pub helper_array_interactions: usize,
    pub constant_fold_patterns: usize,
    pub dead_code_shapes: usize,
    pub opcode_kinds: BTreeSet<String>,
}

/// First failing generated program, with minimized repro when shrinking is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzFailure {
    pub case_index: usize,
    pub case_seed: u64,
    pub reason: FuzzFailureReason,
    pub failure_fingerprint: u64,
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
    AstOracle(String),
    BackendComparison(String),
    TraceReplay(String),
    TraceDiff(String),
    Metamorphic(String),
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
    optimizer_stress: bool,
    branches: bool,
    loops: bool,
    prints: bool,
    global_array_reads: bool,
    global_array_writes: bool,
    local_array_reads: bool,
    local_array_writes: bool,
    loop_indexed_array_writes: bool,
    helper_array_interactions: bool,
    constant_fold_patterns: bool,
    dead_code_shapes: bool,
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
    failure_fingerprint: u64,
    original_source: &'a str,
    minimized_source: &'a str,
    coverage: &'a FuzzCoverage,
    case_features: &'a FeatureSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FailureSignature {
    tag: &'static str,
    fingerprint: u64,
}

#[derive(Debug, Default)]
struct GuidanceState {
    features: BTreeSet<&'static str>,
    opcodes: BTreeSet<&'static str>,
}

#[derive(Debug, Clone)]
struct Rng {
    state: u64,
}

/// Run deterministic MiniLang audit fuzzing.
pub fn run_fuzzer(config: FuzzConfig) -> FuzzReport {
    let mut rng = Rng::new(config.seed);
    let mut coverage = FuzzCoverage::default();
    let mut guidance = GuidanceState::default();

    for case_index in 0..config.cases {
        let case_seed = rng.next_u64();
        let generated = if config.coverage_guided {
            generate_guided_program(
                config.mode,
                case_seed,
                config.max_expr_depth,
                config.max_statements,
                &guidance,
            )
        } else {
            generate_program_with_mode(
                config.mode,
                case_seed,
                config.max_expr_depth,
                config.max_statements,
            )
        };
        coverage.observe(&generated.features);
        if config.coverage_guided {
            coverage.coverage_guided_cases += 1;
        }
        let opcode_names = compiled_opcode_names(&generated.source);
        coverage.observe_opcodes(&opcode_names);
        let metamorphic_variant_count = metamorphic_variants(&generated.source).len();
        coverage.metamorphic_variants += metamorphic_variant_count;
        coverage.oracle_comparisons += 1 + metamorphic_variant_count;
        guidance.observe(&generated.features, &opcode_names);

        if let Some(reason) = audit_source(&generated.source) {
            let signature = FailureSignature::from_reason(&reason);
            let minimized_source = if config.shrink {
                shrink_source(&generated.source, signature)
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
                    failure_fingerprint: signature.fingerprint,
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
            let artifact_error = match (&config.corpus_dir, artifact_error) {
                (Some(corpus_dir), None) => write_corpus_repro(
                    corpus_dir,
                    config.seed,
                    case_index,
                    case_seed,
                    reason.tag(),
                    &minimized_source,
                )
                .err()
                .map(|err| err.to_string()),
                (_, artifact_error) => artifact_error,
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
                    failure_fingerprint: signature.fingerprint,
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
            corpus_dir: None,
            shrink: true,
            mode: FuzzMode::General,
            coverage_guided: true,
        }
    }
}

impl FuzzFailureReason {
    pub fn reason_tag(&self) -> &'static str {
        self.tag()
    }

    pub fn stable_fingerprint(&self) -> u64 {
        self.fingerprint()
    }

    fn tag(&self) -> &'static str {
        match self {
            FuzzFailureReason::Compile(_) => "compile",
            FuzzFailureReason::Verification(_) => "verification",
            FuzzFailureReason::AstOracle(_) => "ast-oracle",
            FuzzFailureReason::BackendComparison(_) => "backend-comparison",
            FuzzFailureReason::TraceReplay(_) => "trace-replay",
            FuzzFailureReason::TraceDiff(_) => "trace-diff",
            FuzzFailureReason::Metamorphic(_) => "metamorphic",
        }
    }

    fn fingerprint(&self) -> u64 {
        stable_hash(&self.to_string())
    }
}

impl FuzzMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FuzzMode::General => "general",
            FuzzMode::OptimizerStress => "optimizer-stress",
        }
    }
}

impl FuzzCoverage {
    fn observe(&mut self, features: &FeatureSet) {
        self.cases += 1;
        self.optimizer_stress_cases += usize::from(features.optimizer_stress);
        self.helper_functions += usize::from(features.helper_functions);
        self.helper_calls += usize::from(features.helper_calls);
        self.branches += usize::from(features.branches);
        self.loops += usize::from(features.loops);
        self.prints += usize::from(features.prints);
        self.global_array_reads += usize::from(features.global_array_reads);
        self.global_array_writes += usize::from(features.global_array_writes);
        self.local_array_reads += usize::from(features.local_array_reads);
        self.local_array_writes += usize::from(features.local_array_writes);
        self.loop_indexed_array_writes += usize::from(features.loop_indexed_array_writes);
        self.helper_array_interactions += usize::from(features.helper_array_interactions);
        self.constant_fold_patterns += usize::from(features.constant_fold_patterns);
        self.dead_code_shapes += usize::from(features.dead_code_shapes);
    }

    fn observe_opcodes(&mut self, opcode_names: &BTreeSet<&'static str>) {
        for opcode in opcode_names {
            self.opcode_kinds.insert((*opcode).to_string());
        }
    }
}

impl FailureSignature {
    fn from_reason(reason: &FuzzFailureReason) -> Self {
        Self {
            tag: reason.tag(),
            fingerprint: reason.fingerprint(),
        }
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
    audit_source_core(source).or_else(|| audit_metamorphic_source(source))
}

fn audit_source_core(source: &str) -> Option<FuzzFailureReason> {
    let program = match parse_checked_program(source) {
        Ok(program) => program,
        Err(err) => return Some(FuzzFailureReason::Compile(err)),
    };
    let compiled = Compiler::new().compile(&program).0;

    let verification = Verifier::new().verify(&compiled);
    if !verification.valid {
        return Some(FuzzFailureReason::Verification(verification.to_string()));
    }

    let oracle = compare_ast_oracle(&program, &compiled);
    if !oracle.backend_report.equivalent {
        return Some(FuzzFailureReason::BackendComparison(
            oracle.backend_report.to_string(),
        ));
    }
    if !oracle.equivalent {
        return Some(FuzzFailureReason::AstOracle(oracle.to_string()));
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

fn audit_metamorphic_source(source: &str) -> Option<FuzzFailureReason> {
    let original = match observable_source_outcome(source) {
        Ok(outcome) => outcome,
        Err(reason) => return Some(reason),
    };

    for (index, variant) in metamorphic_variants(source).into_iter().enumerate() {
        if variant == source {
            continue;
        }
        if let Some(reason) = audit_source_core(&variant) {
            return Some(FuzzFailureReason::Metamorphic(format!(
                "variant {} failed audit: {}",
                index, reason
            )));
        }
        let variant_outcome = match observable_source_outcome(&variant) {
            Ok(outcome) => outcome,
            Err(reason) => {
                return Some(FuzzFailureReason::Metamorphic(format!(
                    "variant {} could not produce an oracle outcome: {}",
                    index, reason
                )));
            }
        };
        if variant_outcome != original {
            return Some(FuzzFailureReason::Metamorphic(format!(
                "variant {} changed observable behavior: {} vs {}",
                index,
                variant_outcome.summary(),
                original.summary()
            )));
        }
    }

    None
}

fn observable_source_outcome(source: &str) -> Result<OracleOutcome, FuzzFailureReason> {
    let program = parse_checked_program(source).map_err(FuzzFailureReason::Compile)?;
    Ok(run_ast_oracle(&program))
}

fn parse_checked_program(source: &str) -> Result<Program, String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let program = parser
        .parse()
        .map_err(|err| format!("Parse error: {}", err))?;
    SemanticAnalyzer::new()
        .analyze(&program)
        .map_err(|errors| {
            errors
                .iter()
                .map(|err| err.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })?;
    Ok(program)
}

fn shrink_source(source: &str, signature: FailureSignature) -> String {
    let mut current = source.to_string();

    for _ in 0..SHRINK_PASSES {
        let Some(candidate) = find_ast_shrink(&current, signature)
            .or_else(|| find_line_removal_shrink(&current, signature))
        else {
            break;
        };
        current = candidate;
    }

    current
}

fn find_ast_shrink(source: &str, signature: FailureSignature) -> Option<String> {
    let program = parse_program(source)?;
    for candidate in ast_shrink_candidates(&program) {
        let candidate_source = emit_program(&candidate);
        if candidate_source != source && has_same_failure(&candidate_source, signature) {
            return Some(candidate_source);
        }
    }
    None
}

fn parse_program(source: &str) -> Option<Program> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    parser.parse().ok()
}

fn ast_shrink_candidates(program: &Program) -> Vec<Program> {
    let mut candidates = Vec::new();

    for index in 0..program.globals.len() {
        let mut candidate = program.clone();
        candidate.globals.remove(index);
        candidates.push(candidate);
    }

    for index in 0..program.functions.len() {
        if program.functions[index].name == "main" {
            continue;
        }
        let mut candidate = program.clone();
        candidate.functions.remove(index);
        candidates.push(candidate);
    }

    for (function_index, function) in program.functions.iter().enumerate() {
        for body in body_shrink_candidates(&function.body) {
            let mut candidate = program.clone();
            candidate.functions[function_index].body = body;
            candidates.push(candidate);
        }
    }

    candidates
}

fn body_shrink_candidates(body: &[Stmt]) -> Vec<Vec<Stmt>> {
    let mut candidates = Vec::new();

    for index in 0..body.len() {
        if !matches!(body[index], Stmt::Return { .. }) {
            let mut candidate = body.to_vec();
            candidate.remove(index);
            candidates.push(candidate);
        }

        for replacement in stmt_shrink_candidates(&body[index]) {
            let mut candidate = body.to_vec();
            candidate.splice(index..=index, replacement);
            candidates.push(candidate);
        }
    }

    candidates
}

fn stmt_shrink_candidates(stmt: &Stmt) -> Vec<Vec<Stmt>> {
    match stmt {
        Stmt::VarDecl {
            var_type,
            name,
            init_expr,
            array_size,
            span,
        } => init_expr
            .as_ref()
            .map(|expr| {
                expr_shrink_candidates(expr)
                    .into_iter()
                    .map(|candidate_expr| {
                        vec![Stmt::VarDecl {
                            var_type: *var_type,
                            name: name.clone(),
                            init_expr: Some(candidate_expr),
                            array_size: *array_size,
                            span: *span,
                        }]
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        Stmt::Assign {
            target,
            index_expr,
            value,
            span,
        } => {
            let mut candidates = Vec::new();
            for candidate_value in expr_shrink_candidates(value) {
                candidates.push(vec![Stmt::Assign {
                    target: target.clone(),
                    index_expr: index_expr.clone(),
                    value: candidate_value,
                    span: *span,
                }]);
            }
            if let Some(index_expr) = index_expr {
                candidates.push(vec![Stmt::Assign {
                    target: target.clone(),
                    index_expr: Some(Expr::IntLiteral {
                        value: 0,
                        span: *span,
                    }),
                    value: value.clone(),
                    span: *span,
                }]);
                for candidate_index in expr_shrink_candidates(index_expr) {
                    candidates.push(vec![Stmt::Assign {
                        target: target.clone(),
                        index_expr: Some(candidate_index),
                        value: value.clone(),
                        span: *span,
                    }]);
                }
            }
            candidates
        }
        Stmt::If {
            condition,
            then_body,
            else_body,
            span,
        } => {
            let mut candidates = vec![then_body.clone()];
            if let Some(else_body) = else_body {
                candidates.push(else_body.clone());
            }
            for candidate_condition in expr_shrink_candidates(condition) {
                candidates.push(vec![Stmt::If {
                    condition: candidate_condition,
                    then_body: then_body.clone(),
                    else_body: else_body.clone(),
                    span: *span,
                }]);
            }
            for candidate_body in body_shrink_candidates(then_body) {
                candidates.push(vec![Stmt::If {
                    condition: condition.clone(),
                    then_body: candidate_body,
                    else_body: else_body.clone(),
                    span: *span,
                }]);
            }
            if let Some(else_body) = else_body {
                for candidate_else in body_shrink_candidates(else_body) {
                    candidates.push(vec![Stmt::If {
                        condition: condition.clone(),
                        then_body: then_body.clone(),
                        else_body: Some(candidate_else),
                        span: *span,
                    }]);
                }
            }
            candidates
        }
        Stmt::While {
            condition,
            body,
            span,
        } => {
            let mut candidates = vec![body.clone()];
            for candidate_condition in expr_shrink_candidates(condition) {
                candidates.push(vec![Stmt::While {
                    condition: candidate_condition,
                    body: body.clone(),
                    span: *span,
                }]);
            }
            for candidate_body in body_shrink_candidates(body) {
                candidates.push(vec![Stmt::While {
                    condition: condition.clone(),
                    body: candidate_body,
                    span: *span,
                }]);
            }
            candidates
        }
        Stmt::Return { value, span } => expr_shrink_candidates(value)
            .into_iter()
            .map(|candidate_value| {
                vec![Stmt::Return {
                    value: candidate_value,
                    span: *span,
                }]
            })
            .collect(),
        Stmt::Print { value, span } => expr_shrink_candidates(value)
            .into_iter()
            .map(|candidate_value| {
                vec![Stmt::Print {
                    value: candidate_value,
                    span: *span,
                }]
            })
            .collect(),
        Stmt::ExprStmt { expr, span } => expr_shrink_candidates(expr)
            .into_iter()
            .map(|candidate_expr| {
                vec![Stmt::ExprStmt {
                    expr: candidate_expr,
                    span: *span,
                }]
            })
            .collect(),
    }
}

fn expr_shrink_candidates(expr: &Expr) -> Vec<Expr> {
    let span = expr.span();
    let zero = Expr::IntLiteral { value: 0, span };
    match expr {
        Expr::IntLiteral { value, .. } => {
            if *value == 0 {
                Vec::new()
            } else {
                vec![zero]
            }
        }
        Expr::BoolLiteral { value, .. } => {
            if *value {
                vec![Expr::BoolLiteral { value: false, span }]
            } else {
                Vec::new()
            }
        }
        Expr::Identifier { .. } => vec![zero],
        Expr::Binary {
            op,
            left,
            right,
            span,
        } => {
            let mut candidates = vec![
                *left.clone(),
                *right.clone(),
                Expr::IntLiteral {
                    value: 0,
                    span: *span,
                },
            ];
            for candidate_left in expr_shrink_candidates(left) {
                candidates.push(Expr::Binary {
                    op: *op,
                    left: Box::new(candidate_left),
                    right: right.clone(),
                    span: *span,
                });
            }
            for candidate_right in expr_shrink_candidates(right) {
                candidates.push(Expr::Binary {
                    op: *op,
                    left: left.clone(),
                    right: Box::new(candidate_right),
                    span: *span,
                });
            }
            candidates
        }
        Expr::Unary { op, operand, span } => {
            let mut candidates = vec![*operand.clone()];
            for candidate_operand in expr_shrink_candidates(operand) {
                candidates.push(Expr::Unary {
                    op: *op,
                    operand: Box::new(candidate_operand),
                    span: *span,
                });
            }
            candidates
        }
        Expr::Call { name, args, span } => {
            let mut candidates = vec![zero];
            for index in 0..args.len() {
                for candidate_arg in expr_shrink_candidates(&args[index]) {
                    let mut candidate_args = args.clone();
                    candidate_args[index] = candidate_arg;
                    candidates.push(Expr::Call {
                        name: name.clone(),
                        args: candidate_args,
                        span: *span,
                    });
                }
            }
            candidates
        }
        Expr::ArrayIndex {
            array_name,
            index,
            span,
        } => {
            let mut candidates = vec![zero];
            candidates.push(Expr::ArrayIndex {
                array_name: array_name.clone(),
                index: Box::new(Expr::IntLiteral {
                    value: 0,
                    span: *span,
                }),
                span: *span,
            });
            for candidate_index in expr_shrink_candidates(index) {
                candidates.push(Expr::ArrayIndex {
                    array_name: array_name.clone(),
                    index: Box::new(candidate_index),
                    span: *span,
                });
            }
            candidates
        }
    }
}

fn find_line_removal_shrink(source: &str, signature: FailureSignature) -> Option<String> {
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

        if has_same_failure(&candidate, signature) {
            return Some(candidate);
        }
    }

    None
}

fn is_removable_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.ends_with(';') && !trimmed.starts_with("return ")
}

fn has_same_failure(source: &str, signature: FailureSignature) -> bool {
    audit_source(source)
        .map(|reason| FailureSignature::from_reason(&reason) == signature)
        .unwrap_or(false)
}

fn generate_program_with_mode(
    mode: FuzzMode,
    seed: u64,
    max_expr_depth: usize,
    max_statements: usize,
) -> GeneratedProgram {
    match mode {
        FuzzMode::General => ProgramGenerator::new(seed, max_expr_depth, max_statements).generate(),
        FuzzMode::OptimizerStress => OptimizerStressGenerator::new(seed).generate(),
    }
}

fn generate_guided_program(
    mode: FuzzMode,
    case_seed: u64,
    max_expr_depth: usize,
    max_statements: usize,
    guidance: &GuidanceState,
) -> GeneratedProgram {
    const CANDIDATES: usize = 6;

    let mut best = generate_program_with_mode(mode, case_seed, max_expr_depth, max_statements);
    let mut best_score = guidance.score(&best, &compiled_opcode_names(&best.source));

    for attempt in 1..CANDIDATES {
        let seed = case_seed ^ (0x9E37_79B9_7F4A_7C15_u64.wrapping_mul((attempt as u64) + 1));
        let candidate = generate_program_with_mode(mode, seed, max_expr_depth, max_statements);
        let opcodes = compiled_opcode_names(&candidate.source);
        let score = guidance.score(&candidate, &opcodes);
        if score > best_score {
            best = candidate;
            best_score = score;
        }
    }

    best
}

fn compiled_opcode_names(source: &str) -> BTreeSet<&'static str> {
    compile(source)
        .map(|compiled| {
            compiled
                .instructions
                .iter()
                .map(|instruction| opcode_name(instruction.opcode))
                .collect()
        })
        .unwrap_or_default()
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

impl GuidanceState {
    fn observe(&mut self, features: &FeatureSet, opcodes: &BTreeSet<&'static str>) {
        self.features.extend(feature_names(features));
        self.opcodes.extend(opcodes.iter().copied());
    }

    fn score(&self, generated: &GeneratedProgram, opcodes: &BTreeSet<&'static str>) -> usize {
        let new_features = feature_names(&generated.features)
            .into_iter()
            .filter(|feature| !self.features.contains(feature))
            .count();
        let new_opcodes = opcodes
            .iter()
            .filter(|opcode| !self.opcodes.contains(**opcode))
            .count();

        new_opcodes * 5 + new_features * 3 + usize::from(generated.features.optimizer_stress)
    }
}

fn feature_names(features: &FeatureSet) -> Vec<&'static str> {
    let mut names = Vec::new();
    if features.helper_functions {
        names.push("helper-functions");
    }
    if features.helper_calls {
        names.push("helper-calls");
    }
    if features.optimizer_stress {
        names.push("optimizer-stress");
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
    if features.loop_indexed_array_writes {
        names.push("loop-indexed-array-writes");
    }
    if features.helper_array_interactions {
        names.push("helper-array-interactions");
    }
    if features.constant_fold_patterns {
        names.push("constant-fold-patterns");
    }
    if features.dead_code_shapes {
        names.push("dead-code-shapes");
    }
    names
}

fn metamorphic_variants(source: &str) -> Vec<String> {
    let Some(program) = parse_program(source) else {
        return Vec::new();
    };

    let mut variants = Vec::new();

    let mut return_neutral = program.clone();
    for function in &mut return_neutral.functions {
        wrap_return_exprs_with_zero(&mut function.body);
    }
    variants.push(emit_program(&return_neutral));

    let mut dead_branch = program.clone();
    if let Some(main) = dead_branch
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
    {
        main.body.insert(0, dead_print_branch());
        variants.push(emit_program(&dead_branch));
    }

    let mut neutral_local = program;
    if let Some(main) = neutral_local
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
    {
        if !main
            .body
            .iter()
            .any(|stmt| declares_name(stmt, "__qydrel_mm0"))
        {
            main.body.splice(0..0, neutral_local_statements());
            variants.push(emit_program(&neutral_local));
        }
    }

    variants.sort();
    variants.dedup();
    variants
}

fn wrap_return_exprs_with_zero(stmts: &mut [Stmt]) {
    for stmt in stmts {
        match stmt {
            Stmt::Return { value, span } => {
                *value = Expr::Binary {
                    op: BinaryOp::Add,
                    left: Box::new(value.clone()),
                    right: Box::new(Expr::IntLiteral {
                        value: 0,
                        span: *span,
                    }),
                    span: *span,
                };
            }
            Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                wrap_return_exprs_with_zero(then_body);
                if let Some(else_body) = else_body {
                    wrap_return_exprs_with_zero(else_body);
                }
            }
            Stmt::While { body, .. } => wrap_return_exprs_with_zero(body),
            _ => {}
        }
    }
}

fn dead_print_branch() -> Stmt {
    let span = Span::default();
    Stmt::If {
        condition: Expr::IntLiteral { value: 0, span },
        then_body: vec![Stmt::Print {
            value: Expr::IntLiteral {
                value: 424242,
                span,
            },
            span,
        }],
        else_body: None,
        span,
    }
}

fn neutral_local_statements() -> Vec<Stmt> {
    let span = Span::default();
    vec![
        Stmt::VarDecl {
            var_type: Type::Int,
            name: "__qydrel_mm0".to_string(),
            init_expr: Some(Expr::IntLiteral { value: 0, span }),
            array_size: None,
            span,
        },
        Stmt::Assign {
            target: "__qydrel_mm0".to_string(),
            index_expr: None,
            value: Expr::Binary {
                op: BinaryOp::Add,
                left: Box::new(Expr::Identifier {
                    name: "__qydrel_mm0".to_string(),
                    span,
                }),
                right: Box::new(Expr::IntLiteral { value: 0, span }),
                span,
            },
            span,
        },
    ]
}

fn declares_name(stmt: &Stmt, name: &str) -> bool {
    match stmt {
        Stmt::VarDecl { name: declared, .. } => declared == name,
        Stmt::If {
            then_body,
            else_body,
            ..
        } => {
            then_body.iter().any(|stmt| declares_name(stmt, name))
                || else_body
                    .as_ref()
                    .map(|body| body.iter().any(|stmt| declares_name(stmt, name)))
                    .unwrap_or(false)
        }
        Stmt::While { body, .. } => body.iter().any(|stmt| declares_name(stmt, name)),
        _ => false,
    }
}

fn emit_program(program: &Program) -> String {
    let mut out = String::new();
    for global in &program.globals {
        emit_global(&mut out, global);
    }
    if !program.globals.is_empty() && !program.functions.is_empty() {
        out.push('\n');
    }
    for (index, function) in program.functions.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        emit_function(&mut out, function);
    }
    out
}

fn emit_global(out: &mut String, global: &GlobalVar) {
    out.push_str(type_name(global.var_type));
    out.push(' ');
    out.push_str(&global.name);
    if let Some(size) = global.array_size {
        write!(out, "[{}]", size).expect("write to string cannot fail");
    } else if let Some(init_expr) = &global.init_expr {
        out.push_str(" = ");
        out.push_str(&emit_expr(init_expr));
    }
    out.push_str(";\n");
}

fn emit_function(out: &mut String, function: &Function) {
    out.push_str("func ");
    out.push_str(&function.name);
    out.push('(');
    for (index, param) in function.params.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        emit_param(out, param);
    }
    out.push_str(") {\n");
    for stmt in &function.body {
        emit_stmt(out, stmt, 1);
    }
    out.push_str("}\n");
}

fn emit_param(out: &mut String, param: &Param) {
    out.push_str(type_name(param.param_type));
    out.push(' ');
    out.push_str(&param.name);
}

fn emit_stmt(out: &mut String, stmt: &Stmt, indent: usize) {
    push_indent(out, indent);
    match stmt {
        Stmt::VarDecl {
            var_type,
            name,
            init_expr,
            array_size,
            ..
        } => {
            out.push_str(type_name(*var_type));
            out.push(' ');
            out.push_str(name);
            if let Some(size) = array_size {
                write!(out, "[{}]", size).expect("write to string cannot fail");
            } else if let Some(init_expr) = init_expr {
                out.push_str(" = ");
                out.push_str(&emit_expr(init_expr));
            }
            out.push_str(";\n");
        }
        Stmt::Assign {
            target,
            index_expr,
            value,
            ..
        } => {
            out.push_str(target);
            if let Some(index_expr) = index_expr {
                out.push('[');
                out.push_str(&emit_expr(index_expr));
                out.push(']');
            }
            out.push_str(" = ");
            out.push_str(&emit_expr(value));
            out.push_str(";\n");
        }
        Stmt::If {
            condition,
            then_body,
            else_body,
            ..
        } => {
            out.push_str("if (");
            out.push_str(&emit_expr(condition));
            out.push_str(") {\n");
            for stmt in then_body {
                emit_stmt(out, stmt, indent + 1);
            }
            push_indent(out, indent);
            out.push('}');
            if let Some(else_body) = else_body {
                out.push_str(" else {\n");
                for stmt in else_body {
                    emit_stmt(out, stmt, indent + 1);
                }
                push_indent(out, indent);
                out.push('}');
            }
            out.push('\n');
        }
        Stmt::While {
            condition, body, ..
        } => {
            out.push_str("while (");
            out.push_str(&emit_expr(condition));
            out.push_str(") {\n");
            for stmt in body {
                emit_stmt(out, stmt, indent + 1);
            }
            push_indent(out, indent);
            out.push_str("}\n");
        }
        Stmt::Return { value, .. } => {
            out.push_str("return ");
            out.push_str(&emit_expr(value));
            out.push_str(";\n");
        }
        Stmt::Print { value, .. } => {
            out.push_str("print ");
            out.push_str(&emit_expr(value));
            out.push_str(";\n");
        }
        Stmt::ExprStmt { expr, .. } => {
            out.push_str(&emit_expr(expr));
            out.push_str(";\n");
        }
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn emit_expr(expr: &Expr) -> String {
    match expr {
        Expr::IntLiteral { value, .. } => value.to_string(),
        Expr::BoolLiteral { value, .. } => value.to_string(),
        Expr::Identifier { name, .. } => name.clone(),
        Expr::Binary {
            op, left, right, ..
        } => format!(
            "({} {} {})",
            emit_expr(left),
            binary_op_symbol(*op),
            emit_expr(right)
        ),
        Expr::Unary { op, operand, .. } => {
            format!("({}{})", unary_op_symbol(*op), emit_expr(operand))
        }
        Expr::Call { name, args, .. } => {
            let args = args.iter().map(emit_expr).collect::<Vec<_>>().join(", ");
            format!("{}({})", name, args)
        }
        Expr::ArrayIndex {
            array_name, index, ..
        } => {
            format!("{}[{}]", array_name, emit_expr(index))
        }
    }
}

fn type_name(ty: Type) -> &'static str {
    match ty {
        Type::Int => "int",
        Type::Bool => "bool",
        Type::Void | Type::Error => "int",
    }
}

fn binary_op_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Gt => ">",
        BinaryOp::Le => "<=",
        BinaryOp::Ge => ">=",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
    }
}

fn unary_op_symbol(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "!",
    }
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

fn write_corpus_repro(
    corpus_dir: &Path,
    run_seed: u64,
    case_index: usize,
    case_seed: u64,
    reason_tag: &str,
    minimized_source: &str,
) -> io::Result<PathBuf> {
    fs::create_dir_all(corpus_dir)?;
    let file_name = format!(
        "{}_seed_{:016x}_case_{:04}_case_seed_{:016x}.lang",
        reason_tag, run_seed, case_index, case_seed
    );
    let path = corpus_dir.join(file_name);
    fs::write(path.as_path(), minimized_source)?;
    Ok(path)
}

fn failure_manifest(input: &FailureArtifactInput<'_>) -> String {
    format!(
        "MiniLang Fuzz Failure Manifest\n\
         run_seed: {:#018x}\n\
         case_index: {}\n\
         case_seed: {:#018x}\n\
         reason: {}\n\
         failure_fingerprint: {:016x}\n\
         original_source_hash: {:016x}\n\
         minimized_source_hash: {:016x}\n\
         repro_command: minilang --fuzz {} --fuzz-seed {:#018x}\n\
         case_features: {}\n\
         run_coverage_at_failure:\n{}",
        input.run_seed,
        input.case_index,
        input.case_seed,
        input.reason.tag(),
        input.failure_fingerprint,
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

        source.push('\n');
    }

    fn generate_helpers(&mut self, source: &mut String) {
        let count = 1 + self.rng.usize(2);
        self.features.helper_functions = true;
        self.features.branches = true;
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
        self.generate_helper_array_smoke(source);

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

    fn generate_helper_array_smoke(&mut self, source: &mut String) {
        let (Some(array), Some(helper)) = (self.local_arrays.first(), self.helpers.first()) else {
            return;
        };
        let array_name = array.name.clone();
        let scope = array.scope;
        let helper = helper.clone();
        let args = (0..helper.arity)
            .map(|index| {
                if index == 0 {
                    self.features.record_array_read(scope);
                    format!("{}[0]", array_name)
                } else {
                    "acc".to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.features.helper_calls = true;
        self.features.helper_array_interactions = true;
        self.features.record_array_write(scope);
        source.push_str(&format!(
            "  {}[0] = {}({});\n",
            array_name, helper.name, args
        ));
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
        let loop_array = self.pick_loop_array();
        let limit = loop_array
            .as_ref()
            .map(|array| 1 + self.rng.usize(array.size.min(5)))
            .unwrap_or_else(|| 1 + self.rng.usize(5));
        source.push_str(&format!("  int {} = 0;\n", index_name));
        self.locals.push(index_name.clone());

        let expr = self.int_expr();
        source.push_str(&format!("  while ({} < {}) {{\n", index_name, limit));
        if let Some(array) = loop_array {
            self.features.record_array_write(array.scope);
            self.features.record_array_read(array.scope);
            self.features.loop_indexed_array_writes = true;
            source.push_str(&format!(
                "    {}[{}] = ({} + {});\n",
                array.name, index_name, expr, index_name
            ));
            source.push_str(&format!(
                "    acc = (acc + {}[{}]);\n",
                array.name, index_name
            ));
        } else {
            source.push_str(&format!("    acc = (acc + {});\n", expr));
        }
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

    fn pick_loop_array(&mut self) -> Option<ArrayBinding> {
        let arrays = self.arrays_in_scope();
        if arrays.is_empty() || !self.rng.chance(3, 4) {
            return None;
        }
        let index = self.rng.usize(arrays.len());
        Some(arrays[index].clone())
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

struct OptimizerStressGenerator {
    rng: Rng,
}

impl OptimizerStressGenerator {
    fn new(seed: u64) -> Self {
        Self {
            rng: Rng::new(seed),
        }
    }

    fn generate(mut self) -> GeneratedProgram {
        let features = FeatureSet {
            optimizer_stress: true,
            branches: true,
            loops: true,
            prints: true,
            constant_fold_patterns: true,
            dead_code_shapes: true,
            ..FeatureSet::default()
        };

        let a = 1 + self.rng.usize(9) as i32;
        let b = 1 + self.rng.usize(9) as i32;
        let c = 1 + self.rng.usize(5) as i32;
        let loop_limit = 2 + self.rng.usize(4);
        let dead_value = 100 + self.rng.usize(900) as i32;
        let branch_bias = self.rng.usize(3) as i32;

        let source = format!(
            "func main() {{\n\
             \x20\x20int acc = (({a} + {b}) * ({c} + 1));\n\
             \x20\x20acc = (acc * 1);\n\
             \x20\x20acc = (acc + 0);\n\
             \x20\x20int i = 0;\n\
             \x20\x20while (i < {loop_limit}) {{\n\
             \x20\x20\x20\x20acc = (acc + ((i * 1) + 0));\n\
             \x20\x20\x20\x20i = (i + 1);\n\
             \x20\x20}}\n\
             \x20\x20if ((({a} + {branch_bias}) >= {a}) && (acc != {dead_value})) {{\n\
             \x20\x20\x20\x20acc = (acc + (2 * 1));\n\
             \x20\x20}} else {{\n\
             \x20\x20\x20\x20acc = (acc + {dead_value});\n\
             \x20\x20}}\n\
             \x20\x20print (acc + 0);\n\
             \x20\x20return acc;\n\
             \x20\x20acc = (acc + {dead_value});\n\
             }}\n"
        );

        GeneratedProgram { source, features }
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
        writeln!(f, "  coverage-guided cases: {}", self.coverage_guided_cases)?;
        writeln!(f, "  AST oracle comparisons: {}", self.oracle_comparisons)?;
        writeln!(
            f,
            "  metamorphic variants checked: {}",
            self.metamorphic_variants
        )?;
        writeln!(f, "  opcode kinds seen: {}", self.opcode_kinds.len())?;
        if !self.opcode_kinds.is_empty() {
            writeln!(
                f,
                "  opcode coverage: {}",
                self.opcode_kinds
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            )?;
        }
        writeln!(
            f,
            "  optimizer stress cases: {}",
            self.optimizer_stress_cases
        )?;
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
        )?;
        writeln!(
            f,
            "  cases with loop-indexed array writes: {}",
            self.loop_indexed_array_writes
        )?;
        writeln!(
            f,
            "  cases with helper/array interactions: {}",
            self.helper_array_interactions
        )?;
        writeln!(
            f,
            "  cases with constant-fold patterns: {}",
            self.constant_fold_patterns
        )?;
        writeln!(
            f,
            "  cases with dead-code shapes: {}",
            self.dead_code_shapes
        )
    }
}

impl fmt::Display for FuzzFailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FuzzFailureReason::Compile(msg) => write!(f, "compile failure: {}", msg),
            FuzzFailureReason::Verification(msg) => write!(f, "verification failure:\n{}", msg),
            FuzzFailureReason::AstOracle(msg) => {
                write!(f, "AST oracle comparison failure:\n{}", msg)
            }
            FuzzFailureReason::BackendComparison(msg) => {
                write!(f, "backend comparison failure:\n{}", msg)
            }
            FuzzFailureReason::TraceReplay(msg) => write!(f, "trace replay failure:\n{}", msg),
            FuzzFailureReason::TraceDiff(msg) => write!(f, "trace diff failure:\n{}", msg),
            FuzzFailureReason::Metamorphic(msg) => {
                write!(f, "metamorphic equivalence failure:\n{}", msg)
            }
        }
    }
}

fn push_coverage_json(out: &mut String, coverage: &FuzzCoverage) {
    out.push('{');
    write!(out, "\"cases\":{}", coverage.cases).expect("write to string cannot fail");
    write!(
        out,
        ",\"coverage_guided_cases\":{}",
        coverage.coverage_guided_cases
    )
    .expect("write to string cannot fail");
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
    write!(
        out,
        ",\"opcode_kinds_seen\":{}",
        coverage.opcode_kinds.len()
    )
    .expect("write to string cannot fail");
    out.push_str(",\"opcode_kinds\":[");
    for (index, opcode) in coverage.opcode_kinds.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_json_string(out, opcode);
    }
    out.push(']');
    write!(
        out,
        ",\"optimizer_stress_cases\":{}",
        coverage.optimizer_stress_cases
    )
    .expect("write to string cannot fail");
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
    write!(
        out,
        ",\"loop_indexed_array_writes\":{}",
        coverage.loop_indexed_array_writes
    )
    .expect("write to string cannot fail");
    write!(
        out,
        ",\"helper_array_interactions\":{}",
        coverage.helper_array_interactions
    )
    .expect("write to string cannot fail");
    write!(
        out,
        ",\"constant_fold_patterns\":{}",
        coverage.constant_fold_patterns
    )
    .expect("write to string cannot fail");
    write!(out, ",\"dead_code_shapes\":{}", coverage.dead_code_shapes)
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
    write!(
        out,
        ",\"failure_fingerprint\":\"{:016x}\"",
        failure.failure_fingerprint
    )
    .expect("write to string cannot fail");
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
    if features.optimizer_stress {
        names.push("optimizer-stress");
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
    if features.loop_indexed_array_writes {
        names.push("loop-indexed-array-writes");
    }
    if features.helper_array_interactions {
        names.push("helper-array-interactions");
    }
    if features.constant_fold_patterns {
        names.push("constant-fold-patterns");
    }
    if features.dead_code_shapes {
        names.push("dead-code-shapes");
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
        assert_eq!(report.coverage.helper_functions, 12);
        assert_eq!(report.coverage.coverage_guided_cases, 12);
        assert!(report.coverage.oracle_comparisons >= 12);
        assert!(report.coverage.metamorphic_variants >= 12);
        assert!(report.coverage.opcode_kinds.contains("Return"));
        assert!(report.coverage.helper_array_interactions > 0);
        assert!(report.to_json().contains("\"success\":true"));
        assert!(report.to_json().contains("\"coverage\""));
        assert!(report.to_json().contains("\"opcode_kinds\""));
    }

    #[test]
    fn generated_program_contains_global_and_local_arrays() {
        let generated =
            ProgramGenerator::new(4321, DEFAULT_MAX_EXPR_DEPTH, DEFAULT_MAX_STATEMENTS).generate();

        assert!(generated.source.contains("int ga0["));
        assert!(generated.source.contains("int la0["));
        assert!(generated.source.contains("la0[0] = acc;"));
        assert!(generated.source.contains("acc = (acc + la0[0]);"));
        assert!(generated.source.contains("func f0("));
        assert!(generated.features.local_array_reads);
        assert!(generated.features.local_array_writes);
        assert!(generated.features.helper_array_interactions);
        assert!(
            audit_source(&generated.source).is_none(),
            "{}",
            generated.source
        );
    }

    #[test]
    fn generated_programs_cover_loop_indexed_arrays() {
        let mut found = false;
        for seed in 0..128 {
            let generated =
                ProgramGenerator::new(seed, DEFAULT_MAX_EXPR_DEPTH, DEFAULT_MAX_STATEMENTS)
                    .generate();
            if generated.features.loop_indexed_array_writes {
                assert!(generated.source.contains("while"));
                assert!(
                    audit_source(&generated.source).is_none(),
                    "{}",
                    generated.source
                );
                found = true;
                break;
            }
        }

        assert!(
            found,
            "expected at least one deterministic seed to cover loop-indexed arrays"
        );
    }

    #[test]
    fn optimizer_stress_mode_passes_audit_pipeline() {
        let report = run_fuzzer(FuzzConfig {
            seed: 0x0f7,
            cases: 8,
            artifact_dir: None,
            mode: FuzzMode::OptimizerStress,
            ..FuzzConfig::default()
        });

        assert!(report.success, "{report:#?}");
        assert_eq!(report.coverage.optimizer_stress_cases, 8);
        assert_eq!(report.coverage.constant_fold_patterns, 8);
        assert_eq!(report.coverage.dead_code_shapes, 8);
        assert!(report.to_json().contains("\"optimizer_stress_cases\":8"));
    }

    #[test]
    fn ast_shrinker_generates_function_statement_and_expression_reductions() {
        let source = concat!(
            "func unused() {\n",
            "  return 1;\n",
            "}\n\n",
            "func main() {\n",
            "  int x = ((1 + 2) * (3 + 4));\n",
            "  if (x > 0) {\n",
            "    print x;\n",
            "  } else {\n",
            "    print 0;\n",
            "  }\n",
            "  return x;\n",
            "}\n"
        );
        let program = parse_program(source).unwrap();
        let candidates = ast_shrink_candidates(&program)
            .into_iter()
            .map(|candidate| emit_program(&candidate))
            .collect::<Vec<_>>();

        assert!(candidates
            .iter()
            .any(|candidate| !candidate.contains("func unused")));
        assert!(candidates
            .iter()
            .any(|candidate| !candidate.contains("if (")));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.contains("int x = (1 + 2);")));
    }

    #[test]
    fn shrinker_preserves_matching_failure_fingerprint() {
        let source = concat!(
            "func main() {\n",
            "  int x = 1;\n",
            "  print x;\n",
            "  return missing;\n",
            "}\n"
        );

        let signature = FailureSignature::from_reason(&audit_source(source).unwrap());
        let shrunk = shrink_source(source, signature);
        assert!(shrunk.len() < source.len(), "{shrunk}");
        assert!(has_same_failure(&shrunk, signature));
    }

    #[test]
    fn metamorphic_variants_preserve_observable_behavior() {
        let source = concat!(
            "func main() {\n",
            "  int x = 3;\n",
            "  print x;\n",
            "  return x * 7;\n",
            "}\n"
        );

        let original = observable_source_outcome(source).unwrap();
        let variants = metamorphic_variants(source);
        assert!(variants.len() >= 3);
        for variant in variants {
            assert!(
                audit_source_core(&variant).is_none(),
                "variant failed audit:\n{}",
                variant
            );
            assert_eq!(observable_source_outcome(&variant).unwrap(), original);
        }
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
            failure_fingerprint: reason.fingerprint(),
            original_source: "func main() { return 1; }\n",
            minimized_source: "func main() { return 1; }\n",
            coverage: &coverage,
            case_features: &features,
        });

        assert!(manifest.contains("case_index: 3"));
        assert!(manifest.contains("reason: trace-diff"));
        assert!(manifest.contains("failure_fingerprint:"));
        assert!(
            manifest.contains("repro_command: minilang --fuzz 4 --fuzz-seed 0x0000000000005eed")
        );
        assert!(manifest.contains("local-array-reads"));
        assert!(manifest.contains("original_source_hash:"));
    }
}
