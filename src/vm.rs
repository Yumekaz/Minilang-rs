//! Virtual Machine (Interpreter) for MiniLang bytecode.
//!
//! A stack-based VM that executes bytecode instructions.
//! Used as fallback when JIT is not available and for debugging.

use crate::alloc::BumpAllocator;
use crate::compiler::{CompiledProgram, Opcode};
use crate::gc::GarbageCollector;

/// Trap codes for runtime errors (matching Python spec exactly)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapCode {
    None = 0,
    DivideByZero = 1,      // TRAP_DIV_ZERO
    UndefinedLocal = 2,    // TRAP_UNDEFINED_LOCAL
    ArrayOutOfBounds = 3,  // TRAP_ARRAY_OOB
    StackOverflow = 4,     // TRAP_STACK_OVERFLOW
    CycleLimit = 5,        // TRAP_CYCLE_LIMIT
    UndefinedFunction = 6, // TRAP_UNDEFINED_FUNCTION
    StackUnderflow = 7,
    InvalidInstruction = 8,
}

/// Call frame for function calls
#[derive(Debug, Clone)]
struct CallFrame {
    /// Return address (PC to return to)
    return_pc: usize,
    /// Base pointer (stack index)
    base_ptr: usize,
    /// Function info
    func_id: usize,
}

/// VM execution result
#[derive(Debug)]
pub struct VmResult {
    pub success: bool,
    pub return_value: i64,
    pub trap_code: TrapCode,
    pub trap_message: String,
    pub cycles: u64,
    pub output: Vec<String>,
    pub pc: usize,
    pub stack_depth: usize,
    pub frame_depth: usize,
}

/// Virtual Machine
pub struct Vm<'a> {
    program: &'a CompiledProgram,
    /// Operand stack
    stack: Vec<i64>,
    /// Local variables (per frame)
    locals: Vec<i64>,
    /// Global variables (256 slots, initialized to 0)
    globals: Vec<i64>,
    /// Call stack
    call_stack: Vec<CallFrame>,
    /// Program counter
    pc: usize,
    /// Cycle counter
    cycles: u64,
    /// Maximum cycles (0 = unlimited)
    max_cycles: u64,
    /// Output buffer
    output: Vec<String>,
    /// Debug mode
    debug: bool,
    /// Allocator for runtime allocations
    allocator: BumpAllocator,
    /// Garbage collector
    gc: GarbageCollector,
}

/// VM limits matching Python spec
impl<'a> Vm<'a> {
    pub const MAX_GLOBALS: usize = 256;
    pub const MAX_LOCAL_SLOTS: usize = 1024;
    pub const MAX_FRAMES: usize = 100;
    pub const MAX_OPERAND_STACK: usize = 1000;
    pub const MAX_CYCLES: u64 = 100_000;
    pub const MAX_INSTRUCTIONS: usize = 10_000;

    /// Create a new VM for the given program
    pub fn new(program: &'a CompiledProgram) -> Self {
        // Calculate total globals needed (max 256)
        let globals_size = program
            .globals
            .values()
            .map(|g| if g.is_array { g.array_size } else { 1 })
            .sum::<usize>()
            .max(Self::MAX_GLOBALS);

        Self {
            program,
            stack: Vec::with_capacity(Self::MAX_OPERAND_STACK),
            locals: vec![i64::MIN; Self::MAX_LOCAL_SLOTS], // Use MIN as "undefined" marker
            globals: vec![0; globals_size],                // Initialized to 0 per spec
            call_stack: Vec::with_capacity(Self::MAX_FRAMES),
            pc: 0,
            cycles: 0,
            max_cycles: Self::MAX_CYCLES,
            output: Vec::new(),
            debug: false,
            allocator: BumpAllocator::new(1024 * 1024), // 1MB
            gc: GarbageCollector::new(512 * 1024),      // 512KB threshold
        }
    }

    /// Enable debug mode
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    /// Set maximum cycles
    pub fn with_max_cycles(mut self, max: u64) -> Self {
        self.max_cycles = max;
        self
    }

    /// Create a trap result with proper formatting
    fn make_trap_result(&mut self, trap_code: TrapCode, message: String) -> VmResult {
        VmResult {
            success: false,
            return_value: 0,
            trap_code,
            trap_message: message,
            cycles: self.cycles,
            output: std::mem::take(&mut self.output),
            pc: self.pc,
            stack_depth: self.stack.len(),
            frame_depth: self.call_stack.len(),
        }
    }

    /// Run the program
    pub fn run(&mut self) -> VmResult {
        // Find main function
        let main_func = match self.program.functions.get(&self.program.main_func_id) {
            Some(f) => f.clone(),
            None => {
                return self.make_trap_result(
                    TrapCode::UndefinedFunction,
                    "No main function found".to_string(),
                );
            }
        };

        if main_func.local_count > Self::MAX_LOCAL_SLOTS {
            return self.make_trap_result(
                TrapCode::StackOverflow,
                format!(
                    "Local storage overflow (need {} slots, max {})",
                    main_func.local_count,
                    Self::MAX_LOCAL_SLOTS
                ),
            );
        }

        // Set up initial frame
        self.pc = main_func.entry_pc;
        self.call_stack.push(CallFrame {
            return_pc: usize::MAX, // Sentinel for exit
            base_ptr: 0,
            func_id: main_func.id,
        });

        // Execute
        loop {
            // Check cycle limit
            if self.max_cycles > 0 && self.cycles >= self.max_cycles {
                return self.make_trap_result(
                    TrapCode::CycleLimit,
                    format!("Cycle limit exceeded ({} cycles)", self.max_cycles),
                );
            }

            // Fetch instruction
            if self.pc >= self.program.instructions.len() {
                return self.make_trap_result(
                    TrapCode::InvalidInstruction,
                    format!("PC out of bounds: {}", self.pc),
                );
            }

            // Fetch instruction (bounds check eliminated in release)
            let instr = unsafe { self.program.instructions.get_unchecked(self.pc) };
            self.cycles += 1;

            if self.debug {
                let stack_before: Vec<String> = self.stack.iter().map(|v| v.to_string()).collect();
                eprintln!(
                    "[cycle={} pc={}] EXEC {:?} {} {} | stack=[{}]",
                    self.cycles,
                    self.pc,
                    instr.opcode,
                    instr.arg1,
                    instr.arg2,
                    stack_before.join(", ")
                );
            }

            // Execute
            match self.execute_instruction(instr.opcode, instr.arg1, instr.arg2) {
                Ok(true) => {
                    // Normal execution, continue
                    self.pc += 1;
                }
                Ok(false) => {
                    // Instruction handled PC change
                }
                Err((trap_code, msg)) => {
                    return self.make_trap_result(trap_code, msg);
                }
            }

            // Check for exit
            if self.call_stack.is_empty() {
                let return_value = self.stack.pop().unwrap_or(0);
                return VmResult {
                    success: true,
                    return_value,
                    trap_code: TrapCode::None,
                    trap_message: String::new(),
                    cycles: self.cycles,
                    output: std::mem::take(&mut self.output),
                    pc: self.pc,
                    stack_depth: self.stack.len(),
                    frame_depth: self.call_stack.len(),
                };
            }
        }
    }

    fn execute_instruction(
        &mut self,
        opcode: Opcode,
        arg1: i32,
        arg2: i32,
    ) -> Result<bool, (TrapCode, String)> {
        match opcode {
            Opcode::LoadConst => {
                self.stack.push(arg1 as i64);
                Ok(true)
            }

            Opcode::LoadLocal => {
                let slot = self.current_local_slot(arg1)?;
                let value = self.locals[slot];
                if value == i64::MIN {
                    return Err((
                        TrapCode::UndefinedLocal,
                        format!("Undefined local at slot {}", arg1),
                    ));
                }
                self.stack.push(value);
                Ok(true)
            }

            Opcode::StoreLocal => {
                let value = self.pop()?;
                let slot = self.current_local_slot(arg1)?;
                self.locals[slot] = value;
                Ok(true)
            }

            Opcode::LoadGlobal => {
                let slot = self.global_slot(arg1)?;
                let value = self.globals[slot];
                self.stack.push(value);
                Ok(true)
            }

            Opcode::StoreGlobal => {
                let slot = self.global_slot(arg1)?;
                let value = self.pop()?;
                self.globals[slot] = value;
                Ok(true)
            }

            Opcode::Add => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Self::normalize_i32(a.wrapping_add(b)));
                Ok(true)
            }

            Opcode::Sub => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Self::normalize_i32(a.wrapping_sub(b)));
                Ok(true)
            }

            Opcode::Mul => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Self::normalize_i32(a.wrapping_mul(b)));
                Ok(true)
            }

            Opcode::Div => {
                let b = self.pop()?;
                let a = self.pop()?;
                if b == 0 {
                    return Err((TrapCode::DivideByZero, "Division by zero".to_string()));
                }
                self.stack.push(a / b);
                Ok(true)
            }

            Opcode::Neg => {
                let a = self.pop()?;
                self.stack.push(-a);
                Ok(true)
            }

            Opcode::Eq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(if a == b { 1 } else { 0 });
                Ok(true)
            }

            Opcode::Ne => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(if a != b { 1 } else { 0 });
                Ok(true)
            }

            Opcode::Lt => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(if a < b { 1 } else { 0 });
                Ok(true)
            }

            Opcode::Gt => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(if a > b { 1 } else { 0 });
                Ok(true)
            }

            Opcode::Le => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(if a <= b { 1 } else { 0 });
                Ok(true)
            }

            Opcode::Ge => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(if a >= b { 1 } else { 0 });
                Ok(true)
            }

            Opcode::And => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(if a != 0 && b != 0 { 1 } else { 0 });
                Ok(true)
            }

            Opcode::Or => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(if a != 0 || b != 0 { 1 } else { 0 });
                Ok(true)
            }

            Opcode::Not => {
                let a = self.pop()?;
                self.stack.push(if a == 0 { 1 } else { 0 });
                Ok(true)
            }

            Opcode::Jump => {
                self.pc = arg1 as usize;
                Ok(false)
            }

            Opcode::JumpIfFalse => {
                let cond = self.pop()?;
                if cond == 0 {
                    self.pc = arg1 as usize;
                    Ok(false)
                } else {
                    Ok(true)
                }
            }

            Opcode::JumpIfTrue => {
                let cond = self.pop()?;
                if cond != 0 {
                    self.pc = arg1 as usize;
                    Ok(false)
                } else {
                    Ok(true)
                }
            }

            Opcode::Call => {
                if arg1 < 0 {
                    return Err((
                        TrapCode::InvalidInstruction,
                        format!("Negative function id {}", arg1),
                    ));
                }
                if arg2 < 0 {
                    return Err((
                        TrapCode::InvalidInstruction,
                        format!("Negative call argument count {}", arg2),
                    ));
                }
                let func_id = arg1 as usize;
                let arg_count = arg2 as usize;

                let func = self
                    .program
                    .functions
                    .get(&func_id)
                    .ok_or((
                        TrapCode::UndefinedFunction,
                        format!("Undefined function {}", func_id),
                    ))?
                    .clone();

                if arg_count > func.local_count {
                    return Err((
                        TrapCode::InvalidInstruction,
                        format!(
                            "Call has {} arguments but function {} has {} local slots",
                            arg_count, func_id, func.local_count
                        ),
                    ));
                }

                // Check stack overflow (max 100 frames per spec)
                if self.call_stack.len() >= Self::MAX_FRAMES {
                    return Err((
                        TrapCode::StackOverflow,
                        format!("Call stack overflow (max {} frames)", Self::MAX_FRAMES),
                    ));
                }

                // Set up new frame
                let new_base = self.call_stack.last().map(|f| f.base_ptr).unwrap_or(0)
                    + self
                        .program
                        .functions
                        .get(&self.call_stack.last().map(|f| f.func_id).unwrap_or(0))
                        .map(|f| f.local_count)
                        .unwrap_or(0);

                let required_locals = new_base.checked_add(func.local_count).ok_or((
                    TrapCode::StackOverflow,
                    "Local storage overflow".to_string(),
                ))?;
                if required_locals > Self::MAX_LOCAL_SLOTS {
                    return Err((
                        TrapCode::StackOverflow,
                        format!(
                            "Local storage overflow (need {} slots, max {})",
                            required_locals,
                            Self::MAX_LOCAL_SLOTS
                        ),
                    ));
                }

                // Copy arguments to new frame's locals
                for i in (0..arg_count).rev() {
                    let arg = self.pop()?;
                    self.locals[new_base + i] = arg;
                }

                // Initialize remaining locals as undefined
                for i in arg_count..func.local_count {
                    self.locals[new_base + i] = i64::MIN;
                }

                self.call_stack.push(CallFrame {
                    return_pc: self.pc + 1,
                    base_ptr: new_base,
                    func_id,
                });

                self.pc = func.entry_pc;
                Ok(false)
            }

            Opcode::Return => {
                let return_value = self.pop()?;
                let frame = self.call_stack.pop().unwrap();

                if frame.return_pc == usize::MAX {
                    // Returning from main
                    self.stack.push(return_value);
                    Ok(false)
                } else {
                    self.stack.push(return_value);
                    self.pc = frame.return_pc;
                    Ok(false)
                }
            }

            Opcode::ArrayLoad => {
                let (base_slot, array_size) = self.global_array_range(arg1, arg2)?;
                let index = Self::array_index(self.pop()?, array_size)?;

                // For global arrays
                let value = self.globals[base_slot + index];
                self.stack.push(value);
                Ok(true)
            }

            Opcode::ArrayStore => {
                let (base_slot, array_size) = self.global_array_range(arg1, arg2)?;
                let value = self.pop()?;
                let index = Self::array_index(self.pop()?, array_size)?;

                // For global arrays
                self.globals[base_slot + index] = value;
                Ok(true)
            }

            Opcode::LocalArrayLoad => {
                let array_size = Self::array_size(arg2)?;
                let index = Self::array_index(self.pop()?, array_size)?;

                // The array reference slot contains the base index into locals
                let ref_slot = self.current_local_slot(arg1)?;
                let array_base = self.local_array_base(ref_slot, array_size)?;
                let value = self.locals[array_base + index];
                self.stack.push(value);
                Ok(true)
            }

            Opcode::LocalArrayStore => {
                let array_size = Self::array_size(arg2)?;
                let value = self.pop()?;
                let index = Self::array_index(self.pop()?, array_size)?;

                // The array reference slot contains the base index into locals
                let ref_slot = self.current_local_slot(arg1)?;
                let array_base = self.local_array_base(ref_slot, array_size)?;
                self.locals[array_base + index] = value;
                Ok(true)
            }

            Opcode::AllocArray | Opcode::ArrayNew => {
                // In the standard VM, allocate contiguous slots in the locals array
                // and push the base address
                let size = Self::array_size(arg1)?;
                // Use a simple bump allocation from the end of locals
                let base = self.locals.len();
                self.locals.extend(std::iter::repeat(0).take(size));
                self.stack.push(base as i64);
                Ok(true)
            }

            Opcode::Print => {
                let value = self.pop()?;
                let msg = format!("OUTPUT: {}", value);
                println!("{}", msg);
                self.output.push(msg);
                Ok(true)
            }

            Opcode::Pop => {
                self.pop()?;
                Ok(true)
            }

            Opcode::Dup => {
                let value = *self
                    .stack
                    .last()
                    .ok_or((TrapCode::StackUnderflow, "Stack underflow".to_string()))?;
                self.stack.push(value);
                Ok(true)
            }

            Opcode::Halt => {
                // Pop return value and exit
                let ret = self.stack.pop().unwrap_or(0);
                self.call_stack.clear();
                self.stack.push(ret);
                Ok(false)
            }
        }
    }

    /// Normalize value to 32-bit signed two's complement (matches Python spec)
    #[inline]
    fn normalize_i32(value: i64) -> i64 {
        let masked = value & 0xFFFFFFFF;
        if masked > 0x7FFFFFFF {
            (masked as i64) - 0x100000000
        } else {
            masked as i64
        }
    }

    #[inline(always)]
    fn pop(&mut self) -> Result<i64, (TrapCode, String)> {
        self.stack
            .pop()
            .ok_or((TrapCode::StackUnderflow, "Stack underflow".to_string()))
    }

    fn current_local_slot(&self, raw_slot: i32) -> Result<usize, (TrapCode, String)> {
        if raw_slot < 0 {
            return Err((
                TrapCode::InvalidInstruction,
                format!("Negative local slot {}", raw_slot),
            ));
        }

        let frame = self.call_stack.last().ok_or((
            TrapCode::InvalidInstruction,
            "No active call frame".to_string(),
        ))?;
        let relative_slot = raw_slot as usize;
        let func = self.program.functions.get(&frame.func_id).ok_or((
            TrapCode::UndefinedFunction,
            format!("Undefined function {}", frame.func_id),
        ))?;

        if relative_slot >= func.local_count {
            return Err((
                TrapCode::InvalidInstruction,
                format!(
                    "Local slot {} out of range for function {} ({} locals)",
                    relative_slot, frame.func_id, func.local_count
                ),
            ));
        }

        let slot = frame
            .base_ptr
            .checked_add(relative_slot)
            .ok_or((TrapCode::StackOverflow, "Local slot overflow".to_string()))?;

        if slot >= Self::MAX_LOCAL_SLOTS {
            return Err((
                TrapCode::StackOverflow,
                format!(
                    "Local storage overflow (slot {}, max {})",
                    slot,
                    Self::MAX_LOCAL_SLOTS
                ),
            ));
        }

        Ok(slot)
    }

    fn global_slot(&self, raw_slot: i32) -> Result<usize, (TrapCode, String)> {
        if raw_slot < 0 {
            return Err((
                TrapCode::InvalidInstruction,
                format!("Negative global slot {}", raw_slot),
            ));
        }

        let slot = raw_slot as usize;
        if slot >= self.globals.len() {
            return Err((
                TrapCode::InvalidInstruction,
                format!(
                    "Global slot {} out of range ({} slots)",
                    slot,
                    self.globals.len()
                ),
            ));
        }

        Ok(slot)
    }

    fn global_array_range(
        &self,
        raw_base_slot: i32,
        raw_array_size: i32,
    ) -> Result<(usize, usize), (TrapCode, String)> {
        let base_slot = self.global_slot(raw_base_slot)?;
        let array_size = Self::array_size(raw_array_size)?;
        let end = base_slot.checked_add(array_size).ok_or((
            TrapCode::InvalidInstruction,
            "Global array range overflow".to_string(),
        ))?;

        if end > self.globals.len() {
            return Err((
                TrapCode::InvalidInstruction,
                format!(
                    "Global array range [{}..{}) exceeds {} global slots",
                    base_slot,
                    end,
                    self.globals.len()
                ),
            ));
        }

        Ok((base_slot, array_size))
    }

    fn array_size(raw_size: i32) -> Result<usize, (TrapCode, String)> {
        if raw_size < 0 {
            return Err((
                TrapCode::InvalidInstruction,
                format!("Negative array size {}", raw_size),
            ));
        }

        Ok(raw_size as usize)
    }

    fn array_index(raw_index: i64, array_size: usize) -> Result<usize, (TrapCode, String)> {
        if raw_index < 0 {
            return Err((
                TrapCode::ArrayOutOfBounds,
                format!(
                    "Array index {} out of bounds (size {})",
                    raw_index, array_size
                ),
            ));
        }

        let index = raw_index as usize;
        if index >= array_size {
            return Err((
                TrapCode::ArrayOutOfBounds,
                format!("Array index {} out of bounds (size {})", index, array_size),
            ));
        }

        Ok(index)
    }

    fn local_array_base(
        &self,
        ref_slot: usize,
        array_size: usize,
    ) -> Result<usize, (TrapCode, String)> {
        let raw_base = self.locals[ref_slot];
        if raw_base == i64::MIN {
            return Err((
                TrapCode::UndefinedLocal,
                format!("Undefined local at slot {}", ref_slot),
            ));
        }

        if raw_base < 0 {
            return Err((
                TrapCode::InvalidInstruction,
                format!("Invalid local array base {}", raw_base),
            ));
        }

        let array_base = raw_base as usize;
        let end = array_base.checked_add(array_size).ok_or((
            TrapCode::InvalidInstruction,
            "Local array range overflow".to_string(),
        ))?;

        if end > self.locals.len() {
            return Err((
                TrapCode::InvalidInstruction,
                format!(
                    "Local array range [{}..{}) exceeds {} local slots",
                    array_base,
                    end,
                    self.locals.len()
                ),
            ));
        }

        Ok(array_base)
    }

    /// Get allocator stats
    pub fn allocator_stats(&self) -> crate::alloc::AllocatorStats {
        self.allocator.stats()
    }

    /// Get GC stats
    pub fn gc_stats(&self) -> &crate::gc::GcStats {
        self.gc.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{CompiledProgram, Compiler, FunctionInfo, Instruction, Opcode};
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::sema::SemanticAnalyzer;
    use std::collections::HashMap;

    fn compile_and_run(source: &str) -> VmResult {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let mut analyzer = SemanticAnalyzer::new();
        analyzer.analyze(&program).unwrap();
        let (compiled, _) = Compiler::new().compile(&program);
        let mut vm = Vm::new(&compiled);
        vm.run()
    }

    fn run_bytecode(instructions: Vec<Instruction>, local_count: usize) -> VmResult {
        let mut functions = HashMap::new();
        functions.insert(
            0,
            FunctionInfo {
                name: "main".to_string(),
                id: 0,
                entry_pc: 0,
                param_count: 0,
                local_count,
            },
        );

        let program = CompiledProgram {
            instructions,
            functions,
            globals: HashMap::new(),
            main_func_id: 0,
            constants: Vec::new(),
        };

        let mut vm = Vm::new(&program);
        vm.run()
    }

    #[test]
    fn test_simple_return() {
        let result = compile_and_run("func main() { return 42; }");
        assert!(result.success);
        assert_eq!(result.return_value, 42);
    }

    #[test]
    fn test_arithmetic() {
        let result = compile_and_run("func main() { return 10 + 20 * 2; }");
        assert!(result.success);
        assert_eq!(result.return_value, 50);
    }

    #[test]
    fn test_function_call() {
        let result = compile_and_run(
            "func add(int a, int b) { return a + b; } func main() { return add(3, 4); }",
        );
        assert!(result.success);
        assert_eq!(result.return_value, 7);
    }

    #[test]
    fn test_recursion() {
        let result = compile_and_run(
            "func fact(int n) { if (n <= 1) { return 1; } return n * fact(n - 1); } func main() { return fact(5); }"
        );
        assert!(result.success);
        assert_eq!(result.return_value, 120);
    }

    #[test]
    fn test_div_by_zero() {
        let result = compile_and_run("func main() { return 10 / 0; }");
        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::DivideByZero);
    }

    #[test]
    fn test_bad_global_array_range_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::ArrayLoad, 999, 1),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_bad_global_slot_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadGlobal, Vm::MAX_GLOBALS as i32, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_negative_call_argument_count_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::Call, 0, -1),
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_negative_array_allocation_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::AllocArray, -1, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_bad_local_array_reference_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, 9999, 0),
                Instruction::new(Opcode::StoreLocal, 0, 0),
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::LocalArrayLoad, 0, 1),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            1,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_undefined_local_array_reference_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::LocalArrayLoad, 0, 1),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            1,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::UndefinedLocal);
    }

    #[test]
    fn test_array_new_legacy_alias_allocates_array() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::ArrayNew, 2, 0),
                Instruction::new(Opcode::StoreLocal, 0, 0),
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::LocalArrayLoad, 0, 2),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            1,
        );

        assert!(result.success);
        assert_eq!(result.return_value, 0);
    }
}
