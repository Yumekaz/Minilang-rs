//! MiniLang CLI
//!
//! Command-line interface for the MiniLang compiler.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;
use std::time::Instant;

use minilang::{
    compare_backends, compiler::disassemble, diff_vm_gc_traces, replay_vm_trace, run_fuzzer,
    Compiler, FuzzConfig, FuzzMode, GcVm, JitCompiler, Lexer, Optimizer, Parser, Repl,
    SemanticAnalyzer, Verifier, Vm,
};

fn print_usage() {
    eprintln!("MiniLang Compiler v{}", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("Usage: minilang [OPTIONS] [file.lang]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --tokens     Print tokens and exit");
    eprintln!("  --ast        Print AST and exit");
    eprintln!("  --ir         Print bytecode IR");
    eprintln!("  --verify     Verify bytecode safety and backend eligibility");
    eprintln!("  --compare-backends");
    eprintln!(
        "               Run VM, GC VM, optimized VM, and JIT when eligible, then compare results"
    );
    eprintln!("  --opt        Enable optimizations");
    eprintln!("  --gc         Use GC-integrated VM (heap arrays)");
    eprintln!("  --jit        Use JIT compiler (linear expressions only, Linux x86-64)");
    eprintln!("  --debug      Enable debug output");
    eprintln!("  --bench      Run with timing information");
    eprintln!("  --stats      Show allocator/GC/optimizer statistics");
    eprintln!("  --trace-json <file>");
    eprintln!("               Write selected VM execution trace as JSON");
    eprintln!("  --trace-replay");
    eprintln!("               Verify reference VM trace determinism");
    eprintln!("  --trace-diff");
    eprintln!("               Compare VM and GC VM instruction traces");
    eprintln!("  --audit-json <file>");
    eprintln!("               Write trace replay/diff audit evidence as JSON");
    eprintln!("  --fuzz <cases>");
    eprintln!("               Generate deterministic programs and run the self-audit pipeline");
    eprintln!("  --fuzz-seed <n>");
    eprintln!("               Seed for --fuzz (decimal or 0x-prefixed hex)");
    eprintln!("  --fuzz-artifacts <dir>");
    eprintln!("               Directory for minimized failing repro artifacts");
    eprintln!("  --fuzz-corpus-out <dir>");
    eprintln!("               Save minimized failing repros into a regression corpus directory");
    eprintln!("  --fuzz-json <file>");
    eprintln!("               Write a machine-readable fuzz audit summary");
    eprintln!("  --fuzz-mode <general|optimizer-stress>");
    eprintln!("               Select generated program family");
    eprintln!("  --fuzz-max-expr-depth <n>");
    eprintln!("               Maximum generated expression depth");
    eprintln!("  --fuzz-max-statements <n>");
    eprintln!("               Maximum generated statements per main function");
    eprintln!("  --fuzz-no-shrink");
    eprintln!("               Keep the first failing generated program without shrinking");
    eprintln!("  --fuzz-no-artifacts");
    eprintln!("               Disable failure artifact output");
    eprintln!("  --repl       Start interactive REPL");
    eprintln!("  --eval <e>   Evaluate expression and exit");
    eprintln!("  --no-color   Disable color output (no-op, for compatibility)");
    eprintln!("  --help       Print this help message");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let mut filename = None;
    let mut show_tokens = false;
    let mut show_ast = false;
    let mut show_ir = false;
    let mut verify = false;
    let mut compare = false;
    let mut use_jit = false;
    let mut use_gc = false;
    let mut debug = false;
    let mut bench = false;
    let mut show_stats = false;
    let mut trace_json_path: Option<String> = None;
    let mut trace_replay = false;
    let mut trace_diff = false;
    let mut audit_json_path: Option<PathBuf> = None;
    let mut fuzz_cases: Option<usize> = None;
    let mut fuzz_seed: Option<u64> = None;
    let mut fuzz_artifacts: Option<PathBuf> = None;
    let mut fuzz_corpus_out: Option<PathBuf> = None;
    let mut fuzz_json_path: Option<PathBuf> = None;
    let mut fuzz_max_expr_depth: Option<usize> = None;
    let mut fuzz_max_statements: Option<usize> = None;
    let mut fuzz_shrink = true;
    let mut fuzz_write_artifacts = true;
    let mut fuzz_mode = FuzzMode::General;
    let mut use_opt = false;
    let mut start_repl = false;
    let mut eval_expr: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tokens" => show_tokens = true,
            "--ast" => show_ast = true,
            "--ir" => show_ir = true,
            "--verify" => verify = true,
            "--compare-backends" => compare = true,
            "--opt" => use_opt = true,
            "--gc" => use_gc = true,
            "--jit" => use_jit = true,
            "--debug" => debug = true,
            "--bench" => bench = true,
            "--stats" => show_stats = true,
            "--trace-json" => {
                i += 1;
                if i < args.len() {
                    trace_json_path = Some(args[i].clone());
                } else {
                    eprintln!("Error: --trace-json requires an output file");
                    process::exit(1);
                }
            }
            "--trace-replay" => trace_replay = true,
            "--trace-diff" => trace_diff = true,
            "--audit-json" => {
                i += 1;
                if i < args.len() {
                    audit_json_path = Some(PathBuf::from(&args[i]));
                } else {
                    eprintln!("Error: --audit-json requires an output file");
                    process::exit(1);
                }
            }
            "--fuzz" => {
                i += 1;
                if i < args.len() {
                    fuzz_cases = Some(parse_usize_arg("--fuzz", &args[i]));
                } else {
                    eprintln!("Error: --fuzz requires a case count");
                    process::exit(1);
                }
            }
            "--fuzz-seed" => {
                i += 1;
                if i < args.len() {
                    fuzz_seed = Some(parse_u64_arg("--fuzz-seed", &args[i]));
                } else {
                    eprintln!("Error: --fuzz-seed requires a seed");
                    process::exit(1);
                }
            }
            "--fuzz-artifacts" => {
                i += 1;
                if i < args.len() {
                    fuzz_artifacts = Some(PathBuf::from(&args[i]));
                } else {
                    eprintln!("Error: --fuzz-artifacts requires a directory");
                    process::exit(1);
                }
            }
            "--fuzz-corpus-out" => {
                i += 1;
                if i < args.len() {
                    fuzz_corpus_out = Some(PathBuf::from(&args[i]));
                } else {
                    eprintln!("Error: --fuzz-corpus-out requires a directory");
                    process::exit(1);
                }
            }
            "--fuzz-json" => {
                i += 1;
                if i < args.len() {
                    fuzz_json_path = Some(PathBuf::from(&args[i]));
                } else {
                    eprintln!("Error: --fuzz-json requires an output file");
                    process::exit(1);
                }
            }
            "--fuzz-mode" => {
                i += 1;
                if i < args.len() {
                    fuzz_mode = parse_fuzz_mode(&args[i]);
                } else {
                    eprintln!("Error: --fuzz-mode requires general or optimizer-stress");
                    process::exit(1);
                }
            }
            "--fuzz-max-expr-depth" => {
                i += 1;
                if i < args.len() {
                    fuzz_max_expr_depth = Some(parse_usize_arg("--fuzz-max-expr-depth", &args[i]));
                } else {
                    eprintln!("Error: --fuzz-max-expr-depth requires a positive integer");
                    process::exit(1);
                }
            }
            "--fuzz-max-statements" => {
                i += 1;
                if i < args.len() {
                    fuzz_max_statements = Some(parse_usize_arg("--fuzz-max-statements", &args[i]));
                } else {
                    eprintln!("Error: --fuzz-max-statements requires a positive integer");
                    process::exit(1);
                }
            }
            "--fuzz-no-shrink" => fuzz_shrink = false,
            "--fuzz-no-artifacts" => fuzz_write_artifacts = false,
            "--repl" => start_repl = true,
            "--eval" => {
                i += 1;
                if i < args.len() {
                    eval_expr = Some(args[i].clone());
                } else {
                    eprintln!("Error: --eval requires an expression");
                    process::exit(1);
                }
            }
            "--no-color" => { /* no-op for compatibility */ }
            "--help" | "-h" => {
                print_usage();
                process::exit(0);
            }
            _ if !args[i].starts_with('-') => filename = Some(args[i].clone()),
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                process::exit(1);
            }
        }
        i += 1;
    }

    if let Some(cases) = fuzz_cases {
        if audit_json_path.is_some() {
            eprintln!("Error: --audit-json requires --trace-replay or --trace-diff");
            process::exit(1);
        }

        let default_config = FuzzConfig::default();
        let artifact_dir = if fuzz_write_artifacts {
            fuzz_artifacts.clone().or(default_config.artifact_dir)
        } else {
            None
        };
        let report = run_fuzzer(FuzzConfig {
            seed: fuzz_seed.unwrap_or(default_config.seed),
            cases,
            max_expr_depth: fuzz_max_expr_depth.unwrap_or(default_config.max_expr_depth),
            max_statements: fuzz_max_statements.unwrap_or(default_config.max_statements),
            artifact_dir,
            corpus_dir: fuzz_corpus_out.clone(),
            shrink: fuzz_shrink,
            mode: fuzz_mode,
        });
        println!("{}", report);
        if let Some(path) = fuzz_json_path {
            if let Err(e) = fs::write(path.as_path(), report.to_json()) {
                eprintln!("Error writing fuzz JSON to {}: {}", path.display(), e);
                process::exit(1);
            }
        }
        process::exit(if report.success { 0 } else { 1 });
    }

    if fuzz_json_path.is_some()
        || fuzz_seed.is_some()
        || fuzz_artifacts.is_some()
        || fuzz_corpus_out.is_some()
        || fuzz_max_expr_depth.is_some()
        || fuzz_max_statements.is_some()
        || fuzz_mode != FuzzMode::General
        || !fuzz_shrink
        || !fuzz_write_artifacts
    {
        eprintln!("Error: fuzz tuning options require --fuzz <cases>");
        process::exit(1);
    }

    // Handle REPL mode
    if start_repl {
        let mut repl = Repl::new().with_verbose(debug);
        if let Err(e) = repl.run() {
            eprintln!("REPL error: {}", e);
            process::exit(1);
        }
        return;
    }

    // Handle eval mode
    if let Some(expr) = eval_expr {
        match minilang::repl::eval(&expr) {
            Ok(result) => {
                println!("{}", result);
                process::exit(0);
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                process::exit(1);
            }
        }
    }

    let filename = match filename {
        Some(f) => f,
        None => {
            eprintln!("Error: No input file specified");
            print_usage();
            process::exit(1);
        }
    };

    // Read source file
    let source = match fs::read_to_string(&filename) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {}: {}", filename, e);
            process::exit(1);
        }
    };

    let total_start = Instant::now();

    // Lexical analysis
    let lex_start = Instant::now();
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize();
    let lex_time = lex_start.elapsed();

    if show_tokens {
        println!("=== Tokens ===");
        for token in &tokens {
            println!("  {:?}", token);
        }
        return;
    }

    // Parsing
    let parse_start = Instant::now();
    let mut parser = Parser::new(tokens);
    let program = match parser.parse() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Parse error: {}", e);
            process::exit(1);
        }
    };
    let parse_time = parse_start.elapsed();

    if show_ast {
        println!("=== AST ===");
        println!("{:#?}", program);
        return;
    }

    // Semantic analysis
    let sema_start = Instant::now();
    let mut analyzer = SemanticAnalyzer::new();
    if let Err(errors) = analyzer.analyze(&program) {
        eprintln!("=== Semantic Errors ===");
        for e in errors {
            eprintln!("  {}", e);
        }
        process::exit(1);
    }
    let sema_time = sema_start.elapsed();

    // Compilation
    let compile_start = Instant::now();
    let (compiled, compiler_arena_stats) = Compiler::new().compile(&program);
    let compile_time = compile_start.elapsed();

    // Optimization (if enabled)
    let opt_start = Instant::now();
    let mut optimizer = Optimizer::new();
    let compiled = if use_opt {
        optimizer.optimize(compiled)
    } else {
        compiled
    };
    let opt_time = opt_start.elapsed();

    if show_ir {
        println!("{}", disassemble(&compiled));
        if use_opt {
            println!();
            println!("{}", optimizer.stats());
        }
        return;
    }

    if verify {
        let report = Verifier::new().verify(&compiled);
        println!("{}", report);
        process::exit(if report.valid { 0 } else { 1 });
    }

    if compare {
        let report = compare_backends(&compiled);
        println!("{}", report);
        process::exit(if report.equivalent { 0 } else { 1 });
    }

    if trace_replay {
        let report = replay_vm_trace(&compiled);
        println!("{}", report);
        if let Some(path) = audit_json_path.as_deref() {
            if let Err(e) = fs::write(path, report.evidence_json()) {
                eprintln!("Error writing audit JSON to {}: {}", path.display(), e);
                process::exit(1);
            }
        }
        process::exit(if report.replayable { 0 } else { 1 });
    }

    if trace_diff {
        let report = diff_vm_gc_traces(&compiled);
        println!("{}", report);
        if let Some(path) = audit_json_path.as_deref() {
            if let Err(e) = fs::write(path, report.evidence_json()) {
                eprintln!("Error writing audit JSON to {}: {}", path.display(), e);
                process::exit(1);
            }
        }
        process::exit(if report.equivalent { 0 } else { 1 });
    }

    if audit_json_path.is_some() {
        eprintln!("Error: --audit-json requires --trace-replay or --trace-diff");
        process::exit(1);
    }

    // Execution
    let exec_start = Instant::now();

    if trace_json_path.is_some() && use_jit {
        eprintln!("Error: --trace-json does not support the JIT backend");
        process::exit(1);
    }

    if use_jit {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let jit = JitCompiler::new();
            match jit.compile(&compiled) {
                Some(exec_mem) => {
                    let func: extern "C" fn() -> i64 = exec_mem.as_fn();
                    let result = func();
                    let exec_time = exec_start.elapsed();

                    if bench {
                        println!();
                        println!("=== Timing (JIT) ===");
                        println!("  Lexer:     {:>10.3}ms", lex_time.as_secs_f64() * 1000.0);
                        println!("  Parser:    {:>10.3}ms", parse_time.as_secs_f64() * 1000.0);
                        println!("  Semantic:  {:>10.3}ms", sema_time.as_secs_f64() * 1000.0);
                        println!(
                            "  Compile:   {:>10.3}ms",
                            compile_time.as_secs_f64() * 1000.0
                        );
                        if use_opt {
                            println!("  Optimize:  {:>10.3}ms", opt_time.as_secs_f64() * 1000.0);
                        }
                        println!("  JIT Exec:  {:>10.3}ms", exec_time.as_secs_f64() * 1000.0);
                        println!(
                            "  Total:     {:>10.3}ms",
                            total_start.elapsed().as_secs_f64() * 1000.0
                        );
                    }

                    if show_stats && use_opt {
                        println!();
                        println!("{}", optimizer.stats());
                    }

                    process::exit(result as i32);
                }
                None => {
                    // JIT doesn't support this program, fall back to interpreter
                    eprintln!(
                        "Note: JIT doesn't support this program's bytecode subset, using interpreter"
                    );
                }
            }
        }

        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            eprintln!("JIT compilation only supported on Linux x86-64");
            process::exit(1);
        }
    }

    if use_gc {
        // GC-integrated VM (heap-allocated arrays)
        let mut vm = GcVm::new(&compiled).with_debug(debug);
        if trace_json_path.is_some() {
            vm = vm.with_trace();
        }
        let result = vm.run();
        let exec_time = exec_start.elapsed();

        if let Some(path) = trace_json_path.as_deref() {
            if let Err(e) = fs::write(path, vm.trace_json()) {
                eprintln!("Error writing trace JSON to {}: {}", path, e);
                process::exit(1);
            }
        }

        if !result.success {
            eprintln!(
                "TRAP: {:?} (code {}) at PC {} after {} cycles",
                result.trap_code, result.trap_code as u8, result.pc, result.cycles
            );
            eprintln!("Stack depth: {}", result.stack_depth);
            eprintln!("Frame depth: {}", result.frame_depth);
            process::exit(result.trap_code as i32);
        }

        if bench {
            println!();
            println!("=== Timing (GC VM) ===");
            println!("  Lexer:      {:>10.3}ms", lex_time.as_secs_f64() * 1000.0);
            println!(
                "  Parser:     {:>10.3}ms",
                parse_time.as_secs_f64() * 1000.0
            );
            println!("  Semantic:   {:>10.3}ms", sema_time.as_secs_f64() * 1000.0);
            println!(
                "  Compile:    {:>10.3}ms",
                compile_time.as_secs_f64() * 1000.0
            );
            if use_opt {
                println!("  Optimize:   {:>10.3}ms", opt_time.as_secs_f64() * 1000.0);
            }
            println!(
                "  Execute:    {:>10.3}ms ({} cycles)",
                exec_time.as_secs_f64() * 1000.0,
                result.cycles
            );
            println!(
                "  Total:      {:>10.3}ms",
                total_start.elapsed().as_secs_f64() * 1000.0
            );
        }

        if show_stats {
            println!();
            println!("=== Compiler Arena Stats ===");
            println!("{}", compiler_arena_stats);
            println!();
            println!("=== GC Statistics ===");
            println!("{}", vm.gc_stats());
            println!();
            println!("=== VM Allocator Stats ===");
            println!("{}", vm.allocator_stats());
            if use_opt {
                println!();
                println!("{}", optimizer.stats());
            }
        }

        process::exit(result.return_value as i32);
    } else {
        // Standard interpreter
        let mut vm = Vm::new(&compiled).with_debug(debug);
        if trace_json_path.is_some() {
            vm = vm.with_trace();
        }
        let result = vm.run();
        let exec_time = exec_start.elapsed();

        if let Some(path) = trace_json_path.as_deref() {
            if let Err(e) = fs::write(path, vm.trace_json()) {
                eprintln!("Error writing trace JSON to {}: {}", path, e);
                process::exit(1);
            }
        }

        if !result.success {
            // Trap message format matching Python spec exactly:
            // TRAP: TRAP_<NAME> (code N) at PC <pc> after <cycles> cycles
            // Stack depth: <len(operand_stack)>
            // Frame depth: <len(frame_stack)>
            eprintln!(
                "TRAP: {:?} (code {}) at PC {} after {} cycles",
                result.trap_code, result.trap_code as u8, result.pc, result.cycles
            );
            eprintln!("Stack depth: {}", result.stack_depth);
            eprintln!("Frame depth: {}", result.frame_depth);
            process::exit(result.trap_code as i32);
        }

        if bench {
            println!();
            println!("=== Timing (Interpreter) ===");
            println!("  Lexer:      {:>10.3}ms", lex_time.as_secs_f64() * 1000.0);
            println!(
                "  Parser:     {:>10.3}ms",
                parse_time.as_secs_f64() * 1000.0
            );
            println!("  Semantic:   {:>10.3}ms", sema_time.as_secs_f64() * 1000.0);
            println!(
                "  Compile:    {:>10.3}ms",
                compile_time.as_secs_f64() * 1000.0
            );
            if use_opt {
                println!("  Optimize:   {:>10.3}ms", opt_time.as_secs_f64() * 1000.0);
            }
            println!(
                "  Execute:    {:>10.3}ms ({} cycles)",
                exec_time.as_secs_f64() * 1000.0,
                result.cycles
            );
            println!(
                "  Total:      {:>10.3}ms",
                total_start.elapsed().as_secs_f64() * 1000.0
            );
        }

        if show_stats {
            println!();
            println!("=== Allocator Stats ===");
            println!("{}", vm.allocator_stats());
            println!();
            println!("=== GC Stats ===");
            println!("{}", vm.gc_stats());
            if use_opt {
                println!();
                println!("{}", optimizer.stats());
            }
        }

        process::exit(result.return_value as i32);
    }
}

fn parse_usize_arg(name: &str, value: &str) -> usize {
    match value.parse::<usize>() {
        Ok(parsed) if parsed > 0 => parsed,
        _ => {
            eprintln!("Error: {} expects a positive integer, got {}", name, value);
            process::exit(1);
        }
    }
}

fn parse_u64_arg(name: &str, value: &str) -> u64 {
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse::<u64>()
    };

    match parsed {
        Ok(seed) => seed,
        Err(_) => {
            eprintln!("Error: {} expects a decimal or 0x-prefixed seed", name);
            process::exit(1);
        }
    }
}

fn parse_fuzz_mode(value: &str) -> FuzzMode {
    match value {
        "general" => FuzzMode::General,
        "optimizer-stress" => FuzzMode::OptimizerStress,
        _ => {
            eprintln!(
                "Error: --fuzz-mode expects general or optimizer-stress, got {}",
                value
            );
            process::exit(1);
        }
    }
}
