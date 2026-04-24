//! Runtime audit reports built on top of execution traces.
//!
//! These helpers are deliberately independent of the CLI so fuzzers, tests, and
//! CI artifact tooling can ask the same questions as a human operator.

use crate::compiler::CompiledProgram;
use crate::gc_vm::{GcVm, GcVmResult};
use crate::trace::{first_semantic_trace_divergence, first_trace_divergence, TraceDivergence};
use crate::vm::{TrapCode, Vm, VmResult};
use std::fmt;

/// Minimal observable result used when comparing traced runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionSummary {
    pub success: bool,
    pub return_value: i64,
    pub trap_code: TrapCode,
    pub output: Vec<String>,
}

/// Deterministic replay check for the reference VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceReplayReport {
    pub replayable: bool,
    pub event_count: usize,
    pub original_outcome: ExecutionSummary,
    pub replay_outcome: ExecutionSummary,
    pub divergence: Option<TraceDivergence>,
    pub outcome_mismatch: Option<String>,
}

/// Instruction trace diff between the standard VM and GC VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendTraceDiffReport {
    pub equivalent: bool,
    pub left_backend: String,
    pub right_backend: String,
    pub left_events: usize,
    pub right_events: usize,
    pub left_outcome: ExecutionSummary,
    pub right_outcome: ExecutionSummary,
    pub divergence: Option<TraceDivergence>,
    pub outcome_mismatch: Option<String>,
}

/// Run the reference VM twice and verify that its instruction trace replays.
pub fn replay_vm_trace(program: &CompiledProgram) -> TraceReplayReport {
    let mut original_vm = Vm::new(program).with_trace();
    let original_result = original_vm.run();
    let original_trace = original_vm.trace_events().to_vec();

    let mut replay_vm = Vm::new(program).with_trace();
    let replay_result = replay_vm.run();
    let replay_trace = replay_vm.trace_events();

    let original_outcome = ExecutionSummary::from_vm(original_result);
    let replay_outcome = ExecutionSummary::from_vm(replay_result);
    let outcome_mismatch = outcome_mismatch(&original_outcome, &replay_outcome);
    let divergence = first_trace_divergence(&original_trace, replay_trace);

    TraceReplayReport {
        replayable: outcome_mismatch.is_none() && divergence.is_none(),
        event_count: original_trace.len(),
        original_outcome,
        replay_outcome,
        divergence,
        outcome_mismatch,
    }
}

/// Compare the standard VM and GC VM on the same bytecode at trace granularity.
pub fn diff_vm_gc_traces(program: &CompiledProgram) -> BackendTraceDiffReport {
    let mut vm = Vm::new(program).with_trace();
    let vm_result = vm.run();
    let vm_trace = vm.trace_events().to_vec();

    let mut gc_vm = GcVm::new(program).with_trace();
    let gc_result = gc_vm.run();
    let gc_trace = gc_vm.trace_events();

    let left_outcome = ExecutionSummary::from_vm(vm_result);
    let right_outcome = ExecutionSummary::from_gc_vm(gc_result);
    let outcome_mismatch = outcome_mismatch(&left_outcome, &right_outcome);
    let divergence = first_semantic_trace_divergence(&vm_trace, gc_trace);

    BackendTraceDiffReport {
        equivalent: outcome_mismatch.is_none() && divergence.is_none(),
        left_backend: "VM".to_string(),
        right_backend: "GC VM".to_string(),
        left_events: vm_trace.len(),
        right_events: gc_trace.len(),
        left_outcome,
        right_outcome,
        divergence,
        outcome_mismatch,
    }
}

impl ExecutionSummary {
    fn from_vm(result: VmResult) -> Self {
        Self {
            success: result.success,
            return_value: result.return_value,
            trap_code: result.trap_code,
            output: result.output,
        }
    }

    fn from_gc_vm(result: GcVmResult) -> Self {
        Self {
            success: result.success,
            return_value: result.return_value,
            trap_code: result.trap_code,
            output: result.output,
        }
    }
}

fn outcome_mismatch(left: &ExecutionSummary, right: &ExecutionSummary) -> Option<String> {
    if left == right {
        None
    } else {
        Some(format!(
            "{} vs {}",
            format_outcome(left),
            format_outcome(right)
        ))
    }
}

fn format_outcome(outcome: &ExecutionSummary) -> String {
    format!(
        "success={}, return={}, trap={:?}, output={:?}",
        outcome.success, outcome.return_value, outcome.trap_code, outcome.output
    )
}

impl fmt::Display for TraceReplayReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Trace Replay")?;
        writeln!(
            f,
            "  status: {}",
            if self.replayable {
                "replayable"
            } else {
                "diverged"
            }
        )?;
        writeln!(f, "  events: {}", self.event_count)?;
        writeln!(f, "  original: {}", format_outcome(&self.original_outcome))?;
        writeln!(f, "  replay:   {}", format_outcome(&self.replay_outcome))?;

        if let Some(mismatch) = &self.outcome_mismatch {
            writeln!(f)?;
            writeln!(f, "Outcome Mismatch")?;
            writeln!(f, "  {}", mismatch)?;
        }

        if let Some(divergence) = &self.divergence {
            writeln!(f)?;
            writeln!(f, "First Trace Divergence")?;
            writeln!(f, "  {}", divergence)?;
        }

        Ok(())
    }
}

impl fmt::Display for BackendTraceDiffReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Backend Trace Diff")?;
        writeln!(
            f,
            "  status: {}",
            if self.equivalent {
                "equivalent"
            } else {
                "mismatch"
            }
        )?;
        writeln!(f, "  {} events: {}", self.left_backend, self.left_events)?;
        writeln!(f, "  {} events: {}", self.right_backend, self.right_events)?;
        writeln!(
            f,
            "  {}: {}",
            self.left_backend,
            format_outcome(&self.left_outcome)
        )?;
        writeln!(
            f,
            "  {}: {}",
            self.right_backend,
            format_outcome(&self.right_outcome)
        )?;

        if let Some(mismatch) = &self.outcome_mismatch {
            writeln!(f)?;
            writeln!(f, "Outcome Mismatch")?;
            writeln!(f, "  {}", mismatch)?;
        }

        if let Some(divergence) = &self.divergence {
            writeln!(f)?;
            writeln!(f, "First Trace Divergence")?;
            writeln!(f, "  {}", divergence)?;
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
    fn replays_reference_vm_trace() {
        let program = compile_source("func main() { int x = 40 + 2; return x; }");
        let report = replay_vm_trace(&program);

        assert!(report.replayable, "{report:#?}");
        assert!(report.event_count > 0);
    }

    #[test]
    fn diffs_vm_and_gc_trace_for_simple_program() {
        let program = compile_source("func main() { int x = 5; return x * 3; }");
        let report = diff_vm_gc_traces(&program);

        assert!(report.equivalent, "{report:#?}");
        assert!(report.left_events > 0);
        assert_eq!(report.left_events, report.right_events);
    }
}
