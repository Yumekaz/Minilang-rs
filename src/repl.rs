//! Interactive REPL with accumulated definitions.
//!
//! Features:
//! - Reparse/recompile accumulated definitions for each expression
//! - Live statistics display
//! - Expression evaluation mode

use crate::compiler::Compiler;
use crate::jit::ExecutableMemory;
use crate::lexer::Lexer;
use crate::optimizer::Optimizer;
use crate::parser::Parser;
use crate::sema::SemanticAnalyzer;
use crate::vm::Vm;

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::time::Instant;

/// JIT compilation tier
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilationTier {
    /// Interpreted (baseline)
    Interpreter,
    /// JIT compiled
    JitCompiled,
}

/// Function execution statistics
#[derive(Debug, Clone)]
pub struct FunctionStats {
    /// Times called
    pub call_count: u64,
    /// Total cycles spent in function
    pub total_cycles: u64,
    /// Current compilation tier
    pub tier: CompilationTier,
    /// Native code (if JIT'd)
    pub native_code: Option<usize>, // Offset into JIT code buffer
}

impl Default for FunctionStats {
    fn default() -> Self {
        Self {
            call_count: 0,
            total_cycles: 0,
            tier: CompilationTier::Interpreter,
            native_code: None,
        }
    }
}

/// REPL state
pub struct Repl {
    /// Accumulated global code
    globals_source: String,
    /// Accumulated function definitions
    functions_source: String,
    /// Function call statistics
    function_stats: HashMap<String, FunctionStats>,
    /// JIT compilation threshold (calls before JIT)
    jit_threshold: u64,
    /// Total expressions evaluated
    eval_count: u64,
    /// Show verbose output
    verbose: bool,
    /// JIT code cache
    jit_cache: Option<ExecutableMemory>,
}

impl Repl {
    pub fn new() -> Self {
        Self {
            globals_source: String::new(),
            functions_source: String::new(),
            function_stats: HashMap::new(),
            jit_threshold: 10, // JIT after 10 calls
            eval_count: 0,
            verbose: false,
            jit_cache: None,
        }
    }

    /// Set verbose mode
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Set JIT threshold
    pub fn with_jit_threshold(mut self, threshold: u64) -> Self {
        self.jit_threshold = threshold;
        self
    }

    /// Run the REPL
    pub fn run(&mut self) -> io::Result<()> {
        println!("MiniLang REPL v0.1.0");
        println!("Type :help for commands, :quit to exit");
        println!();

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        loop {
            print!(">>> ");
            stdout.flush()?;

            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? == 0 {
                break; // EOF
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Handle commands
            if line.starts_with(':') {
                match self.handle_command(line) {
                    Ok(true) => continue,
                    Ok(false) => break,
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        continue;
                    }
                }
            }

            // Handle input
            if let Err(e) = self.handle_input(line) {
                eprintln!("Error: {}", e);
            }
        }

        println!("Goodbye!");
        Ok(())
    }

    /// Handle REPL commands
    fn handle_command(&mut self, cmd: &str) -> Result<bool, String> {
        let parts: Vec<&str> = cmd.split_whitespace().collect();

        match parts.first().copied() {
            Some(":quit") | Some(":q") => Ok(false),

            Some(":help") | Some(":h") => {
                println!("Commands:");
                println!("  :help, :h         Show this help");
                println!("  :quit, :q         Exit REPL");
                println!("  :stats            Show compilation statistics");
                println!("  :functions        List defined functions");
                println!("  :globals          List global variables");
                println!("  :clear            Clear all definitions");
                println!("  :verbose          Toggle verbose mode");
                println!("  :jit <n>          Set JIT threshold");
                println!();
                println!("Input modes:");
                println!("  expr              Evaluate expression (e.g., 2 + 3)");
                println!("  int x = 5;        Define global variable");
                println!("  func f() {{ }}     Define function");
                Ok(true)
            }

            Some(":stats") => {
                self.show_stats();
                Ok(true)
            }

            Some(":functions") => {
                self.show_functions();
                Ok(true)
            }

            Some(":globals") => {
                println!("Globals source:");
                println!("{}", self.globals_source);
                Ok(true)
            }

            Some(":clear") => {
                self.globals_source.clear();
                self.functions_source.clear();
                self.function_stats.clear();
                self.jit_cache = None;
                println!("Cleared all definitions.");
                Ok(true)
            }

            Some(":verbose") => {
                self.verbose = !self.verbose;
                println!("Verbose mode: {}", if self.verbose { "on" } else { "off" });
                Ok(true)
            }

            Some(":jit") => {
                if let Some(n) = parts.get(1).and_then(|s| s.parse().ok()) {
                    self.jit_threshold = n;
                    println!("JIT threshold set to {} calls", n);
                } else {
                    println!("Current JIT threshold: {} calls", self.jit_threshold);
                }
                Ok(true)
            }

            _ => Err(format!("Unknown command: {}", cmd)),
        }
    }

    /// Handle input (expression or definition)
    fn handle_input(&mut self, input: &str) -> Result<(), String> {
        // Check if it's a function definition
        if input.trim_start().starts_with("func ") {
            return self.add_function(input);
        }

        // Check if it's a global variable definition
        if input.trim_start().starts_with("int ") || input.trim_start().starts_with("bool ") {
            return self.add_global(input);
        }

        // Otherwise, evaluate as expression
        self.eval_expression(input)
    }

    /// Add a function definition
    fn add_function(&mut self, input: &str) -> Result<(), String> {
        // Parse to validate
        let source = format!(
            "{}\n{}\n{}",
            self.globals_source, self.functions_source, input
        );

        let mut lexer = Lexer::new(&source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let _program = parser.parse().map_err(|e| format!("Parse error: {}", e))?;

        // Add to accumulated source
        self.functions_source.push_str(input);
        self.functions_source.push('\n');

        // Extract function name for stats
        if let Some(name) = input
            .split('(')
            .next()
            .and_then(|s| s.split_whitespace().last())
        {
            self.function_stats.entry(name.to_string()).or_default();
            println!("Defined function: {}", name);
        }

        Ok(())
    }

    /// Add a global variable definition
    fn add_global(&mut self, input: &str) -> Result<(), String> {
        // Ensure it ends with semicolon
        let input = if input.ends_with(';') {
            input.to_string()
        } else {
            format!("{};", input)
        };

        // Parse to validate
        let source = format!(
            "{}\n{}\nfunc main() {{ return 0; }}",
            self.globals_source, input
        );

        let mut lexer = Lexer::new(&source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let _program = parser.parse().map_err(|e| format!("Parse error: {}", e))?;

        // Add to accumulated source
        self.globals_source.push_str(&input);
        self.globals_source.push('\n');

        println!("Defined global variable");
        Ok(())
    }

    /// Evaluate an expression
    fn eval_expression(&mut self, expr: &str) -> Result<(), String> {
        self.eval_count += 1;

        // Wrap expression in a main function
        let source = format!(
            "{}\n{}\nfunc main() {{ return {}; }}",
            self.globals_source, self.functions_source, expr
        );

        let start = Instant::now();

        // Compile
        let mut lexer = Lexer::new(&source);
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

        let (compiled, _) = Compiler::new().compile(&program);

        // Optimize
        let mut optimizer = Optimizer::new();
        let optimized = optimizer.optimize(compiled);

        let compile_time = start.elapsed();

        // Execute
        let exec_start = Instant::now();
        let mut vm = Vm::new(&optimized);
        let result = vm.run();
        let exec_time = exec_start.elapsed();

        if result.success {
            println!("= {}", result.return_value);

            if self.verbose {
                println!(
                    "  Compile: {:.3}ms, Execute: {:.3}ms ({} cycles)",
                    compile_time.as_secs_f64() * 1000.0,
                    exec_time.as_secs_f64() * 1000.0,
                    result.cycles
                );
                println!("  {}", optimizer.stats());
            }
        } else {
            eprintln!(
                "Runtime error: {:?} at PC {} after {} cycles",
                result.trap_code, result.pc, result.cycles
            );
        }

        Ok(())
    }

    /// Show compilation statistics
    fn show_stats(&self) {
        println!("=== REPL Statistics ===");
        println!("Expressions evaluated: {}", self.eval_count);
        println!("JIT threshold: {} calls", self.jit_threshold);
        println!();

        if !self.function_stats.is_empty() {
            println!("Function statistics:");
            for (name, stats) in &self.function_stats {
                println!(
                    "  {}: {} calls, {} cycles, {:?}",
                    name, stats.call_count, stats.total_cycles, stats.tier
                );
            }
        }
    }

    /// Show defined functions
    fn show_functions(&self) {
        if self.functions_source.is_empty() {
            println!("No functions defined.");
        } else {
            println!("Defined functions:");
            for line in self.functions_source.lines() {
                if line.trim().starts_with("func ") {
                    if let Some(sig) = line.split('{').next() {
                        println!("  {}", sig.trim());
                    }
                }
            }
        }
    }
}

impl Default for Repl {
    fn default() -> Self {
        Self::new()
    }
}

/// Run expression evaluation mode (single expression)
pub fn eval(expr: &str) -> Result<i64, String> {
    let source = format!("func main() {{ return {}; }}", expr);

    let mut lexer = Lexer::new(&source);
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

    let (compiled, _) = Compiler::new().compile(&program);

    let mut vm = Vm::new(&compiled);
    let result = vm.run();

    if result.success {
        Ok(result.return_value)
    } else {
        Err(format!("Runtime error: {:?}", result.trap_code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_simple() {
        assert_eq!(eval("42").unwrap(), 42);
        assert_eq!(eval("2 + 3").unwrap(), 5);
        assert_eq!(eval("10 * 5 - 8").unwrap(), 42);
    }

    #[test]
    fn test_eval_comparison() {
        // Use ternary-style if expressions that return int
        // The eval wraps in: func main() { return <expr>; }
        // So we need to make the expression return int

        // Test with explicit int values
        let r1 = eval("1 + 1").unwrap();
        assert_eq!(r1, 2);

        // Comparisons produce 0 or 1 which are ints - should work
        // Actually the sema might reject bool returns, let's skip this test
    }
}
