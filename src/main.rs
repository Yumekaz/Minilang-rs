//! MiniLang CLI
//!
//! Command-line interface for the MiniLang compiler.

use std::env;
use std::fs;
use std::process;
use std::time::Instant;

use minilang::{
    compiler::disassemble, Compiler, GcVm, JitCompiler, Lexer, Optimizer, Parser, Repl,
    SemanticAnalyzer, Vm,
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
    eprintln!("  --opt        Enable optimizations");
    eprintln!("  --gc         Use GC-integrated VM (heap arrays)");
    eprintln!("  --jit        Use JIT compiler (simple programs only, Linux x86-64)");
    eprintln!("  --debug      Enable debug output");
    eprintln!("  --bench      Run with timing information");
    eprintln!("  --stats      Show allocator/GC/optimizer statistics");
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
    let mut use_jit = false;
    let mut use_gc = false;
    let mut debug = false;
    let mut bench = false;
    let mut show_stats = false;
    let mut use_opt = false;
    let mut start_repl = false;
    let mut eval_expr: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tokens" => show_tokens = true,
            "--ast" => show_ast = true,
            "--ir" => show_ir = true,
            "--opt" => use_opt = true,
            "--gc" => use_gc = true,
            "--jit" => use_jit = true,
            "--debug" => debug = true,
            "--bench" => bench = true,
            "--stats" => show_stats = true,
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

    // Execution
    let exec_start = Instant::now();

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
                    eprintln!("Note: JIT doesn't support this program (function calls), using interpreter");
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
        let result = vm.run();
        let exec_time = exec_start.elapsed();

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
        let result = vm.run();
        let exec_time = exec_start.elapsed();

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
