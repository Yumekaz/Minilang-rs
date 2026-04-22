//! GC-Integrated Virtual Machine for MiniLang bytecode.
//!
//! This VM uses the garbage collector for heap-allocated arrays.
//! Demonstrates real memory management where:
//! - Immediate values (int, bool) stay on the stack
//! - Arrays are heap-allocated and GC-managed
//! - Root tracking prevents premature collection

use crate::alloc::BumpAllocator;
use crate::compiler::{CompiledProgram, Opcode};
use crate::vm::TrapCode;

/// A value that can be either immediate or a heap reference
#[derive(Clone, Copy, Debug)]
pub enum GcValue {
    /// Immediate integer (no GC)
    Int(i64),
    /// Reference to heap-allocated array
    ArrayRef(u32), // Index into heap_arrays
}

impl GcValue {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            GcValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            GcValue::Int(v) => *v != 0,
            GcValue::ArrayRef(_) => true,
        }
    }

    pub fn to_i64(&self) -> i64 {
        match self {
            GcValue::Int(v) => *v,
            GcValue::ArrayRef(id) => *id as i64,
        }
    }
}

/// Heap-allocated array
#[derive(Debug)]
pub struct HeapArray {
    /// Array data
    data: Vec<i64>,
    /// Is this array still alive? (for GC)
    marked: bool,
}

impl HeapArray {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            marked: false,
        }
    }
}

/// Call frame with proper local tracking
#[derive(Debug)]
struct GcCallFrame {
    return_pc: usize,
    /// Local values for this frame
    locals: Vec<GcValue>,
    /// Initialization flags
    init_flags: Vec<bool>,
}

impl GcCallFrame {
    fn new(local_count: usize, return_pc: usize) -> Self {
        Self {
            return_pc,
            locals: vec![GcValue::Int(0); local_count],
            init_flags: vec![false; local_count],
        }
    }

    fn get(&self, slot: usize) -> Option<GcValue> {
        if slot >= self.locals.len() || !self.init_flags[slot] {
            None
        } else {
            Some(self.locals[slot])
        }
    }

    fn set(&mut self, slot: usize, value: GcValue) {
        if slot < self.locals.len() {
            self.locals[slot] = value;
            self.init_flags[slot] = true;
        }
    }

    fn init_param(&mut self, slot: usize, value: GcValue) {
        if slot < self.locals.len() {
            self.locals[slot] = value;
            self.init_flags[slot] = true;
        }
    }
}

/// Execution result
#[derive(Debug)]
pub struct GcVmResult {
    pub success: bool,
    pub return_value: i64,
    pub trap_code: TrapCode,
    pub trap_message: String,
    pub cycles: u64,
    pub output: Vec<String>,
    pub pc: usize,
    pub stack_depth: usize,
    pub frame_depth: usize,
    /// GC statistics
    pub gc_collections: usize,
    pub gc_objects_freed: usize,
    pub heap_arrays_allocated: usize,
}

/// GC-integrated Virtual Machine
pub struct GcVm<'a> {
    program: &'a CompiledProgram,
    /// Operand stack (now holds GcValue)
    stack: Vec<GcValue>,
    /// Global variables
    globals: Vec<GcValue>,
    /// Call stack with per-frame locals
    call_stack: Vec<GcCallFrame>,
    /// Heap-allocated arrays
    heap_arrays: Vec<Option<HeapArray>>,
    /// Free list for array slots
    free_array_slots: Vec<u32>,
    /// Next array ID
    next_array_id: u32,
    /// Program counter
    pc: usize,
    /// Cycle counter
    cycles: u64,
    /// Maximum cycles
    max_cycles: u64,
    /// Output buffer
    output: Vec<String>,
    /// Debug mode
    debug: bool,
    /// GC statistics
    gc_collections: usize,
    gc_objects_freed: usize,
    /// GC threshold (collect when this many arrays allocated)
    gc_threshold: usize,
    /// Allocator for bump allocations
    allocator: BumpAllocator,
}

impl<'a> GcVm<'a> {
    pub const MAX_GLOBALS: usize = 256;
    pub const MAX_FRAMES: usize = 100;
    pub const MAX_OPERAND_STACK: usize = 1000;
    pub const MAX_CYCLES: u64 = 100_000;
    pub const GC_THRESHOLD: usize = 8; // Collect after 8 arrays allocated

    pub fn new(program: &'a CompiledProgram) -> Self {
        let globals_size = program
            .globals
            .values()
            .map(|g| if g.is_array { g.array_size } else { 1 })
            .sum::<usize>()
            .max(Self::MAX_GLOBALS);

        Self {
            program,
            stack: Vec::with_capacity(Self::MAX_OPERAND_STACK),
            globals: vec![GcValue::Int(0); globals_size],
            call_stack: Vec::with_capacity(Self::MAX_FRAMES),
            heap_arrays: Vec::new(),
            free_array_slots: Vec::new(),
            next_array_id: 0,
            pc: 0,
            cycles: 0,
            max_cycles: Self::MAX_CYCLES,
            output: Vec::new(),
            debug: false,
            gc_collections: 0,
            gc_objects_freed: 0,
            gc_threshold: Self::GC_THRESHOLD,
            allocator: BumpAllocator::new(1024 * 1024),
        }
    }

    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    pub fn with_max_cycles(mut self, max: u64) -> Self {
        self.max_cycles = max;
        self
    }

    /// Allocate a heap array
    fn alloc_array(&mut self, size: usize) -> u32 {
        // Check if we need GC
        if self.heap_arrays.len() - self.free_array_slots.len() >= self.gc_threshold {
            self.collect_garbage();
        }

        let array = HeapArray::new(size);

        // Reuse a free slot if available
        if let Some(slot) = self.free_array_slots.pop() {
            self.heap_arrays[slot as usize] = Some(array);
            slot
        } else {
            let id = self.next_array_id;
            self.next_array_id += 1;
            if (id as usize) < self.heap_arrays.len() {
                self.heap_arrays[id as usize] = Some(array);
            } else {
                self.heap_arrays.push(Some(array));
            }
            id
        }
    }

    /// Get array by ID
    fn get_array(&self, id: u32) -> Option<&HeapArray> {
        self.heap_arrays.get(id as usize).and_then(|a| a.as_ref())
    }

    /// Get mutable array by ID
    fn get_array_mut(&mut self, id: u32) -> Option<&mut HeapArray> {
        self.heap_arrays
            .get_mut(id as usize)
            .and_then(|a| a.as_mut())
    }

    /// Mark-sweep garbage collection
    fn collect_garbage(&mut self) {
        self.gc_collections += 1;

        // Clear all marks
        for arr in self.heap_arrays.iter_mut().flatten() {
            arr.marked = false;
        }

        // Collect all array IDs that need marking
        let mut ids_to_mark = Vec::new();

        // Root 1: Stack
        for value in &self.stack {
            if let GcValue::ArrayRef(id) = value {
                ids_to_mark.push(*id);
            }
        }

        // Root 2: Globals
        for value in &self.globals {
            if let GcValue::ArrayRef(id) = value {
                ids_to_mark.push(*id);
            }
        }

        // Root 3: Call frame locals
        for frame in &self.call_stack {
            for (i, value) in frame.locals.iter().enumerate() {
                if frame.init_flags[i] {
                    if let GcValue::ArrayRef(id) = value {
                        ids_to_mark.push(*id);
                    }
                }
            }
        }

        // Mark phase
        for id in ids_to_mark {
            if let Some(arr) = self
                .heap_arrays
                .get_mut(id as usize)
                .and_then(|a| a.as_mut())
            {
                arr.marked = true;
            }
        }

        // Sweep phase: free unmarked arrays
        let mut freed = 0;
        for (i, slot) in self.heap_arrays.iter_mut().enumerate() {
            if let Some(arr) = slot {
                if !arr.marked {
                    *slot = None;
                    self.free_array_slots.push(i as u32);
                    freed += 1;
                }
            }
        }

        self.gc_objects_freed += freed;

        if self.debug && freed > 0 {
            eprintln!("[GC] Collected {} arrays", freed);
        }
    }

    /// Pop value from stack
    fn pop(&mut self) -> Result<GcValue, (TrapCode, String)> {
        self.stack
            .pop()
            .ok_or((TrapCode::StackUnderflow, "Stack underflow".to_string()))
    }

    /// Pop as integer
    fn pop_int(&mut self) -> Result<i64, (TrapCode, String)> {
        let v = self.pop()?;
        Ok(v.to_i64())
    }

    /// 32-bit normalization
    #[inline]
    fn normalize_i32(value: i64) -> i64 {
        let masked = value & 0xFFFFFFFF;
        if masked > 0x7FFFFFFF {
            masked - 0x100000000
        } else {
            masked
        }
    }

    fn make_trap_result(&mut self, trap_code: TrapCode, message: String) -> GcVmResult {
        GcVmResult {
            success: false,
            return_value: 0,
            trap_code,
            trap_message: message,
            cycles: self.cycles,
            output: std::mem::take(&mut self.output),
            pc: self.pc,
            stack_depth: self.stack.len(),
            frame_depth: self.call_stack.len(),
            gc_collections: self.gc_collections,
            gc_objects_freed: self.gc_objects_freed,
            heap_arrays_allocated: self.next_array_id as usize,
        }
    }

    /// Run the program
    pub fn run(&mut self) -> GcVmResult {
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

        // Initialize global arrays on heap
        for info in self.program.globals.values() {
            if info.is_array {
                let array_id = self.alloc_array(info.array_size);
                self.globals[info.slot] = GcValue::ArrayRef(array_id);
            }
        }

        // Set up initial frame
        self.pc = main_func.entry_pc;
        self.call_stack
            .push(GcCallFrame::new(main_func.local_count, usize::MAX));

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

            let instr = &self.program.instructions[self.pc];
            self.cycles += 1;

            if self.debug {
                let stack_str: Vec<String> =
                    self.stack.iter().map(|v| format!("{:?}", v)).collect();
                eprintln!(
                    "[cycle={} pc={}] EXEC {:?} {} {} | stack=[{}]",
                    self.cycles,
                    self.pc,
                    instr.opcode,
                    instr.arg1,
                    instr.arg2,
                    stack_str.join(", ")
                );
            }

            // Execute
            match self.execute_instruction(instr.opcode, instr.arg1, instr.arg2) {
                Ok(true) => self.pc += 1,
                Ok(false) => {}
                Err((trap_code, msg)) => {
                    return self.make_trap_result(trap_code, msg);
                }
            }

            // Check for exit
            if self.call_stack.is_empty() {
                let return_value = self.stack.pop().map(|v| v.to_i64()).unwrap_or(0);
                return GcVmResult {
                    success: true,
                    return_value,
                    trap_code: TrapCode::None,
                    trap_message: String::new(),
                    cycles: self.cycles,
                    output: std::mem::take(&mut self.output),
                    pc: self.pc,
                    stack_depth: self.stack.len(),
                    frame_depth: self.call_stack.len(),
                    gc_collections: self.gc_collections,
                    gc_objects_freed: self.gc_objects_freed,
                    heap_arrays_allocated: self.next_array_id as usize,
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
                self.stack.push(GcValue::Int(arg1 as i64));
                Ok(true)
            }

            Opcode::LoadLocal => {
                let slot = self.current_local_slot(arg1)?;
                let frame = self.current_frame()?;
                match frame.get(slot) {
                    Some(v) => {
                        self.stack.push(v);
                        Ok(true)
                    }
                    None => Err((
                        TrapCode::UndefinedLocal,
                        format!("Undefined local at slot {}", arg1),
                    )),
                }
            }

            Opcode::StoreLocal => {
                let slot = self.current_local_slot(arg1)?;
                let value = self.pop()?;
                let frame = self.current_frame_mut()?;
                frame.set(slot, value);
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
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack
                    .push(GcValue::Int(Self::normalize_i32(a.wrapping_add(b))));
                Ok(true)
            }

            Opcode::Sub => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack
                    .push(GcValue::Int(Self::normalize_i32(a.wrapping_sub(b))));
                Ok(true)
            }

            Opcode::Mul => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack
                    .push(GcValue::Int(Self::normalize_i32(a.wrapping_mul(b))));
                Ok(true)
            }

            Opcode::Div => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                if b == 0 {
                    return Err((TrapCode::DivideByZero, "Division by zero".to_string()));
                }
                self.stack.push(GcValue::Int(Self::normalize_i32(a / b)));
                Ok(true)
            }

            Opcode::Neg => {
                let a = self.pop_int()?;
                self.stack.push(GcValue::Int(Self::normalize_i32(-a)));
                Ok(true)
            }

            Opcode::Eq => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack.push(GcValue::Int(if a == b { 1 } else { 0 }));
                Ok(true)
            }

            Opcode::Ne => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack.push(GcValue::Int(if a != b { 1 } else { 0 }));
                Ok(true)
            }

            Opcode::Lt => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack.push(GcValue::Int(if a < b { 1 } else { 0 }));
                Ok(true)
            }

            Opcode::Gt => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack.push(GcValue::Int(if a > b { 1 } else { 0 }));
                Ok(true)
            }

            Opcode::Le => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack.push(GcValue::Int(if a <= b { 1 } else { 0 }));
                Ok(true)
            }

            Opcode::Ge => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                self.stack.push(GcValue::Int(if a >= b { 1 } else { 0 }));
                Ok(true)
            }

            Opcode::And => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack
                    .push(GcValue::Int(if a.is_truthy() && b.is_truthy() {
                        1
                    } else {
                        0
                    }));
                Ok(true)
            }

            Opcode::Or => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack
                    .push(GcValue::Int(if a.is_truthy() || b.is_truthy() {
                        1
                    } else {
                        0
                    }));
                Ok(true)
            }

            Opcode::Not => {
                let a = self.pop()?;
                self.stack
                    .push(GcValue::Int(if !a.is_truthy() { 1 } else { 0 }));
                Ok(true)
            }

            Opcode::Print => {
                let value = self.pop_int()?;
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
                let value = *self.stack.last().ok_or((
                    TrapCode::StackUnderflow,
                    "Stack underflow on DUP".to_string(),
                ))?;
                self.stack.push(value);
                Ok(true)
            }

            Opcode::Jump => {
                self.pc = arg1 as usize;
                Ok(false)
            }

            Opcode::JumpIfFalse => {
                let cond = self.pop()?;
                if !cond.is_truthy() {
                    self.pc = arg1 as usize;
                    Ok(false)
                } else {
                    Ok(true)
                }
            }

            Opcode::JumpIfTrue => {
                let cond = self.pop()?;
                if cond.is_truthy() {
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

                if self.call_stack.len() >= Self::MAX_FRAMES {
                    return Err((
                        TrapCode::StackOverflow,
                        format!("Call stack overflow (max {} frames)", Self::MAX_FRAMES),
                    ));
                }

                if arg_count > func.local_count {
                    return Err((
                        TrapCode::InvalidInstruction,
                        format!(
                            "Call has {} arguments but function {} has {} local slots",
                            arg_count, func_id, func.local_count
                        ),
                    ));
                }

                // Pop arguments
                let mut args = Vec::with_capacity(arg_count);
                for _ in 0..arg_count {
                    args.push(self.pop()?);
                }
                args.reverse();

                // Create new frame
                let mut new_frame = GcCallFrame::new(func.local_count, self.pc + 1);

                // Initialize parameters
                for (i, arg) in args.into_iter().enumerate() {
                    new_frame.init_param(i, arg);
                }

                // Allocate local arrays on heap
                // (This requires compiler to track which locals are arrays)

                self.call_stack.push(new_frame);
                self.pc = func.entry_pc;
                Ok(false)
            }

            Opcode::Return => {
                let return_value = self.pop()?;
                let frame = self.call_stack.pop().unwrap();

                if frame.return_pc == usize::MAX {
                    // Main returning
                    self.stack.push(return_value);
                    // call_stack is now empty, will exit
                } else {
                    self.pc = frame.return_pc;
                    self.stack.push(return_value);
                }
                Ok(false)
            }

            Opcode::Halt => {
                // Clear call stack to trigger exit
                self.call_stack.clear();
                Ok(false)
            }

            Opcode::ArrayLoad => {
                let (base, size) = self.global_array_range(arg1, arg2)?;
                let index = Self::array_index(self.pop_int()?, size)?;

                // Check if this is a global array (stored as ArrayRef)
                let value = match &self.globals[base] {
                    GcValue::ArrayRef(id) => {
                        if let Some(arr) = self.get_array(*id) {
                            GcValue::Int(*arr.data.get(index).ok_or((
                                TrapCode::ArrayOutOfBounds,
                                format!(
                                    "Array index {} out of bounds (size {})",
                                    index,
                                    arr.data.len()
                                ),
                            ))?)
                        } else {
                            return Err((
                                TrapCode::InvalidInstruction,
                                "Invalid array reference".to_string(),
                            ));
                        }
                    }
                    GcValue::Int(_) => {
                        // Stack-allocated array (legacy path)
                        self.globals[base + index]
                    }
                };

                self.stack.push(value);
                Ok(true)
            }

            Opcode::ArrayStore => {
                let (base, size) = self.global_array_range(arg1, arg2)?;
                let value = self.pop_int()?;
                let index = Self::array_index(self.pop_int()?, size)?;

                // Check if this is a global array
                match self.globals[base] {
                    GcValue::ArrayRef(id) => {
                        if let Some(arr) = self.get_array_mut(id) {
                            let len = arr.data.len();
                            let Some(element) = arr.data.get_mut(index) else {
                                return Err((
                                    TrapCode::ArrayOutOfBounds,
                                    format!("Array index {} out of bounds (size {})", index, len),
                                ));
                            };
                            *element = value;
                        } else {
                            return Err((
                                TrapCode::InvalidInstruction,
                                "Invalid array reference".to_string(),
                            ));
                        }
                    }
                    GcValue::Int(_) => {
                        // Stack-allocated array (legacy path)
                        self.globals[base + index] = GcValue::Int(value);
                    }
                }

                Ok(true)
            }

            Opcode::LocalArrayLoad => {
                let base = self.current_local_slot(arg1)?;
                let size = Self::array_size(arg2)?;
                let index = Self::array_index(self.pop_int()?, size)?;
                let local_value = self.current_frame()?.get(base);
                let value = match local_value {
                    Some(GcValue::ArrayRef(id)) => {
                        if let Some(arr) = self.get_array(id) {
                            GcValue::Int(*arr.data.get(index).ok_or((
                                TrapCode::ArrayOutOfBounds,
                                format!(
                                    "Array index {} out of bounds (size {})",
                                    index,
                                    arr.data.len()
                                ),
                            ))?)
                        } else {
                            return Err((
                                TrapCode::InvalidInstruction,
                                "Invalid array reference".to_string(),
                            ));
                        }
                    }
                    Some(GcValue::Int(v)) => GcValue::Int(v),
                    None => {
                        return Err((
                            TrapCode::UndefinedLocal,
                            format!("Undefined local at slot {}", base),
                        ))
                    }
                };

                self.stack.push(value);
                Ok(true)
            }

            Opcode::LocalArrayStore => {
                let base = self.current_local_slot(arg1)?;
                let size = Self::array_size(arg2)?;
                let value = self.pop_int()?;
                let index = Self::array_index(self.pop_int()?, size)?;
                let local_value = self.current_frame()?.get(base);
                let array_ref = match local_value {
                    Some(GcValue::ArrayRef(id)) => id,
                    _ => {
                        return Err((
                            TrapCode::InvalidInstruction,
                            "Expected array reference".to_string(),
                        ))
                    }
                };

                if let Some(arr) = self.get_array_mut(array_ref) {
                    let len = arr.data.len();
                    let Some(element) = arr.data.get_mut(index) else {
                        return Err((
                            TrapCode::ArrayOutOfBounds,
                            format!("Array index {} out of bounds (size {})", index, len),
                        ));
                    };
                    *element = value;
                } else {
                    return Err((
                        TrapCode::InvalidInstruction,
                        "Invalid array reference".to_string(),
                    ));
                }

                Ok(true)
            }

            Opcode::AllocArray => {
                // Allocate a new array on the heap
                let size = Self::array_size(arg1)?;
                let array_id = self.alloc_array(size);
                self.stack.push(GcValue::ArrayRef(array_id));
                Ok(true)
            }

            Opcode::ArrayNew => {
                // Legacy opcode - same as AllocArray
                let size = Self::array_size(arg1)?;
                let array_id = self.alloc_array(size);
                self.stack.push(GcValue::ArrayRef(array_id));
                Ok(true)
            }
        }
    }

    fn current_frame(&self) -> Result<&GcCallFrame, (TrapCode, String)> {
        self.call_stack.last().ok_or((
            TrapCode::InvalidInstruction,
            "No active call frame".to_string(),
        ))
    }

    fn current_frame_mut(&mut self) -> Result<&mut GcCallFrame, (TrapCode, String)> {
        self.call_stack.last_mut().ok_or((
            TrapCode::InvalidInstruction,
            "No active call frame".to_string(),
        ))
    }

    fn current_local_slot(&self, raw_slot: i32) -> Result<usize, (TrapCode, String)> {
        if raw_slot < 0 {
            return Err((
                TrapCode::InvalidInstruction,
                format!("Negative local slot {}", raw_slot),
            ));
        }

        let slot = raw_slot as usize;
        let frame = self.current_frame()?;
        if slot >= frame.locals.len() {
            return Err((
                TrapCode::InvalidInstruction,
                format!(
                    "Local slot {} out of range ({} locals)",
                    slot,
                    frame.locals.len()
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

    /// Get GC statistics
    pub fn gc_stats(&self) -> String {
        format!(
            "GC Statistics:\n  \
             Collections: {}\n  \
             Objects freed: {}\n  \
             Heap arrays allocated: {}\n  \
             Currently live: {}",
            self.gc_collections,
            self.gc_objects_freed,
            self.next_array_id,
            self.heap_arrays.iter().filter(|a| a.is_some()).count()
        )
    }

    /// Get allocator statistics
    pub fn allocator_stats(&self) -> String {
        format!("{}", self.allocator.stats())
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

    fn compile_and_run_gc(source: &str) -> GcVmResult {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let mut analyzer = SemanticAnalyzer::new();
        analyzer.analyze(&program).unwrap();
        let (compiled, _) = Compiler::new().compile(&program);
        let mut vm = GcVm::new(&compiled);
        vm.run()
    }

    fn run_bytecode(instructions: Vec<Instruction>, local_count: usize) -> GcVmResult {
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

        let mut vm = GcVm::new(&program);
        vm.run()
    }

    #[test]
    fn test_gc_vm_basic() {
        let result = compile_and_run_gc("func main() { return 42; }");
        assert!(result.success);
        assert_eq!(result.return_value, 42);
    }

    #[test]
    fn test_gc_vm_arithmetic() {
        let result = compile_and_run_gc("func main() { return 10 + 20 * 2; }");
        assert!(result.success);
        assert_eq!(result.return_value, 50);
    }

    #[test]
    fn test_gc_vm_locals() {
        let result = compile_and_run_gc("func main() { int x = 5; int y = 10; return x + y; }");
        assert!(result.success);
        assert_eq!(result.return_value, 15);
    }

    #[test]
    fn test_gc_vm_function_call() {
        let result = compile_and_run_gc(
            "func add(int a, int b) { return a + b; } func main() { return add(3, 4); }",
        );
        assert!(result.success);
        assert_eq!(result.return_value, 7);
    }

    #[test]
    fn test_gc_vm_undefined_local() {
        let result = compile_and_run_gc("func main() { int x; return x; }");
        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::UndefinedLocal);
    }

    #[test]
    fn test_gc_vm_global_array() {
        let result = compile_and_run_gc(
            "int arr[5]; func main() { arr[0] = 10; arr[1] = 20; return arr[0] + arr[1]; }",
        );
        assert!(result.success);
        assert_eq!(result.return_value, 30);
        // Check that GC allocated the array
        assert!(result.heap_arrays_allocated > 0);
    }

    #[test]
    fn test_gc_vm_bad_global_slot_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadGlobal, GcVm::MAX_GLOBALS as i32, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_gc_vm_bad_global_array_range_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::ArrayLoad, GcVm::MAX_GLOBALS as i32, 1),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_gc_vm_bad_local_slot_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, 1, 0),
                Instruction::new(Opcode::StoreLocal, -1, 0),
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            1,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_gc_vm_too_many_call_args_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::LoadConst, 1, 0),
                Instruction::new(Opcode::Call, 0, 1),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }

    #[test]
    fn test_gc_vm_negative_array_allocation_traps() {
        let result = run_bytecode(
            vec![
                Instruction::new(Opcode::ArrayNew, -1, 0),
                Instruction::new(Opcode::Return, 0, 0),
            ],
            0,
        );

        assert!(!result.success);
        assert_eq!(result.trap_code, TrapCode::InvalidInstruction);
    }
}
