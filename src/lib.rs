//! MiniLang - A minimal systems programming language compiler
//!
//! Features:
//! - Complete compiler pipeline (lexer → parser → semantic analysis → bytecode)
//! - Experimental x86-64 JIT compiler for a small linear expression subset
//! - Custom memory allocators (bump, free-list, slab) - ACTUALLY USED
//! - Mark-sweep garbage collector - ACTUALLY USED
//! - Stack-based bytecode VM
//! - Optimization passes (constant folding, DCE, strength reduction)
//! - Interactive REPL with accumulated definitions
//!
//! This project demonstrates core systems programming concepts:
//! - Manual memory management
//! - Machine code generation
//! - Low-level OS interfaces (mmap/mprotect)
//! - Performance optimization

pub mod alloc;
pub mod arena_ast;
pub mod ast;
pub mod audit;
pub mod compare;
pub mod compiler;
pub mod fuzz;
pub mod gc;
pub mod gc_vm;
pub mod jit;
pub mod lexer;
pub mod limits;
pub mod optimizer;
pub mod parser;
pub mod repl;
pub mod runtime;
pub mod sema;
pub mod token;
pub mod trace;
pub mod verifier;
pub mod vm;

pub use alloc::{AllocatorStats, BumpAllocator, FreeListAllocator, SlabAllocator};
pub use arena_ast::{ArenaExpr, ArenaStmt, ArenaStr, ArenaVec, AstArena};
pub use ast::{BinaryOp, Expr, Function, Program, Stmt, Type, UnaryOp};
pub use audit::{
    diff_vm_gc_traces, replay_vm_trace, BackendTraceDiffReport, ExecutionSummary, TraceReplayReport,
};
pub use compare::{
    compare_backends, BackendComparisonReport, BackendOutcome, BackendRun, BackendRunStatus,
};
pub use compiler::{CompiledProgram, Compiler, Opcode};
pub use fuzz::{
    run_fuzzer, FuzzConfig, FuzzCoverage, FuzzFailure, FuzzFailureReason, FuzzMode, FuzzReport,
};
pub use gc::{GarbageCollector, GcStats, TypeTag};
pub use gc_vm::{GcValue, GcVm, GcVmResult, HeapArray};
pub use jit::{ExecutableMemory, JitCompiler, MachineCode, Reg};
pub use lexer::Lexer;
pub use optimizer::{OptimizationStats, Optimizer};
pub use parser::Parser;
pub use repl::Repl;
pub use runtime::{GcArray, GlobalStore, LocalFrame, Value, ValueStack};
pub use sema::SemanticAnalyzer;
pub use token::{Span, Token, TokenKind};
pub use trace::{
    events_to_json, first_semantic_trace_divergence, first_trace_divergence, summarize_trace,
    trace_fingerprint, trace_summary_to_json, TraceDivergence, TraceEvent, TraceOutcome,
    TraceSummary,
};
pub use verifier::{
    BackendEligibility, BackendStatus, FunctionVerification, VerificationError, VerificationReport,
    Verifier,
};
pub use vm::{TrapCode, Vm, VmResult};

/// Compile and run a MiniLang program
pub fn run(source: &str) -> Result<VmResult, String> {
    // Lexical analysis
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();

    // Parsing
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| format!("Parse error: {}", e))?;

    // Semantic analysis
    let mut analyzer = SemanticAnalyzer::new();
    analyzer.analyze(&program).map_err(|errors| {
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    // Compilation
    let (compiled, _) = Compiler::new().compile(&program);

    // Execution
    let mut vm = Vm::new(&compiled);
    Ok(vm.run())
}

/// Compile to bytecode without running
pub fn compile(source: &str) -> Result<CompiledProgram, String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize();

    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| format!("Parse error: {}", e))?;

    let mut analyzer = SemanticAnalyzer::new();
    analyzer.analyze(&program).map_err(|errors| {
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    Ok(Compiler::new().compile(&program).0)
}

/// JIT compile and run (Linux x86-64 only)
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn run_jit(source: &str) -> Result<i64, String> {
    let compiled = compile(source)?;

    let jit = JitCompiler::new();
    let exec_mem = jit.compile(&compiled).ok_or("Failed to JIT compile")?;

    // Call the JIT-compiled code
    let func: extern "C" fn() -> i64 = exec_mem.as_fn();
    Ok(func())
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
pub fn run_jit(_source: &str) -> Result<i64, String> {
    Err("JIT compilation only supported on Linux x86-64".to_string())
}
