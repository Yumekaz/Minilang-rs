//! Bytecode IR and compiler for MiniLang.
//!
//! Compiles AST to stack-based bytecode that can be:
//! 1. Interpreted by the VM
//! 2. JIT compiled to native x86-64 code

use crate::ast::*;
use std::collections::HashMap;

/// Bytecode opcodes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    /// Push a constant onto the stack
    LoadConst = 0,
    /// Load a local variable
    LoadLocal = 1,
    /// Store to a local variable
    StoreLocal = 2,
    /// Load a global variable
    LoadGlobal = 3,
    /// Store to a global variable
    StoreGlobal = 4,

    /// Arithmetic operations
    Add = 10,
    Sub = 11,
    Mul = 12,
    Div = 13,
    Neg = 14,

    /// Comparison operations
    Eq = 20,
    Ne = 21,
    Lt = 22,
    Gt = 23,
    Le = 24,
    Ge = 25,

    /// Logical operations
    And = 30,
    Or = 31,
    Not = 32,

    /// Control flow
    Jump = 40,
    JumpIfFalse = 41,
    JumpIfTrue = 42,

    /// Function operations
    Call = 50,
    Return = 51,

    /// Array operations
    ArrayLoad = 60,
    ArrayStore = 61,
    ArrayNew = 62,
    LocalArrayLoad = 63,
    LocalArrayStore = 64,
    AllocArray = 65,

    /// Misc
    Print = 70,
    Pop = 71,
    Dup = 72,
    Halt = 73,
}

/// A single bytecode instruction
#[derive(Debug, Clone)]
pub struct Instruction {
    pub opcode: Opcode,
    pub arg1: i32,
    pub arg2: i32,
}

impl Instruction {
    pub fn new(opcode: Opcode, arg1: i32, arg2: i32) -> Self {
        Self { opcode, arg1, arg2 }
    }

    pub fn simple(opcode: Opcode) -> Self {
        Self::new(opcode, 0, 0)
    }
}

/// Function metadata
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    pub name: String,
    pub id: usize,
    pub entry_pc: usize,
    pub param_count: usize,
    pub local_count: usize,
}

/// Global variable metadata
#[derive(Debug, Clone)]
pub struct GlobalInfo {
    pub name: String,
    pub slot: usize,
    pub is_array: bool,
    pub array_size: usize,
}

/// Compiled program
#[derive(Debug, Clone)]
pub struct CompiledProgram {
    pub instructions: Vec<Instruction>,
    pub functions: HashMap<usize, FunctionInfo>,
    pub globals: HashMap<String, GlobalInfo>,
    pub main_func_id: usize,
    pub constants: Vec<i32>,
}

/// Local variable during compilation
#[derive(Debug, Clone)]
struct LocalVar {
    slot: usize,
    is_array: bool,
    array_size: usize,
}

/// Bytecode compiler
pub struct Compiler {
    instructions: Vec<Instruction>,
    functions: HashMap<usize, FunctionInfo>,
    globals: HashMap<String, GlobalInfo>,
    func_name_to_id: HashMap<String, usize>,

    // Current function state
    current_locals: HashMap<String, LocalVar>,
    next_local_slot: usize,

    // Global allocation
    next_global_slot: usize,
    next_func_id: usize,
    main_func_id: Option<usize>,

    // Constants pool
    constants: Vec<i32>,

    // Arena allocator for string interning during compilation
    string_arena: crate::alloc::BumpAllocator,
    interned_strings: usize, // Count of interned strings
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            functions: HashMap::new(),
            globals: HashMap::new(),
            func_name_to_id: HashMap::new(),
            current_locals: HashMap::new(),
            next_local_slot: 0,
            next_global_slot: 0,
            next_func_id: 0,
            main_func_id: None,
            constants: Vec::new(),
            string_arena: crate::alloc::BumpAllocator::new(64 * 1024), // 64KB for strings
            interned_strings: 0,
        }
    }

    /// Compile a program to bytecode
    /// Returns (CompiledProgram, arena_stats)
    pub fn compile(mut self, program: &Program) -> (CompiledProgram, crate::alloc::AllocatorStats) {
        // Pass 1: Collect globals and register functions
        self.collect_globals(program);
        self.register_functions(program);

        // Pass 2: Compile function bodies
        for func in &program.functions {
            self.compile_function(func, program);
        }

        let stats = self.string_arena.stats();

        (
            CompiledProgram {
                instructions: self.instructions,
                functions: self.functions,
                globals: self.globals,
                main_func_id: self.main_func_id.unwrap_or(0),
                constants: self.constants,
            },
            stats,
        )
    }

    /// Intern a string in the arena (returns pointer for deduplication)
    fn intern_string(&mut self, s: &str) -> usize {
        // Allocate string in arena
        if let Some(ptr) = self.string_arena.alloc(s.len()) {
            unsafe {
                std::ptr::copy_nonoverlapping(s.as_ptr(), ptr.as_ptr(), s.len());
            }
            self.interned_strings += 1;
            ptr.as_ptr() as usize
        } else {
            0 // Arena full, fall back to no interning
        }
    }

    /// Get arena allocator statistics
    pub fn arena_stats(&self) -> crate::alloc::AllocatorStats {
        self.string_arena.stats()
    }

    fn emit(&mut self, opcode: Opcode, arg1: i32, arg2: i32) -> usize {
        let pc = self.instructions.len();
        self.instructions.push(Instruction::new(opcode, arg1, arg2));
        pc
    }

    fn emit_simple(&mut self, opcode: Opcode) -> usize {
        self.emit(opcode, 0, 0)
    }

    fn patch(&mut self, pc: usize, arg1: i32) {
        self.instructions[pc].arg1 = arg1;
    }

    fn current_pc(&self) -> usize {
        self.instructions.len()
    }

    fn collect_globals(&mut self, program: &Program) {
        for glob in &program.globals {
            let (slot, size) = if let Some(arr_size) = glob.array_size {
                let slot = self.next_global_slot;
                self.next_global_slot += arr_size as usize;
                (slot, arr_size as usize)
            } else {
                let slot = self.next_global_slot;
                self.next_global_slot += 1;
                (slot, 0)
            };

            // Intern global name in arena
            self.intern_string(&glob.name);

            self.globals.insert(
                glob.name.clone(),
                GlobalInfo {
                    name: glob.name.clone(),
                    slot,
                    is_array: glob.array_size.is_some(),
                    array_size: size,
                },
            );
        }
    }

    fn register_functions(&mut self, program: &Program) {
        for func in &program.functions {
            let id = self.next_func_id;
            self.next_func_id += 1;

            // Intern function name in arena
            self.intern_string(&func.name);

            self.func_name_to_id.insert(func.name.clone(), id);

            if func.name == "main" {
                self.main_func_id = Some(id);
            }
        }
    }

    fn compile_function(&mut self, func: &Function, program: &Program) {
        let func_id = self.func_name_to_id[&func.name];
        let entry_pc = self.current_pc();

        // Reset local state
        self.current_locals.clear();
        self.next_local_slot = 0;

        // Allocate parameter slots
        for param in &func.params {
            let local = LocalVar {
                slot: self.next_local_slot,
                is_array: false,
                array_size: 0,
            };
            self.current_locals.insert(param.name.clone(), local);
            self.next_local_slot += 1;
        }

        // First pass: collect local declarations
        self.collect_local_decls(&func.body);
        let local_count = self.next_local_slot;

        // Reset for compilation (keep params)
        self.next_local_slot = func.params.len();
        self.current_locals
            .retain(|_, v| v.slot < func.params.len());

        // If this is main, compile global initializers first
        if func.name == "main" {
            for glob in &program.globals {
                if let Some(ref init) = glob.init_expr {
                    self.compile_expr(init);
                    let slot = self.globals[&glob.name].slot;
                    self.emit(Opcode::StoreGlobal, slot as i32, 0);
                }
            }
        }

        // Compile body
        for stmt in &func.body {
            self.compile_stmt(stmt);
        }

        // Implicit return 0
        self.emit(Opcode::LoadConst, 0, 0);
        self.emit_simple(Opcode::Return);

        // Intern function name in arena
        self.intern_string(&func.name);

        // Register function
        self.functions.insert(
            func_id,
            FunctionInfo {
                name: func.name.clone(),
                id: func_id,
                entry_pc,
                param_count: func.params.len(),
                local_count,
            },
        );
    }

    fn collect_local_decls(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::VarDecl {
                    name, array_size, ..
                } if !self.current_locals.contains_key(name) => {
                    // Arrays only need 1 slot for the reference
                    let slot = self.next_local_slot;
                    self.next_local_slot += 1;
                    let size = array_size.map(|s| s as usize).unwrap_or(0);

                    self.current_locals.insert(
                        name.clone(),
                        LocalVar {
                            slot,
                            is_array: array_size.is_some(),
                            array_size: size,
                        },
                    );
                }
                Stmt::VarDecl { .. } => {}
                Stmt::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    self.collect_local_decls(then_body);
                    if let Some(else_stmts) = else_body {
                        self.collect_local_decls(else_stmts);
                    }
                }
                Stmt::While { body, .. } => {
                    self.collect_local_decls(body);
                }
                _ => {}
            }
        }
    }

    fn compile_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::VarDecl {
                name,
                init_expr,
                array_size,
                ..
            } => {
                // Ensure local is allocated
                if !self.current_locals.contains_key(name) {
                    let (slot, size) = if let Some(arr_size) = array_size {
                        // For arrays, just need one slot to hold the array reference
                        let slot = self.next_local_slot;
                        self.next_local_slot += 1; // Only 1 slot for the reference
                        (slot, *arr_size as usize)
                    } else {
                        let slot = self.next_local_slot;
                        self.next_local_slot += 1;
                        (slot, 0)
                    };

                    self.current_locals.insert(
                        name.clone(),
                        LocalVar {
                            slot,
                            is_array: array_size.is_some(),
                            array_size: size,
                        },
                    );
                }

                let local = self.current_locals[name].clone();

                if local.is_array {
                    // Emit AllocArray instruction to heap-allocate
                    self.emit(Opcode::AllocArray, local.array_size as i32, 0);
                    self.emit(Opcode::StoreLocal, local.slot as i32, 0);
                } else if let Some(init) = init_expr {
                    self.compile_expr(init);
                    self.emit(Opcode::StoreLocal, local.slot as i32, 0);
                }
            }

            Stmt::Assign {
                target,
                index_expr,
                value,
                ..
            } => {
                if let Some(local) = self.current_locals.get(target).cloned() {
                    if let Some(idx_expr) = index_expr {
                        // Local array store: arr[idx] = value
                        self.compile_expr(idx_expr);
                        self.compile_expr(value);
                        self.emit(
                            Opcode::LocalArrayStore,
                            local.slot as i32,
                            local.array_size as i32,
                        );
                    } else {
                        self.compile_expr(value);
                        self.emit(Opcode::StoreLocal, local.slot as i32, 0);
                    }
                } else if let Some(global) = self.globals.get(target).cloned() {
                    if let Some(idx_expr) = index_expr {
                        // Global array store
                        self.compile_expr(idx_expr);
                        self.compile_expr(value);
                        self.emit(
                            Opcode::ArrayStore,
                            global.slot as i32,
                            global.array_size as i32,
                        );
                    } else {
                        self.compile_expr(value);
                        self.emit(Opcode::StoreGlobal, global.slot as i32, 0);
                    }
                }
            }

            Stmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                self.compile_expr(condition);
                let jump_to_else = self.emit(Opcode::JumpIfFalse, 0, 0);

                for s in then_body {
                    self.compile_stmt(s);
                }

                if let Some(else_stmts) = else_body {
                    let jump_to_end = self.emit(Opcode::Jump, 0, 0);
                    let else_pc = self.current_pc();
                    self.patch(jump_to_else, else_pc as i32);

                    for s in else_stmts {
                        self.compile_stmt(s);
                    }

                    let end_pc = self.current_pc();
                    self.patch(jump_to_end, end_pc as i32);
                } else {
                    let end_pc = self.current_pc();
                    self.patch(jump_to_else, end_pc as i32);
                }
            }

            Stmt::While {
                condition, body, ..
            } => {
                let loop_pc = self.current_pc();
                self.compile_expr(condition);
                let jump_to_exit = self.emit(Opcode::JumpIfFalse, 0, 0);

                for s in body {
                    self.compile_stmt(s);
                }

                self.emit(Opcode::Jump, loop_pc as i32, 0);
                let exit_pc = self.current_pc();
                self.patch(jump_to_exit, exit_pc as i32);
            }

            Stmt::Return { value, .. } => {
                self.compile_expr(value);
                self.emit_simple(Opcode::Return);
            }

            Stmt::Print { value, .. } => {
                self.compile_expr(value);
                self.emit_simple(Opcode::Print);
            }

            Stmt::ExprStmt { expr, .. } => {
                self.compile_expr(expr);
                self.emit_simple(Opcode::Pop);
            }
        }
    }

    fn compile_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::IntLiteral { value, .. } => {
                self.emit(Opcode::LoadConst, *value, 0);
            }

            Expr::BoolLiteral { value, .. } => {
                self.emit(Opcode::LoadConst, if *value { 1 } else { 0 }, 0);
            }

            Expr::Identifier { name, .. } => {
                if let Some(local) = self.current_locals.get(name) {
                    self.emit(Opcode::LoadLocal, local.slot as i32, 0);
                } else if let Some(global) = self.globals.get(name) {
                    self.emit(Opcode::LoadGlobal, global.slot as i32, 0);
                }
            }

            Expr::Binary {
                op, left, right, ..
            } => {
                // Handle short-circuit for And/Or
                match op {
                    BinaryOp::And => {
                        self.compile_expr(left);
                        let jump_short = self.emit(Opcode::JumpIfFalse, 0, 0);
                        self.compile_expr(right);
                        self.emit(Opcode::LoadConst, 0, 0);
                        self.emit_simple(Opcode::Ne);
                        let jump_end = self.emit(Opcode::Jump, 0, 0);
                        let false_pc = self.current_pc();
                        self.patch(jump_short, false_pc as i32);
                        self.emit(Opcode::LoadConst, 0, 0);
                        let end_pc = self.current_pc();
                        self.patch(jump_end, end_pc as i32);
                        return;
                    }
                    BinaryOp::Or => {
                        self.compile_expr(left);
                        let jump_short = self.emit(Opcode::JumpIfTrue, 0, 0);
                        self.compile_expr(right);
                        self.emit(Opcode::LoadConst, 0, 0);
                        self.emit_simple(Opcode::Ne);
                        let jump_end = self.emit(Opcode::Jump, 0, 0);
                        let true_pc = self.current_pc();
                        self.patch(jump_short, true_pc as i32);
                        self.emit(Opcode::LoadConst, 1, 0);
                        let end_pc = self.current_pc();
                        self.patch(jump_end, end_pc as i32);
                        return;
                    }
                    _ => {}
                }

                self.compile_expr(left);
                self.compile_expr(right);

                let opcode = match op {
                    BinaryOp::Add => Opcode::Add,
                    BinaryOp::Sub => Opcode::Sub,
                    BinaryOp::Mul => Opcode::Mul,
                    BinaryOp::Div => Opcode::Div,
                    BinaryOp::Eq => Opcode::Eq,
                    BinaryOp::Ne => Opcode::Ne,
                    BinaryOp::Lt => Opcode::Lt,
                    BinaryOp::Gt => Opcode::Gt,
                    BinaryOp::Le => Opcode::Le,
                    BinaryOp::Ge => Opcode::Ge,
                    BinaryOp::And | BinaryOp::Or => unreachable!(),
                };
                self.emit_simple(opcode);
            }

            Expr::Unary { op, operand, .. } => {
                self.compile_expr(operand);
                match op {
                    UnaryOp::Neg => self.emit_simple(Opcode::Neg),
                    UnaryOp::Not => self.emit_simple(Opcode::Not),
                };
            }

            Expr::Call { name, args, .. } => {
                // Push arguments left-to-right
                for arg in args {
                    self.compile_expr(arg);
                }

                let func_id = self.func_name_to_id[name];
                self.emit(Opcode::Call, func_id as i32, args.len() as i32);
            }

            Expr::ArrayIndex {
                array_name, index, ..
            } => {
                self.compile_expr(index);

                if let Some(local) = self.current_locals.get(array_name) {
                    self.emit(
                        Opcode::LocalArrayLoad,
                        local.slot as i32,
                        local.array_size as i32,
                    );
                } else if let Some(global) = self.globals.get(array_name) {
                    self.emit(
                        Opcode::ArrayLoad,
                        global.slot as i32,
                        global.array_size as i32,
                    );
                }
            }
        }
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

/// Disassemble bytecode for debugging
pub fn disassemble(program: &CompiledProgram) -> String {
    let mut output = String::new();

    output.push_str("=== Globals ===\n");
    for (name, info) in &program.globals {
        if info.is_array {
            output.push_str(&format!(
                "  {} [{}]: slot {}\n",
                name, info.array_size, info.slot
            ));
        } else {
            output.push_str(&format!("  {}: slot {}\n", name, info.slot));
        }
    }

    output.push_str("\n=== Functions ===\n");
    for (id, func) in &program.functions {
        output.push_str(&format!(
            "  {}: id={}, entry={}, params={}, locals={}\n",
            func.name, id, func.entry_pc, func.param_count, func.local_count
        ));
    }

    output.push_str("\n=== Instructions ===\n");
    for (pc, instr) in program.instructions.iter().enumerate() {
        output.push_str(&format!(
            "  {:4}: {:?} {} {}\n",
            pc, instr.opcode, instr.arg1, instr.arg2
        ));
    }

    output
}
