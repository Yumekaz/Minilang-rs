//! Virtual Machine (Interpreter) for MiniLang bytecode.
//!
//! A stack-based VM that executes bytecode instructions.
//! Used as fallback when JIT is not available and for debugging.

use crate::alloc::BumpAllocator;
use crate::compiler::{CompiledProgram, GlobalInfo, Opcode};
use crate::gc::GarbageCollector;
use crate::limits;
use crate::trace::{events_to_json, TraceEvent, TraceOutcome};

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
    /// Optional execution trace
    trace: Option<Vec<TraceEvent>>,
}

struct TraceStep {
    pc: usize,
    opcode: Opcode,
    arg1: i32,
    arg2: i32,
    stack_before: Option<Vec<i64>>,
    frame_depth_before: usize,
}

/// VM limits matching Python spec
impl<'a> Vm<'a> {
    pub const MAX_GLOBALS: usize = limits::MAX_GLOBAL_SLOTS;
    pub const MAX_LOCAL_SLOTS: usize = limits::MAX_LOCAL_SLOTS;
    pub const MAX_FRAMES: usize = limits::MAX_FRAMES;
    pub const MAX_OPERAND_STACK: usize = limits::MAX_OPERAND_STACK;
    pub const MAX_CYCLES: u64 = limits::MAX_CYCLES;
    pub const MAX_INSTRUCTIONS: usize = limits::MAX_INSTRUCTIONS;

    /// Create a new VM for the given program
    pub fn new(program: &'a CompiledProgram) -> Self {
        Self {
            program,
            stack: Vec::with_capacity(Self::MAX_OPERAND_STACK),
            locals: vec![i64::MIN; Self::MAX_LOCAL_SLOTS], // Use MIN as "undefined" marker
            globals: vec![0; Self::MAX_GLOBALS],           // Initialized to 0 per spec
            call_stack: Vec::with_capacity(Self::MAX_FRAMES),
            pc: 0,
            cycles: 0,
            max_cycles: Self::MAX_CYCLES,
            output: Vec::new(),
            debug: false,
            allocator: BumpAllocator::new(1024 * 1024), // 1MB
            gc: GarbageCollector::new(512 * 1024),      // 512KB threshold
            trace: None,
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

    /// Enable execution tracing.
    pub fn with_trace(mut self) -> Self {
        self.trace = Some(Vec::new());
        self
    }

    /// Get recorded trace events.
    pub fn trace_events(&self) -> &[TraceEvent] {
        match &self.trace {
            Some(events) => events,
            None => &[],
        }
    }

    /// Serialize recorded trace events to JSON.
    pub fn trace_json(&self) -> String {
        events_to_json("VM", self.trace_events())
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
        if let Err((trap_code, msg)) = self.validate_program() {
            return self.make_trap_result(trap_code, msg);
        }

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
            let trace_step = TraceStep {
                pc: self.pc,
                opcode: instr.opcode,
                arg1: instr.arg1,
                arg2: instr.arg2,
                stack_before: self.trace.as_ref().map(|_| self.stack.clone()),
                frame_depth_before: self.call_stack.len(),
            };
            let opcode = trace_step.opcode;
            let arg1 = trace_step.arg1;
            let arg2 = trace_step.arg2;
            self.cycles += 1;

            if self.debug {
                let stack_before: Vec<String> = self.stack.iter().map(|v| v.to_string()).collect();
                eprintln!(
                    "[cycle={} pc={}] EXEC {:?} {} {} | stack=[{}]",
                    self.cycles,
                    self.pc,
                    opcode,
                    arg1,
                    arg2,
                    stack_before.join(", ")
                );
            }

            // Execute
            let mut outcome = TraceOutcome::Continue;
            match self.execute_instruction(opcode, arg1, arg2) {
                Ok(true) => {
                    // Normal execution, continue
                    self.pc += 1;
                }
                Ok(false) => {
                    // Instruction handled PC change
                    outcome = TraceOutcome::Jump;
                }
                Err((trap_code, msg)) => {
                    self.record_trace(
                        trace_step,
                        TraceOutcome::Trap {
                            code: trap_code,
                            message: msg.clone(),
                        },
                    );
                    return self.make_trap_result(trap_code, msg);
                }
            }

            // Check for exit
            if self.call_stack.is_empty() {
                self.record_trace(trace_step, TraceOutcome::Exit);

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

            self.record_trace(trace_step, outcome);
        }
    }

    fn record_trace(&mut self, step: TraceStep, outcome: TraceOutcome) {
        let Some(events) = self.trace.as_mut() else {
            return;
        };

        events.push(TraceEvent {
            cycle: self.cycles,
            pc: step.pc,
            opcode: format!("{:?}", step.opcode),
            arg1: step.arg1,
            arg2: step.arg2,
            stack_before: step.stack_before.unwrap_or_default(),
            stack_after: self.stack.clone(),
            frame_depth_before: step.frame_depth_before,
            frame_depth_after: self.call_stack.len(),
            next_pc: self.pc,
            outcome,
        });
    }

    fn validate_program(&self) -> Result<(), (TrapCode, String)> {
        if self.program.instructions.len() > Self::MAX_INSTRUCTIONS {
            return Err((
                TrapCode::InvalidInstruction,
                format!(
                    "Instruction count {} exceeds limit {}",
                    self.program.instructions.len(),
                    Self::MAX_INSTRUCTIONS
                ),
            ));
        }

        for func in self.program.functions.values() {
            if func.param_count > func.local_count {
                return Err((
                    TrapCode::InvalidInstruction,
                    format!(
                        "Function '{}' has {} parameters but only {} local slots",
                        func.name, func.param_count, func.local_count
                    ),
                ));
            }

            if func.local_count > Self::MAX_LOCAL_SLOTS {
                return Err((
                    TrapCode::StackOverflow,
                    format!(
                        "Function '{}' needs {} local slots, max {}",
                        func.name,
                        func.local_count,
                        Self::MAX_LOCAL_SLOTS
                    ),
                ));
            }
        }

        let global_slots = Self::required_global_slots(self.program.globals.values())?;
        if global_slots > Self::MAX_GLOBALS {
            return Err((
                TrapCode::InvalidInstruction,
                format!(
                    "Global storage exceeds {} slots (needs {})",
                    Self::MAX_GLOBALS,
                    global_slots
                ),
            ));
        }

        Ok(())
    }

    fn required_global_slots<'g>(
        globals: impl Iterator<Item = &'g GlobalInfo>,
    ) -> Result<usize, (TrapCode, String)> {
        let mut max_slot = 0usize;

        for info in globals {
            let width = if info.is_array {
                if info.array_size == 0 {
                    return Err((
                        TrapCode::InvalidInstruction,
                        format!("Global array '{}' has zero size", info.name),
                    ));
                }
                info.array_size
            } else {
                1
            };

            let end = info.slot.checked_add(width).ok_or((
                TrapCode::InvalidInstruction,
                "Global storage range overflow".to_string(),
            ))?;
            max_slot = max_slot.max(end);
        }

        Ok(max_slot)
    }

    fn execute_instruction(
        &mut self,
        opcode: Opcode,
        arg1: i32,
        arg2: i32,
    ) -> Result<bool, (TrapCode, String)> {
        match opcode {
            Opcode::LoadConst => {
                self.push(arg1 as i64)?;
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
                self.push(value)?;
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
                self.push(value)?;
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
                self.push(Self::normalize_i32(a.wrapping_add(b)))?;
                Ok(true)
            }

            Opcode::Sub => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(Self::normalize_i32(a.wrapping_sub(b)))?;
                Ok(true)
            }

            Opcode::Mul => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(Self::normalize_i32(a.wrapping_mul(b)))?;
                Ok(true)
            }

            Opcode::Div => {
                let b = self.pop()?;
                let a = self.pop()?;
                if b == 0 {
                    return Err((TrapCode::DivideByZero, "Division by zero".to_string()));
                }
                self.push(Self::normalize_i32(a / b))?;
                Ok(true)
            }

            Opcode::Neg => {
                let a = self.pop()?;
                self.push(Self::normalize_i32(-a))?;
                Ok(true)
            }

            Opcode::Eq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a == b { 1 } else { 0 })?;
                Ok(true)
            }

            Opcode::Ne => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a != b { 1 } else { 0 })?;
                Ok(true)
            }

            Opcode::Lt => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a < b { 1 } else { 0 })?;
                Ok(true)
            }

            Opcode::Gt => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a > b { 1 } else { 0 })?;
                Ok(true)
            }

            Opcode::Le => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a <= b { 1 } else { 0 })?;
                Ok(true)
            }

            Opcode::Ge => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a >= b { 1 } else { 0 })?;
                Ok(true)
            }

            Opcode::And => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a != 0 && b != 0 { 1 } else { 0 })?;
                Ok(true)
            }

            Opcode::Or => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a != 0 || b != 0 { 1 } else { 0 })?;
                Ok(true)
            }

            Opcode::Not => {
                let a = self.pop()?;
                self.push(if a == 0 { 1 } else { 0 })?;
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

                if arg_count != func.param_count {
                    return Err((
                        TrapCode::InvalidInstruction,
                        format!(
                            "Call has {} arguments but function {} expects {}",
                            arg_count, func_id, func.param_count
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
                    self.push(return_value)?;
                    Ok(false)
                } else {
                    self.push(return_value)?;
                    self.pc = frame.return_pc;
                    Ok(false)
                }
            }

            Opcode::ArrayLoad => {
                let (base_slot, array_size) = self.global_array_range(arg1, arg2)?;
                let index = Self::array_index(self.pop()?, array_size)?;

                // For global arrays
                let value = self.globals[base_slot + index];
                self.push(value)?;
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
                self.push(value)?;
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
                self.locals.extend(std::iter::repeat_n(0, size));
                self.push(base as i64)?;
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
                self.push(value)?;
                Ok(true)
            }

            Opcode::Halt => {
                // Pop return value and exit
                let ret = self.stack.pop().unwrap_or(0);
                self.call_stack.clear();
                self.push(ret)?;
                Ok(false)
            }
        }
    }

    /// Normalize value to 32-bit signed two's complement (matches Python spec)
    #[inline]
    fn normalize_i32(value: i64) -> i64 {
        let masked = value & 0xFFFFFFFF;
        if masked > 0x7FFFFFFF {
            masked - 0x100000000
        } else {
            masked
        }
    }

    #[inline(always)]
    fn pop(&mut self) -> Result<i64, (TrapCode, String)> {
        self.stack
            .pop()
            .ok_or((TrapCode::StackUnderflow, "Stack underflow".to_string()))
    }

    fn push(&mut self, value: i64) -> Result<(), (TrapCode, String)> {
        if self.stack.len() >= Self::MAX_OPERAND_STACK {
            return Err((
                TrapCode::StackOverflow,
                format!(
                    "Operand stack overflow (max {} entries)",
                    Self::MAX_OPERAND_STACK
                ),
            ));
        }

        self.stack.push(value);
        Ok(())
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
    use crate::compiler::{
        CompiledProgram, Compiler, FunctionInfo, GlobalInfo, Instruction, Opcode,
    };
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

    fn make_function(
        id: usize,
        name: &str,
        entry_pc: usize,
        param_count: usize,
        local_count: usize,
    ) -> FunctionInfo {
        FunctionInfo {
            name: name.to_string(),
            id,
            entry_pc,
            param_count,
            local_count,
        }
    }

    #[test]
    fn test_simple_return() {
        let result = compile_and_run("func main() { return 42; }");
        assert!(result.success);
        assert_eq!(result.return_value, 42);
    }

    #[test]
    fn test_trace_records_instruction_events() {
        let mut lexer = Lexer::new("func main() { return 42; }");
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let mut analyzer = SemanticAnalyzer::new();
        analyzer.analyze(&program).unwrap();
        let (compiled, _) = Compiler::new().compile(&program);

        let mut vm = Vm::new(&compiled).with_trace();
        let result = vm.run();

        assert!(result.success);
        assert!(!vm.trace_events().is_empty());
        assert!(vm.trace_json().contains("\"backend\": \"VM\""));
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
    fn test_program_global_metadata_over_limit_traps() {
        let mut functions = HashMap::new();
        functions.insert(0, make_function(0, "main", 0, 0, 0));

        let mut globals = HashMap::new();
        globals.insert(
            "g".to_string(),
            GlobalInfo {
                name: "g".to_string(),
                slot: Vm::MAX_GLOBALS,
                is_array: false,
                array_size: 0,
            },
        );

        let program = CompiledProgram {
            instructions: vec![
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            functions,
            globals,
            main_func_id: 0,
            constants: Vec::new(),
        };

        let mut vm = Vm::new(&program);
        let result = vm.run();
        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_instruction_count_limit_traps() {
        let result = run_bytecode(
            vec![Instruction::new(Opcode::LoadConst, 0, 0); Vm::MAX_INSTRUCTIONS + 1],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_operand_stack_overflow_traps() {
        let result = run_bytecode(
            vec![Instruction::new(Opcode::LoadConst, 0, 0); Vm::MAX_OPERAND_STACK + 1],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::StackOverflow);
    }

    #[test]
    fn test_function_local_limit_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            Vm::MAX_LOCAL_SLOTS + 1,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::StackOverflow);
    }

    #[test]
    fn test_call_with_too_few_args_traps() {
        let mut functions = HashMap::new();
        functions.insert(0, make_function(0, "main", 0, 0, 0));
        functions.insert(1, make_function(1, "f", 2, 1, 2));

        let program = CompiledProgram {
            instructions: vec![
                Instruction::new(Opcode::Call, 1, 0),
                Instruction::new(Opcode::Return, 0, 0),
                Instruction::new(Opcode::LoadConst, 7, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            functions,
            globals: HashMap::new(),
            main_func_id: 0,
            constants: Vec::new(),
        };

        let mut vm = Vm::new(&program);
        let result = vm.run();
        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_call_with_extra_args_within_local_count_traps() {
        let mut functions = HashMap::new();
        functions.insert(0, make_function(0, "main", 0, 0, 0));
        functions.insert(1, make_function(1, "f", 4, 1, 2));

        let program = CompiledProgram {
            instructions: vec![
                Instruction::new(Opcode::LoadConst, 1, 0),
                Instruction::new(Opcode::LoadConst, 2, 0),
                Instruction::new(Opcode::Call, 1, 2),
                Instruction::new(Opcode::Return, 0, 0),
                Instruction::new(Opcode::LoadConst, 7, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            functions,
            globals: HashMap::new(),
            main_func_id: 0,
            constants: Vec::new(),
        };

        let mut vm = Vm::new(&program);
        let result = vm.run();
        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_neg_wraps_i32_min() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, i32::MIN, 0),
                Instruction::new(Opcode::Neg, 0, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(result.success);
        assert_eq!(result.return_value, i32::MIN as i64);
    }

    #[test]
    fn test_div_wraps_i32_min_by_minus_one() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, i32::MIN, 0),
                Instruction::new(Opcode::LoadConst, -1, 0),
                Instruction::new(Opcode::Div, 0, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(result.success);
        assert_eq!(result.return_value, i32::MIN as i64);
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
