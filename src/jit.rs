//! x86-64 JIT Compiler for MiniLang.
//!
//! Compiles bytecode to native x86-64 machine code.
//! This is the core systems engineering component demonstrating:
//! - Direct machine code generation
//! - Register allocation
//! - Calling conventions (System V AMD64 ABI)
//! - Executable memory management (mmap/mprotect)

use crate::compiler::{CompiledProgram, Instruction, Opcode};
use std::collections::HashMap;

#[cfg(target_os = "linux")]
use libc::{mmap, mprotect, munmap, MAP_ANONYMOUS, MAP_PRIVATE, PROT_EXEC, PROT_READ, PROT_WRITE};

/// x86-64 registers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

/// Machine code buffer
pub struct MachineCode {
    code: Vec<u8>,
    /// Label positions for patching
    labels: HashMap<usize, usize>,
    /// Pending jumps to patch
    pending_jumps: Vec<(usize, usize)>, // (code_offset, target_label)
}

impl MachineCode {
    pub fn new() -> Self {
        Self {
            code: Vec::with_capacity(4096),
            labels: HashMap::new(),
            pending_jumps: Vec::new(),
        }
    }

    /// Emit raw bytes
    #[inline]
    pub fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// Emit a single byte
    #[inline]
    pub fn emit_u8(&mut self, b: u8) {
        self.code.push(b);
    }

    /// Emit a 32-bit value (little-endian)
    #[inline]
    pub fn emit_i32(&mut self, val: i32) {
        self.code.extend_from_slice(&val.to_le_bytes());
    }

    /// Emit a 64-bit value (little-endian)
    #[inline]
    pub fn emit_i64(&mut self, val: i64) {
        self.code.extend_from_slice(&val.to_le_bytes());
    }

    /// Current position in code buffer
    pub fn pos(&self) -> usize {
        self.code.len()
    }

    /// Define a label at current position
    pub fn label(&mut self, id: usize) {
        self.labels.insert(id, self.pos());
    }

    /// Get the generated code
    pub fn code(&self) -> &[u8] {
        &self.code
    }

    // ========================================================================
    // x86-64 Instruction Encoding
    // ========================================================================

    /// REX prefix for 64-bit operations
    fn rex(&mut self, w: bool, r: Reg, b: Reg) {
        let mut rex = 0x40u8;
        if w {
            rex |= 0x08;
        } // W bit for 64-bit operand
        if (r as u8) >= 8 {
            rex |= 0x04;
        } // R bit for extended reg
        if (b as u8) >= 8 {
            rex |= 0x01;
        } // B bit for extended reg
        if rex != 0x40 {
            self.emit_u8(rex);
        }
    }

    /// REX.W prefix (always needed for 64-bit)
    fn rex_w(&mut self, r: Reg, b: Reg) {
        self.rex(true, r, b);
    }

    /// ModR/M byte
    fn modrm(&mut self, mode: u8, reg: Reg, rm: Reg) {
        let r = (reg as u8) & 7;
        let b = (rm as u8) & 7;
        self.emit_u8((mode << 6) | (r << 3) | b);
    }

    /// push reg
    pub fn push(&mut self, reg: Reg) {
        if (reg as u8) >= 8 {
            self.emit_u8(0x41); // REX.B
        }
        self.emit_u8(0x50 + ((reg as u8) & 7));
    }

    /// pop reg
    pub fn pop(&mut self, reg: Reg) {
        if (reg as u8) >= 8 {
            self.emit_u8(0x41); // REX.B
        }
        self.emit_u8(0x58 + ((reg as u8) & 7));
    }

    /// mov reg, imm64
    pub fn mov_imm64(&mut self, dst: Reg, imm: i64) {
        self.rex_w(Reg::Rax, dst);
        self.emit_u8(0xB8 + ((dst as u8) & 7));
        self.emit_i64(imm);
    }

    /// mov reg, imm32 (sign-extended)
    pub fn mov_imm32(&mut self, dst: Reg, imm: i32) {
        self.rex_w(Reg::Rax, dst);
        self.emit_u8(0xC7);
        self.modrm(0b11, Reg::Rax, dst);
        self.emit_i32(imm);
    }

    /// mov dst, src (64-bit)
    pub fn mov(&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst);
        self.emit_u8(0x89);
        self.modrm(0b11, src, dst);
    }

    /// mov dst, [src + offset]
    pub fn mov_load(&mut self, dst: Reg, src: Reg, offset: i32) {
        self.rex_w(dst, src);
        self.emit_u8(0x8B);
        if offset == 0 && (src as u8) != 5 {
            self.modrm(0b00, dst, src);
        } else if offset >= -128 && offset <= 127 {
            self.modrm(0b01, dst, src);
            self.emit_u8(offset as u8);
        } else {
            self.modrm(0b10, dst, src);
            self.emit_i32(offset);
        }
        // Handle RSP/R12 (need SIB byte)
        if (src as u8) & 7 == 4 {
            self.emit_u8(0x24); // SIB: scale=0, index=RSP, base=RSP
        }
    }

    /// mov [dst + offset], src
    pub fn mov_store(&mut self, dst: Reg, offset: i32, src: Reg) {
        self.rex_w(src, dst);
        self.emit_u8(0x89);
        if offset == 0 && (dst as u8) != 5 {
            self.modrm(0b00, src, dst);
        } else if offset >= -128 && offset <= 127 {
            self.modrm(0b01, src, dst);
            self.emit_u8(offset as u8);
        } else {
            self.modrm(0b10, src, dst);
            self.emit_i32(offset);
        }
        // Handle RSP/R12
        if (dst as u8) & 7 == 4 {
            self.emit_u8(0x24);
        }
    }

    /// add dst, src
    pub fn add(&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst);
        self.emit_u8(0x01);
        self.modrm(0b11, src, dst);
    }

    /// add dst, imm32
    pub fn add_imm(&mut self, dst: Reg, imm: i32) {
        self.rex_w(Reg::Rax, dst);
        if imm >= -128 && imm <= 127 {
            self.emit_u8(0x83);
            self.modrm(0b11, Reg::Rax, dst);
            self.emit_u8(imm as u8);
        } else {
            self.emit_u8(0x81);
            self.modrm(0b11, Reg::Rax, dst);
            self.emit_i32(imm);
        }
    }

    /// sub dst, src
    pub fn sub(&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst);
        self.emit_u8(0x29);
        self.modrm(0b11, src, dst);
    }

    /// sub dst, imm32
    pub fn sub_imm(&mut self, dst: Reg, imm: i32) {
        self.rex_w(Reg::Rax, dst);
        if imm >= -128 && imm <= 127 {
            self.emit_u8(0x83);
            self.modrm(0b11, Reg::Rbp, dst); // /5 for sub
            self.emit_u8(imm as u8);
        } else {
            self.emit_u8(0x81);
            self.modrm(0b11, Reg::Rbp, dst);
            self.emit_i32(imm);
        }
    }

    /// imul dst, src
    pub fn imul(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.emit(&[0x0F, 0xAF]);
        self.modrm(0b11, dst, src);
    }

    /// neg dst
    pub fn neg(&mut self, dst: Reg) {
        self.rex_w(Reg::Rax, dst);
        self.emit_u8(0xF7);
        self.modrm(0b11, Reg::Rbx, dst); // /3 for neg
    }

    /// cmp dst, src
    pub fn cmp(&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst);
        self.emit_u8(0x39);
        self.modrm(0b11, src, dst);
    }

    /// cmp dst, imm32
    pub fn cmp_imm(&mut self, dst: Reg, imm: i32) {
        self.rex_w(Reg::Rax, dst);
        if imm >= -128 && imm <= 127 {
            self.emit_u8(0x83);
            self.modrm(0b11, Reg::Rdi, dst); // /7 for cmp
            self.emit_u8(imm as u8);
        } else {
            self.emit_u8(0x81);
            self.modrm(0b11, Reg::Rdi, dst);
            self.emit_i32(imm);
        }
    }

    /// test dst, src
    pub fn test(&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst);
        self.emit_u8(0x85);
        self.modrm(0b11, src, dst);
    }

    /// sete dst (set if equal)
    pub fn sete(&mut self, dst: Reg) {
        if (dst as u8) >= 8 {
            self.emit_u8(0x41);
        }
        self.emit(&[0x0F, 0x94]);
        self.modrm(0b11, Reg::Rax, dst);
    }

    /// setne dst
    pub fn setne(&mut self, dst: Reg) {
        if (dst as u8) >= 8 {
            self.emit_u8(0x41);
        }
        self.emit(&[0x0F, 0x95]);
        self.modrm(0b11, Reg::Rax, dst);
    }

    /// setl dst (set if less)
    pub fn setl(&mut self, dst: Reg) {
        if (dst as u8) >= 8 {
            self.emit_u8(0x41);
        }
        self.emit(&[0x0F, 0x9C]);
        self.modrm(0b11, Reg::Rax, dst);
    }

    /// setg dst (set if greater)
    pub fn setg(&mut self, dst: Reg) {
        if (dst as u8) >= 8 {
            self.emit_u8(0x41);
        }
        self.emit(&[0x0F, 0x9F]);
        self.modrm(0b11, Reg::Rax, dst);
    }

    /// setle dst
    pub fn setle(&mut self, dst: Reg) {
        if (dst as u8) >= 8 {
            self.emit_u8(0x41);
        }
        self.emit(&[0x0F, 0x9E]);
        self.modrm(0b11, Reg::Rax, dst);
    }

    /// setge dst
    pub fn setge(&mut self, dst: Reg) {
        if (dst as u8) >= 8 {
            self.emit_u8(0x41);
        }
        self.emit(&[0x0F, 0x9D]);
        self.modrm(0b11, Reg::Rax, dst);
    }

    /// movzx dst, src (zero-extend byte to 64-bit)
    pub fn movzx(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.emit(&[0x0F, 0xB6]);
        self.modrm(0b11, dst, src);
    }

    /// movsxd dst, src (sign-extend 32-bit value to 64-bit)
    pub fn movsxd_32(&mut self, dst: Reg, src: Reg) {
        self.rex_w(dst, src);
        self.emit_u8(0x63);
        self.modrm(0b11, dst, src);
    }

    /// jmp rel32
    pub fn jmp(&mut self, offset: i32) {
        self.emit_u8(0xE9);
        self.emit_i32(offset);
    }

    /// jmp to label (to be patched)
    pub fn jmp_label(&mut self, label: usize) {
        self.emit_u8(0xE9);
        self.pending_jumps.push((self.pos(), label));
        self.emit_i32(0); // Placeholder
    }

    /// je rel32 (jump if equal)
    pub fn je(&mut self, offset: i32) {
        self.emit(&[0x0F, 0x84]);
        self.emit_i32(offset);
    }

    /// je to label
    pub fn je_label(&mut self, label: usize) {
        self.emit(&[0x0F, 0x84]);
        self.pending_jumps.push((self.pos(), label));
        self.emit_i32(0);
    }

    /// jne rel32 (jump if not equal)
    pub fn jne(&mut self, offset: i32) {
        self.emit(&[0x0F, 0x85]);
        self.emit_i32(offset);
    }

    /// jne to label
    pub fn jne_label(&mut self, label: usize) {
        self.emit(&[0x0F, 0x85]);
        self.pending_jumps.push((self.pos(), label));
        self.emit_i32(0);
    }

    /// call rel32
    pub fn call(&mut self, offset: i32) {
        self.emit_u8(0xE8);
        self.emit_i32(offset);
    }

    /// call reg
    pub fn call_reg(&mut self, reg: Reg) {
        if (reg as u8) >= 8 {
            self.emit_u8(0x41);
        }
        self.emit_u8(0xFF);
        self.modrm(0b11, Reg::Rdx, reg); // /2 for call
    }

    /// ret
    pub fn ret(&mut self) {
        self.emit_u8(0xC3);
    }

    /// cdq (sign-extend EAX into EDX:EAX)
    pub fn cdq(&mut self) {
        self.emit_u8(0x99);
    }

    /// cqo (sign-extend RAX into RDX:RAX)
    pub fn cqo(&mut self) {
        self.rex_w(Reg::Rax, Reg::Rax);
        self.emit_u8(0x99);
    }

    /// idiv src (signed divide RDX:RAX by src)
    pub fn idiv(&mut self, src: Reg) {
        self.rex_w(Reg::Rax, src);
        self.emit_u8(0xF7);
        self.modrm(0b11, Reg::Rdi, src); // /7 for idiv
    }

    /// xor dst, dst (efficient zero)
    pub fn xor(&mut self, dst: Reg, src: Reg) {
        self.rex_w(src, dst);
        self.emit_u8(0x31);
        self.modrm(0b11, src, dst);
    }

    /// Patch all pending jumps
    pub fn patch_jumps(&mut self) {
        for (code_offset, label) in &self.pending_jumps {
            if let Some(&target) = self.labels.get(label) {
                let rel = (target as i32) - (*code_offset as i32) - 4;
                let bytes = rel.to_le_bytes();
                self.code[*code_offset] = bytes[0];
                self.code[*code_offset + 1] = bytes[1];
                self.code[*code_offset + 2] = bytes[2];
                self.code[*code_offset + 3] = bytes[3];
            }
        }
    }
}

impl Default for MachineCode {
    fn default() -> Self {
        Self::new()
    }
}

/// Executable memory region
pub struct ExecutableMemory {
    ptr: *mut u8,
    size: usize,
}

impl ExecutableMemory {
    /// Allocate executable memory and copy code into it
    #[cfg(target_os = "linux")]
    pub fn new(code: &[u8]) -> Option<Self> {
        let size = (code.len() + 4095) & !4095; // Round up to page size

        unsafe {
            let ptr = mmap(
                std::ptr::null_mut(),
                size,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            ) as *mut u8;

            if ptr.is_null() || ptr == libc::MAP_FAILED as *mut u8 {
                return None;
            }

            // Copy code
            std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len());

            // Make executable
            if mprotect(ptr as *mut _, size, PROT_READ | PROT_EXEC) != 0 {
                munmap(ptr as *mut _, size);
                return None;
            }

            Some(Self { ptr, size })
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new(_code: &[u8]) -> Option<Self> {
        // Stub for non-Linux platforms
        None
    }

    /// Get function pointer to the start of the code
    pub fn as_fn<T>(&self) -> T
    where
        T: Copy,
    {
        unsafe { std::mem::transmute_copy(&self.ptr) }
    }

    /// Get function pointer at an offset
    pub fn as_fn_at<T>(&self, offset: usize) -> T
    where
        T: Copy,
    {
        unsafe {
            let ptr = self.ptr.add(offset);
            std::mem::transmute_copy(&ptr)
        }
    }
}

impl Drop for ExecutableMemory {
    #[cfg(target_os = "linux")]
    fn drop(&mut self) {
        unsafe {
            munmap(self.ptr as *mut _, self.size);
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn drop(&mut self) {}
}

/// JIT compiler
pub struct JitCompiler {
    code: MachineCode,
    /// Bytecode PC to machine code offset mapping
    pc_to_offset: HashMap<usize, usize>,
    /// Function entry points (bytecode PC)
    func_entries: HashMap<usize, usize>, // func_id -> entry_pc
}

impl JitCompiler {
    pub fn new() -> Self {
        Self {
            code: MachineCode::new(),
            pc_to_offset: HashMap::new(),
            func_entries: HashMap::new(),
        }
    }

    /// Compile bytecode to native code
    pub fn compile(mut self, program: &CompiledProgram) -> Option<ExecutableMemory> {
        // Record function info
        for (func_id, func_info) in &program.functions {
            self.func_entries.insert(*func_id, func_info.entry_pc);
        }

        // Find main function entry
        let main_entry_pc = program
            .functions
            .get(&program.main_func_id)
            .map(|f| f.entry_pc)
            .unwrap_or(0);

        if !Self::supports_program(program, main_entry_pc) {
            return None;
        }

        // Emit prologue for main
        self.emit_prologue();

        // Compile only main function (starting from its entry PC)
        for pc in main_entry_pc..program.instructions.len() {
            let instr = &program.instructions[pc];

            // Record where this PC starts in machine code
            self.pc_to_offset.insert(pc, self.code.pos());
            self.code.label(pc);

            self.compile_instruction(instr, pc, program);

            // Stop if we hit a Return (end of main)
            if matches!(instr.opcode, Opcode::Return) {
                break;
            }
        }

        // Patch jumps
        self.code.patch_jumps();

        // Allocate executable memory
        ExecutableMemory::new(self.code.code())
    }

    fn supports_program(program: &CompiledProgram, main_entry_pc: usize) -> bool {
        if program.functions.len() != 1 {
            return false;
        }

        if main_entry_pc >= program.instructions.len() {
            return false;
        }

        let mut saw_return = false;
        for instr in program.instructions.iter().skip(main_entry_pc) {
            if !Self::supports_opcode(instr.opcode) {
                return false;
            }

            if matches!(instr.opcode, Opcode::Return) {
                saw_return = true;
                break;
            }
        }

        saw_return
    }

    fn supports_opcode(opcode: Opcode) -> bool {
        matches!(
            opcode,
            Opcode::LoadConst
                | Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::Neg
                | Opcode::Eq
                | Opcode::Ne
                | Opcode::Lt
                | Opcode::Gt
                | Opcode::Le
                | Opcode::Ge
                | Opcode::Not
                | Opcode::Pop
                | Opcode::Dup
                | Opcode::Return
        )
    }

    fn emit_prologue(&mut self) {
        // Standard System V AMD64 prologue
        self.code.push(Reg::Rbp);
        self.code.mov(Reg::Rbp, Reg::Rsp);
        // Reserve stack space:
        // - 256 bytes for locals (slots 0-31)
        // - 2048 bytes for globals (slots 0-255)
        // - 4096 bytes for arrays
        // Total: ~8KB
        self.code.sub_imm(Reg::Rsp, 8192);
    }

    fn emit_epilogue(&mut self) {
        // Simple epilogue
        self.code.mov(Reg::Rsp, Reg::Rbp);
        self.code.pop(Reg::Rbp);
        self.code.ret();
    }

    fn compile_instruction(&mut self, instr: &Instruction, _pc: usize, _program: &CompiledProgram) {
        match instr.opcode {
            Opcode::LoadConst => {
                // Push constant onto stack (using RAX as temp)
                self.code.mov_imm32(Reg::Rax, instr.arg1);
                self.code.push(Reg::Rax);
            }

            Opcode::LoadLocal => {
                // Load from stack frame
                let offset = -(8 + instr.arg1 as i32 * 8);
                self.code.mov_load(Reg::Rax, Reg::Rbp, offset);
                self.code.push(Reg::Rax);
            }

            Opcode::StoreLocal => {
                let offset = -(8 + instr.arg1 as i32 * 8);
                self.code.pop(Reg::Rax);
                self.code.mov_store(Reg::Rbp, offset, Reg::Rax);
            }

            Opcode::Add => {
                self.code.pop(Reg::Rcx); // Right operand
                self.code.pop(Reg::Rax); // Left operand
                self.code.add(Reg::Rax, Reg::Rcx);
                self.code.movsxd_32(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Sub => {
                self.code.pop(Reg::Rcx);
                self.code.pop(Reg::Rax);
                self.code.sub(Reg::Rax, Reg::Rcx);
                self.code.movsxd_32(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Mul => {
                self.code.pop(Reg::Rcx);
                self.code.pop(Reg::Rax);
                self.code.imul(Reg::Rax, Reg::Rcx);
                self.code.movsxd_32(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Div => {
                self.code.pop(Reg::Rcx); // Divisor
                self.code.pop(Reg::Rax); // Dividend
                self.code.cqo(); // Sign-extend RAX into RDX:RAX
                self.code.idiv(Reg::Rcx); // RAX = quotient
                self.code.push(Reg::Rax);
            }

            Opcode::Neg => {
                self.code.pop(Reg::Rax);
                self.code.neg(Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Eq => {
                self.code.pop(Reg::Rcx);
                self.code.pop(Reg::Rax);
                self.code.cmp(Reg::Rax, Reg::Rcx);
                self.code.sete(Reg::Rax);
                self.code.movzx(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Ne => {
                self.code.pop(Reg::Rcx);
                self.code.pop(Reg::Rax);
                self.code.cmp(Reg::Rax, Reg::Rcx);
                self.code.setne(Reg::Rax);
                self.code.movzx(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Lt => {
                self.code.pop(Reg::Rcx);
                self.code.pop(Reg::Rax);
                self.code.cmp(Reg::Rax, Reg::Rcx);
                self.code.setl(Reg::Rax);
                self.code.movzx(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Gt => {
                self.code.pop(Reg::Rcx);
                self.code.pop(Reg::Rax);
                self.code.cmp(Reg::Rax, Reg::Rcx);
                self.code.setg(Reg::Rax);
                self.code.movzx(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Le => {
                self.code.pop(Reg::Rcx);
                self.code.pop(Reg::Rax);
                self.code.cmp(Reg::Rax, Reg::Rcx);
                self.code.setle(Reg::Rax);
                self.code.movzx(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Ge => {
                self.code.pop(Reg::Rcx);
                self.code.pop(Reg::Rax);
                self.code.cmp(Reg::Rax, Reg::Rcx);
                self.code.setge(Reg::Rax);
                self.code.movzx(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Not => {
                self.code.pop(Reg::Rax);
                self.code.test(Reg::Rax, Reg::Rax);
                self.code.sete(Reg::Rax);
                self.code.movzx(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Jump => {
                let target_label = instr.arg1 as usize;
                self.code.jmp_label(target_label);
            }

            Opcode::JumpIfFalse => {
                self.code.pop(Reg::Rax);
                self.code.test(Reg::Rax, Reg::Rax);
                let target_label = instr.arg1 as usize;
                self.code.je_label(target_label);
            }

            Opcode::JumpIfTrue => {
                self.code.pop(Reg::Rax);
                self.code.test(Reg::Rax, Reg::Rax);
                let target_label = instr.arg1 as usize;
                self.code.jne_label(target_label);
            }

            Opcode::Return => {
                self.code.pop(Reg::Rax); // Return value in RAX
                self.emit_epilogue();
            }

            Opcode::Pop => {
                self.code.pop(Reg::Rax); // Discard
            }

            Opcode::Dup => {
                // Duplicate top of stack
                self.code.pop(Reg::Rax);
                self.code.push(Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::Halt => {
                self.emit_epilogue();
            }

            Opcode::And => {
                // Logical AND
                self.code.pop(Reg::Rcx); // Right
                self.code.pop(Reg::Rax); // Left
                self.code.test(Reg::Rax, Reg::Rax);
                self.code.setne(Reg::Rax); // RAX = (left != 0) ? 1 : 0
                self.code.test(Reg::Rcx, Reg::Rcx);
                self.code.setne(Reg::Rcx); // RCX = (right != 0) ? 1 : 0
                self.code.emit(&[0x48, 0x21, 0xC8]); // and rax, rcx
                self.code.push(Reg::Rax);
            }

            Opcode::Or => {
                // Logical OR
                self.code.pop(Reg::Rcx); // Right
                self.code.pop(Reg::Rax); // Left
                self.code.emit(&[0x48, 0x09, 0xC8]); // or rax, rcx
                self.code.test(Reg::Rax, Reg::Rax);
                self.code.setne(Reg::Rax);
                self.code.movzx(Reg::Rax, Reg::Rax);
                self.code.push(Reg::Rax);
            }

            Opcode::LoadGlobal => {
                // Globals are stored at RBP - 512 - slot*8
                // (after 256 bytes of locals, we have 256*8 bytes for globals)
                let offset = -(512 + instr.arg1 as i32 * 8);
                self.code.mov_load(Reg::Rax, Reg::Rbp, offset);
                self.code.push(Reg::Rax);
            }

            Opcode::StoreGlobal => {
                let offset = -(512 + instr.arg1 as i32 * 8);
                self.code.pop(Reg::Rax);
                self.code.mov_store(Reg::Rbp, offset, Reg::Rax);
            }

            Opcode::ArrayLoad => {
                // Global array: base at RBP - 512 - base_slot*8, indexed by top of stack
                let base_slot = instr.arg1;
                self.code.pop(Reg::Rcx); // Index
                                         // Calculate address: RBP - 512 - (base_slot + index) * 8
                self.code.mov_imm32(Reg::Rax, base_slot);
                self.code.add(Reg::Rax, Reg::Rcx); // RAX = base_slot + index
                self.code.emit(&[0x48, 0xC1, 0xE0, 0x03]); // shl rax, 3 (multiply by 8)
                self.code.neg(Reg::Rax);
                self.code.sub_imm(Reg::Rax, 512);
                self.code.add(Reg::Rax, Reg::Rbp);
                self.code.mov_load(Reg::Rax, Reg::Rax, 0);
                self.code.push(Reg::Rax);
            }

            Opcode::ArrayStore => {
                // Global array store
                let base_slot = instr.arg1;
                self.code.pop(Reg::Rdx); // Value
                self.code.pop(Reg::Rcx); // Index
                                         // Calculate address
                self.code.mov_imm32(Reg::Rax, base_slot);
                self.code.add(Reg::Rax, Reg::Rcx);
                self.code.emit(&[0x48, 0xC1, 0xE0, 0x03]); // shl rax, 3
                self.code.neg(Reg::Rax);
                self.code.sub_imm(Reg::Rax, 512);
                self.code.add(Reg::Rax, Reg::Rbp);
                self.code.mov_store(Reg::Rax, 0, Reg::Rdx);
            }

            Opcode::LocalArrayLoad => {
                // Local array: load array base from local slot, then index
                let base_slot = instr.arg1;
                self.code.pop(Reg::Rcx); // Index
                                         // Load array base address from local
                let slot_offset = -(8 + base_slot as i32 * 8);
                self.code.mov_load(Reg::Rax, Reg::Rbp, slot_offset);
                // Add index * 8
                self.code.emit(&[0x48, 0xC1, 0xE1, 0x03]); // shl rcx, 3
                self.code.add(Reg::Rax, Reg::Rcx);
                // Load value
                self.code.mov_load(Reg::Rax, Reg::Rax, 0);
                self.code.push(Reg::Rax);
            }

            Opcode::LocalArrayStore => {
                // Local array store
                let base_slot = instr.arg1;
                self.code.pop(Reg::Rdx); // Value
                self.code.pop(Reg::Rcx); // Index
                                         // Load array base address
                let slot_offset = -(8 + base_slot as i32 * 8);
                self.code.mov_load(Reg::Rax, Reg::Rbp, slot_offset);
                // Add index * 8
                self.code.emit(&[0x48, 0xC1, 0xE1, 0x03]); // shl rcx, 3
                self.code.add(Reg::Rax, Reg::Rcx);
                // Store value
                self.code.mov_store(Reg::Rax, 0, Reg::Rdx);
            }

            Opcode::AllocArray => {
                // Allocate array on stack - push the base address
                // For simplicity, just use a large negative offset from RBP
                // Unsupported by the current JIT program gate.
                // Use R13 as array allocation pointer (starts at RBP - 4096)
                // Just push the current position as the array base
                self.code.mov(Reg::Rax, Reg::Rbp);
                self.code.sub_imm(Reg::Rax, 4096);
                self.code.push(Reg::Rax);
            }

            Opcode::Call => {
                // Function call
                // Arguments are already pushed on the operand stack
                // Call the function (which will return with result in RAX)
                let func_id = instr.arg1 as usize;
                let _arg_count = instr.arg2 as usize;

                // Get the function's entry PC and call it
                if let Some(&entry_pc) = self.func_entries.get(&func_id) {
                    // Call to label (will be patched)
                    self.code.emit_u8(0xE8); // call rel32
                    self.code.pending_jumps.push((self.code.pos(), entry_pc));
                    self.code.emit_i32(0);

                    // Clean up arguments from stack (they were pushed by caller)
                    // Actually, we keep the result which is in RAX, push it
                    // First, pop all the args we pushed
                    for _ in 0.._arg_count {
                        self.code.pop(Reg::Rcx); // Discard args
                    }
                    // Push the result
                    self.code.push(Reg::Rax);
                } else {
                    // Unknown function - trap
                    self.code.emit(&[0xCC]);
                }
            }

            Opcode::Print => {
                // Print is tricky - we'd need to call libc printf
                // For now, just pop and discard
                self.code.pop(Reg::Rax);
            }

            Opcode::ArrayNew => {
                // Same as AllocArray
                self.code.mov(Reg::Rax, Reg::Rbp);
                self.code.sub_imm(Reg::Rax, 4096);
                self.code.push(Reg::Rax);
            }
        }
    }
}

impl Default for JitCompiler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile_source(source: &str) -> CompiledProgram {
        crate::compile(source).expect("test source should compile")
    }

    fn jit_supports(source: &str) -> bool {
        let compiled = compile_source(source);
        let main_entry_pc = compiled
            .functions
            .get(&compiled.main_func_id)
            .expect("compiled program should have main")
            .entry_pc;

        JitCompiler::supports_program(&compiled, main_entry_pc)
    }

    #[cfg(target_os = "linux")]
    fn run_jit_source(source: &str) -> i64 {
        let compiled = compile_source(source);
        let exec_mem = JitCompiler::new()
            .compile(&compiled)
            .expect("test source should be accepted by the JIT");
        let func: extern "C" fn() -> i64 = exec_mem.as_fn();
        func()
    }

    #[test]
    fn test_machine_code_generation() {
        let mut code = MachineCode::new();

        // mov rax, 42
        code.mov_imm32(Reg::Rax, 42);
        // ret
        code.ret();

        assert!(!code.code().is_empty());
    }

    #[test]
    fn test_jit_supports_linear_expression_bytecode() {
        assert!(jit_supports("func main() { return (1 + 2) * 3; }"));
    }

    #[test]
    fn test_jit_rejects_function_calls() {
        assert!(!jit_supports(
            "func id(int x) { return x; } func main() { return id(1); }"
        ));
    }

    #[test]
    fn test_jit_rejects_control_flow() {
        assert!(!jit_supports(
            "func main() { if (1) { return 1; } return 0; }"
        ));
    }

    #[test]
    fn test_jit_rejects_loops() {
        assert!(!jit_supports(
            "func main() { while (0) { return 1; } return 0; }"
        ));
    }

    #[test]
    fn test_jit_rejects_division_until_traps_are_lowered() {
        assert!(!jit_supports("func main() { return 10 / 2; }"));
    }

    #[test]
    fn test_jit_rejects_locals() {
        assert!(!jit_supports("func main() { int x = 1; return x; }"));
    }

    #[test]
    fn test_jit_rejects_arrays() {
        assert!(!jit_supports(
            "func main() { int a[2]; a[0] = 1; return a[0]; }"
        ));
    }

    #[test]
    fn test_jit_rejects_print() {
        assert!(!jit_supports("func main() { print 1; return 0; }"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_jit_matches_vm_i32_add_wrap() {
        assert_eq!(
            run_jit_source("func main() { return 2147483647 + 1; }"),
            -2147483648
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_jit_matches_vm_i32_mul_wrap() {
        assert_eq!(run_jit_source("func main() { return 1073741824 * 4; }"), 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_jit_simple_return() {
        let mut code = MachineCode::new();

        // Simple function that returns 42
        code.push(Reg::Rbp);
        code.mov(Reg::Rbp, Reg::Rsp);
        code.mov_imm32(Reg::Rax, 42);
        code.mov(Reg::Rsp, Reg::Rbp);
        code.pop(Reg::Rbp);
        code.ret();

        let mem = ExecutableMemory::new(code.code()).unwrap();
        let func: extern "C" fn() -> i64 = mem.as_fn();

        assert_eq!(func(), 42);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_jit_add() {
        let mut code = MachineCode::new();

        // Function that adds two arguments (in RDI and RSI)
        code.push(Reg::Rbp);
        code.mov(Reg::Rbp, Reg::Rsp);
        code.mov(Reg::Rax, Reg::Rdi);
        code.add(Reg::Rax, Reg::Rsi);
        code.mov(Reg::Rsp, Reg::Rbp);
        code.pop(Reg::Rbp);
        code.ret();

        let mem = ExecutableMemory::new(code.code()).unwrap();
        let func: extern "C" fn(i64, i64) -> i64 = mem.as_fn();

        assert_eq!(func(10, 32), 42);
        assert_eq!(func(-5, 15), 10);
    }
}
