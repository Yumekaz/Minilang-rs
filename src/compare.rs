//! Backend comparison for MiniLang.
//!
//! This module runs the same compiled program across available execution
//! backends and compares their observable behavior. It is deliberately
//! separated from the CLI so tests and future fuzzing can call it directly.

use crate::compiler::CompiledProgram;
use crate::gc_vm::GcVm;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use crate::jit::JitCompiler;
use crate::optimizer::Optimizer;
use crate::verifier::{BackendStatus, Verifier};
use crate::vm::{TrapCode, Vm};

/// One backend execution outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRun {
    pub name: String,
    pub status: BackendRunStatus,
}

/// Whether a backend executed, skipped, or failed before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendRunStatus {
    Executed(BackendOutcome),
    Skipped(String),
}

/// Observable backend behavior used for equivalence checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendOutcome {
    pub success: bool,
    pub return_value: i64,
    pub trap_code: TrapCode,
    pub output: Vec<String>,
}

/// Full backend comparison output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendComparisonReport {
    pub equivalent: bool,
    pub reference_backend: Option<String>,
    pub runs: Vec<BackendRun>,
    pub mismatches: Vec<String>,
}

/// Compare available execution backends for a compiled program.
pub fn compare_backends(program: &CompiledProgram) -> BackendComparisonReport {
    let mut runs = Vec::new();

    let mut vm = Vm::new(program);
    runs.push(BackendRun::executed(
        "VM",
        BackendOutcome::from_vm(vm.run()),
    ));

    let mut gc_vm = GcVm::new(program);
    runs.push(BackendRun::executed(
        "GC VM",
        BackendOutcome::from_gc_vm(gc_vm.run()),
    ));

    let mut optimizer = Optimizer::new();
    let optimized = optimizer.optimize(program.clone());
    let mut opt_vm = Vm::new(&optimized);
    runs.push(BackendRun::executed(
        "Optimized VM",
        BackendOutcome::from_vm(opt_vm.run()),
    ));

    runs.push(run_jit(program));

    let (reference_backend, mismatches) = compare_observable_outcomes(&runs);

    BackendComparisonReport {
        equivalent: mismatches.is_empty(),
        reference_backend,
        runs,
        mismatches,
    }
}

impl BackendRun {
    fn executed(name: &str, outcome: BackendOutcome) -> Self {
        Self {
            name: name.to_string(),
            status: BackendRunStatus::Executed(outcome),
        }
    }

    fn skipped(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            status: BackendRunStatus::Skipped(reason.into()),
        }
    }
}

impl BackendOutcome {
    fn from_vm(result: crate::vm::VmResult) -> Self {
        Self {
            success: result.success,
            return_value: result.return_value,
            trap_code: result.trap_code,
            output: result.output,
        }
    }

    fn from_gc_vm(result: crate::gc_vm::GcVmResult) -> Self {
        Self {
            success: result.success,
            return_value: result.return_value,
            trap_code: result.trap_code,
            output: result.output,
        }
    }

    fn from_jit_return(return_value: i64) -> Self {
        Self {
            success: true,
            return_value,
            trap_code: TrapCode::None,
            output: Vec::new(),
        }
    }
}

fn run_jit(program: &CompiledProgram) -> BackendRun {
    let verifier = Verifier::new();
    let status = verifier.verify(program).backend_eligibility.jit;
    if !status.eligible {
        return BackendRun::skipped("JIT", backend_reason(status));
    }

    run_jit_eligible(program)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn run_jit_eligible(program: &CompiledProgram) -> BackendRun {
    let jit = JitCompiler::new();
    let Some(exec_mem) = jit.compile(program) else {
        return BackendRun::skipped("JIT", "JIT compiler rejected bytecode");
    };
    let func: extern "C" fn() -> i64 = exec_mem.as_fn();
    BackendRun::executed("JIT", BackendOutcome::from_jit_return(func()))
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn run_jit_eligible(_program: &CompiledProgram) -> BackendRun {
    BackendRun::skipped("JIT", "current build target is not linux x86-64")
}

fn backend_reason(status: BackendStatus) -> String {
    status
        .reason
        .unwrap_or_else(|| "backend is not eligible".to_string())
}

fn compare_observable_outcomes(runs: &[BackendRun]) -> (Option<String>, Vec<String>) {
    let reference = runs.iter().find_map(|run| match &run.status {
        BackendRunStatus::Executed(outcome) => Some((run.name.clone(), outcome.clone())),
        BackendRunStatus::Skipped(_) => None,
    });

    let Some((reference_name, reference_outcome)) = reference else {
        return (None, vec!["no backend executed".to_string()]);
    };

    let mut mismatches = Vec::new();
    for run in runs {
        let BackendRunStatus::Executed(outcome) = &run.status else {
            continue;
        };

        if outcome != &reference_outcome {
            mismatches.push(format!(
                "{} differs from {}: {} vs {}",
                run.name,
                reference_name,
                format_outcome(outcome),
                format_outcome(&reference_outcome)
            ));
        }
    }

    (Some(reference_name), mismatches)
}

fn format_outcome(outcome: &BackendOutcome) -> String {
    format!(
        "success={}, return={}, trap={:?}, output={:?}",
        outcome.success, outcome.return_value, outcome.trap_code, outcome.output
    )
}

impl std::fmt::Display for BackendComparisonReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Backend Comparison")?;
        writeln!(
            f,
            "  status: {}",
            if self.equivalent {
                "equivalent"
            } else {
                "mismatch"
            }
        )?;
        match &self.reference_backend {
            Some(name) => writeln!(f, "  reference: {}", name)?,
            None => writeln!(f, "  reference: none")?,
        }

        writeln!(f)?;
        writeln!(f, "Runs")?;
        for run in &self.runs {
            match &run.status {
                BackendRunStatus::Executed(outcome) => {
                    writeln!(f, "  {}: {}", run.name, format_outcome(outcome))?;
                }
                BackendRunStatus::Skipped(reason) => {
                    writeln!(f, "  {}: skipped ({})", run.name, reason)?;
                }
            }
        }

        if !self.mismatches.is_empty() {
            writeln!(f)?;
            writeln!(f, "Mismatches")?;
            for mismatch in &self.mismatches {
                writeln!(f, "  {}", mismatch)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::sema::SemanticAnalyzer;

    fn compile_source(source: &str) -> CompiledProgram {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().expect("parse failed");
        let mut analyzer = SemanticAnalyzer::new();
        analyzer.analyze(&program).expect("sema failed");
        Compiler::new().compile(&program).0
    }

    #[test]
    fn compares_successful_backends() {
        let program = compile_source("func main() { int x = 40 + 2; return x; }");
        let report = compare_backends(&program);

        assert!(report.equivalent, "{:#?}", report.mismatches);
        assert!(report.runs.len() >= 3);
    }

    #[test]
    fn compares_trapping_backends() {
        let program = compile_source("func main() { return 10 / 0; }");
        let report = compare_backends(&program);

        assert!(report.equivalent, "{:#?}", report.mismatches);
        assert!(report.runs.iter().any(|run| {
            matches!(
                &run.status,
                BackendRunStatus::Executed(outcome)
                    if !outcome.success && outcome.trap_code == TrapCode::DivideByZero
            )
        }));
    }
}
