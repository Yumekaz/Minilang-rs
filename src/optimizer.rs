//! Optimization passes for MiniLang IR.
//!
//! Implements classic compiler optimizations:
//! - Constant folding: Evaluate constant expressions at compile time
//! - Dead code elimination: Remove unreachable code
//! - Strength reduction: Replace expensive ops with cheaper ones
//! - Peephole optimization: Local instruction pattern matching

use crate::compiler::{CompiledProgram, FunctionInfo, Instruction, Opcode};
use std::collections::{HashMap, HashSet};

/// Optimization statistics
#[derive(Debug, Default)]
pub struct OptimizationStats {
    pub constants_folded: usize,
    pub dead_instructions_removed: usize,
    pub strength_reductions: usize,
    pub peephole_optimizations: usize,
    pub instructions_before: usize,
    pub instructions_after: usize,
}

impl std::fmt::Display for OptimizationStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reduction = if self.instructions_before > 0 {
            100.0 * (1.0 - (self.instructions_after as f64 / self.instructions_before as f64))
        } else {
            0.0
        };

        write!(
            f,
            "Optimization Stats:\n\
             \x20 Constants folded:    {}\n\
             \x20 Dead code removed:   {}\n\
             \x20 Strength reductions: {}\n\
             \x20 Peephole opts:       {}\n\
             \x20 Instructions: {} -> {} ({:.1}% reduction)",
            self.constants_folded,
            self.dead_instructions_removed,
            self.strength_reductions,
            self.peephole_optimizations,
            self.instructions_before,
            self.instructions_after,
            reduction
        )
    }
}

/// Optimizer for compiled programs
pub struct Optimizer {
    stats: OptimizationStats,
}

impl Optimizer {
    pub fn new() -> Self {
        Self {
            stats: OptimizationStats::default(),
        }
    }

    /// Run all optimization passes
    pub fn optimize(&mut self, program: CompiledProgram) -> CompiledProgram {
        self.stats.instructions_before = program.instructions.len();

        let mut instructions = program.instructions;
        let mut functions = program.functions;

        // Pass 1: Constant folding
        instructions = self.constant_folding(instructions);

        // Pass 2: Strength reduction
        instructions = self.strength_reduction(instructions);

        // Pass 3: Peephole optimization
        instructions = self.peephole_optimization(instructions);

        // Pass 4: Dead code elimination
        let (new_instructions, new_functions) = self.dead_code_elimination(instructions, functions);
        instructions = new_instructions;
        functions = new_functions;

        self.stats.instructions_after = instructions.len();

        CompiledProgram {
            instructions,
            functions,
            globals: program.globals,
            main_func_id: program.main_func_id,
            constants: program.constants,
        }
    }

    /// Get optimization statistics
    pub fn stats(&self) -> &OptimizationStats {
        &self.stats
    }

    /// Constant folding: evaluate constant expressions at compile time
    fn constant_folding(&mut self, instructions: Vec<Instruction>) -> Vec<Instruction> {
        let mut result = Vec::with_capacity(instructions.len());
        let mut i = 0;

        while i < instructions.len() {
            // Look for pattern: LOAD_CONST a, LOAD_CONST b, <op>
            if i + 2 < instructions.len() {
                let i0 = &instructions[i];
                let i1 = &instructions[i + 1];
                let i2 = &instructions[i + 2];

                if i0.opcode == Opcode::LoadConst && i1.opcode == Opcode::LoadConst {
                    let a = i0.arg1 as i64;
                    let b = i1.arg1 as i64;

                    let folded = match i2.opcode {
                        Opcode::Add => Some(Self::normalize_i32(a.wrapping_add(b))),
                        Opcode::Sub => Some(Self::normalize_i32(a.wrapping_sub(b))),
                        Opcode::Mul => Some(Self::normalize_i32(a.wrapping_mul(b))),
                        Opcode::Div if b != 0 => Some(Self::normalize_i32(a / b)),
                        Opcode::Eq => Some(if a == b { 1 } else { 0 }),
                        Opcode::Ne => Some(if a != b { 1 } else { 0 }),
                        Opcode::Lt => Some(if a < b { 1 } else { 0 }),
                        Opcode::Gt => Some(if a > b { 1 } else { 0 }),
                        Opcode::Le => Some(if a <= b { 1 } else { 0 }),
                        Opcode::Ge => Some(if a >= b { 1 } else { 0 }),
                        _ => None,
                    };

                    if let Some(value) = folded {
                        result.push(Instruction::new(Opcode::LoadConst, value as i32, 0));
                        self.stats.constants_folded += 1;
                        i += 3;
                        continue;
                    }
                }
            }

            // Look for pattern: LOAD_CONST a, NEG -> LOAD_CONST -a
            if i + 1 < instructions.len() {
                let i0 = &instructions[i];
                let i1 = &instructions[i + 1];

                if i0.opcode == Opcode::LoadConst && i1.opcode == Opcode::Neg {
                    let value = Self::normalize_i32(-(i0.arg1 as i64));
                    result.push(Instruction::new(Opcode::LoadConst, value as i32, 0));
                    self.stats.constants_folded += 1;
                    i += 2;
                    continue;
                }

                // LOAD_CONST 0/1, NOT -> LOAD_CONST 1/0
                if i0.opcode == Opcode::LoadConst && i1.opcode == Opcode::Not {
                    let value = if i0.arg1 == 0 { 1 } else { 0 };
                    result.push(Instruction::new(Opcode::LoadConst, value, 0));
                    self.stats.constants_folded += 1;
                    i += 2;
                    continue;
                }
            }

            result.push(instructions[i].clone());
            i += 1;
        }

        result
    }

    /// Strength reduction: replace expensive operations with cheaper ones
    fn strength_reduction(&mut self, instructions: Vec<Instruction>) -> Vec<Instruction> {
        let mut result = Vec::with_capacity(instructions.len());
        let mut i = 0;

        while i < instructions.len() {
            // Look for: LOAD_CONST 2, MUL -> duplicate + ADD (shifts would be better but we don't have them)
            if i + 1 < instructions.len() {
                let i0 = &instructions[i];
                let i1 = &instructions[i + 1];

                // Multiply by 0 -> pop, push 0
                if i0.opcode == Opcode::LoadConst && i0.arg1 == 0 && i1.opcode == Opcode::Mul {
                    result.push(Instruction::new(Opcode::Pop, 0, 0));
                    result.push(Instruction::new(Opcode::LoadConst, 0, 0));
                    self.stats.strength_reductions += 1;
                    i += 2;
                    continue;
                }

                // Multiply by 1 -> remove both instructions
                if i0.opcode == Opcode::LoadConst && i0.arg1 == 1 && i1.opcode == Opcode::Mul {
                    // Just skip both - the value is already on stack
                    self.stats.strength_reductions += 1;
                    i += 2;
                    continue;
                }

                // Add 0 -> remove both
                if i0.opcode == Opcode::LoadConst && i0.arg1 == 0 && i1.opcode == Opcode::Add {
                    self.stats.strength_reductions += 1;
                    i += 2;
                    continue;
                }

                // Subtract 0 -> remove both
                if i0.opcode == Opcode::LoadConst && i0.arg1 == 0 && i1.opcode == Opcode::Sub {
                    self.stats.strength_reductions += 1;
                    i += 2;
                    continue;
                }

                // Divide by 1 -> remove both
                if i0.opcode == Opcode::LoadConst && i0.arg1 == 1 && i1.opcode == Opcode::Div {
                    self.stats.strength_reductions += 1;
                    i += 2;
                    continue;
                }
            }

            result.push(instructions[i].clone());
            i += 1;
        }

        result
    }

    /// Peephole optimization: local pattern matching
    fn peephole_optimization(&mut self, instructions: Vec<Instruction>) -> Vec<Instruction> {
        let mut result = Vec::with_capacity(instructions.len());
        let mut i = 0;

        while i < instructions.len() {
            if i + 1 < instructions.len() {
                let i0 = &instructions[i];
                let i1 = &instructions[i + 1];

                // Pattern: LOAD_LOCAL x, POP -> nothing (dead load)
                if i0.opcode == Opcode::LoadLocal && i1.opcode == Opcode::Pop {
                    self.stats.peephole_optimizations += 1;
                    i += 2;
                    continue;
                }

                // Pattern: LOAD_CONST x, POP -> nothing (dead load)
                if i0.opcode == Opcode::LoadConst && i1.opcode == Opcode::Pop {
                    self.stats.peephole_optimizations += 1;
                    i += 2;
                    continue;
                }

                // Pattern: JUMP to next instruction -> nothing
                if i0.opcode == Opcode::Jump && i0.arg1 == (i + 1) as i32 {
                    self.stats.peephole_optimizations += 1;
                    i += 1;
                    continue;
                }
            }

            result.push(instructions[i].clone());
            i += 1;
        }

        result
    }

    /// Dead code elimination: remove unreachable instructions
    fn dead_code_elimination(
        &mut self,
        instructions: Vec<Instruction>,
        functions: HashMap<usize, FunctionInfo>,
    ) -> (Vec<Instruction>, HashMap<usize, FunctionInfo>) {
        if instructions.is_empty() {
            return (instructions, functions);
        }

        // Find all reachable instructions via control flow analysis
        let mut reachable = HashSet::new();
        let mut worklist = Vec::new();

        // Start from all function entry points
        for func in functions.values() {
            worklist.push(func.entry_pc);
        }

        while let Some(pc) = worklist.pop() {
            if pc >= instructions.len() || reachable.contains(&pc) {
                continue;
            }

            reachable.insert(pc);
            let instr = &instructions[pc];

            match instr.opcode {
                Opcode::Jump => {
                    // Only follow jump target
                    worklist.push(instr.arg1 as usize);
                }
                Opcode::JumpIfFalse | Opcode::JumpIfTrue => {
                    // Follow both branches
                    worklist.push(pc + 1);
                    worklist.push(instr.arg1 as usize);
                }
                Opcode::Return | Opcode::Halt => {
                    // No successors
                }
                Opcode::Call => {
                    // Continue after call returns
                    worklist.push(pc + 1);
                }
                _ => {
                    // Fall through to next instruction
                    worklist.push(pc + 1);
                }
            }
        }

        // If all instructions are reachable, no changes needed
        if reachable.len() == instructions.len() {
            return (instructions, functions);
        }

        // Build new instruction array with only reachable instructions
        // Need to remap all jump targets
        let mut old_to_new: HashMap<usize, usize> = HashMap::new();
        let mut new_pc = 0;

        for old_pc in 0..instructions.len() {
            if reachable.contains(&old_pc) {
                old_to_new.insert(old_pc, new_pc);
                new_pc += 1;
            }
        }

        // Build new instruction array
        let mut new_instructions = Vec::new();
        for (old_pc, instr) in instructions.into_iter().enumerate() {
            if !reachable.contains(&old_pc) {
                self.stats.dead_instructions_removed += 1;
                continue;
            }

            // Remap jump targets
            let new_instr = match instr.opcode {
                Opcode::Jump | Opcode::JumpIfFalse | Opcode::JumpIfTrue => {
                    let new_target = old_to_new
                        .get(&(instr.arg1 as usize))
                        .copied()
                        .unwrap_or(instr.arg1 as usize);
                    Instruction::new(instr.opcode, new_target as i32, instr.arg2)
                }
                _ => instr,
            };

            new_instructions.push(new_instr);
        }

        // Update function entry points
        let mut new_functions = HashMap::new();
        for (id, mut func) in functions {
            if let Some(&new_entry) = old_to_new.get(&func.entry_pc) {
                func.entry_pc = new_entry;
                new_functions.insert(id, func);
            }
        }

        (new_instructions, new_functions)
    }

    /// 32-bit normalization
    fn normalize_i32(value: i64) -> i64 {
        let masked = value & 0xFFFFFFFF;
        if masked > 0x7FFFFFFF {
            (masked as i64) - 0x100000000
        } else {
            masked as i64
        }
    }
}

impl Default for Optimizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_program(instructions: Vec<Instruction>) -> CompiledProgram {
        let mut functions = HashMap::new();
        functions.insert(
            0,
            FunctionInfo {
                name: "main".to_string(),
                id: 0,
                entry_pc: 0,
                param_count: 0,
                local_count: 0,
            },
        );

        CompiledProgram {
            instructions,
            functions,
            globals: HashMap::new(),
            main_func_id: 0,
            constants: Vec::new(),
        }
    }

    #[test]
    fn test_constant_folding_add() {
        let program = make_program(vec![
            Instruction::new(Opcode::LoadConst, 10, 0),
            Instruction::new(Opcode::LoadConst, 20, 0),
            Instruction::new(Opcode::Add, 0, 0),
            Instruction::new(Opcode::Return, 0, 0),
        ]);

        let mut opt = Optimizer::new();
        let optimized = opt.optimize(program);

        assert_eq!(optimized.instructions.len(), 2);
        assert_eq!(optimized.instructions[0].opcode, Opcode::LoadConst);
        assert_eq!(optimized.instructions[0].arg1, 30);
        assert_eq!(opt.stats.constants_folded, 1);
    }

    #[test]
    fn test_constant_folding_multiply() {
        let program = make_program(vec![
            Instruction::new(Opcode::LoadConst, 6, 0),
            Instruction::new(Opcode::LoadConst, 7, 0),
            Instruction::new(Opcode::Mul, 0, 0),
            Instruction::new(Opcode::Return, 0, 0),
        ]);

        let mut opt = Optimizer::new();
        let optimized = opt.optimize(program);

        assert_eq!(optimized.instructions[0].arg1, 42);
    }

    #[test]
    fn test_strength_reduction_multiply_by_zero() {
        let program = make_program(vec![
            Instruction::new(Opcode::LoadLocal, 0, 0),
            Instruction::new(Opcode::LoadConst, 0, 0),
            Instruction::new(Opcode::Mul, 0, 0),
            Instruction::new(Opcode::Return, 0, 0),
        ]);

        let mut opt = Optimizer::new();
        let optimized = opt.optimize(program);

        // Should become: LOAD_LOCAL, POP, LOAD_CONST 0, RETURN
        assert!(opt.stats.strength_reductions > 0);
    }

    #[test]
    fn test_strength_reduction_add_zero() {
        let program = make_program(vec![
            Instruction::new(Opcode::LoadLocal, 0, 0),
            Instruction::new(Opcode::LoadConst, 0, 0),
            Instruction::new(Opcode::Add, 0, 0),
            Instruction::new(Opcode::Return, 0, 0),
        ]);

        let mut opt = Optimizer::new();
        let optimized = opt.optimize(program);

        // LOAD_CONST 0 and ADD should be removed
        assert_eq!(optimized.instructions.len(), 2);
        assert!(opt.stats.strength_reductions > 0);
    }

    #[test]
    fn test_dead_code_after_return() {
        let program = make_program(vec![
            Instruction::new(Opcode::LoadConst, 42, 0),
            Instruction::new(Opcode::Return, 0, 0),
            Instruction::new(Opcode::LoadConst, 0, 0), // Dead
            Instruction::new(Opcode::Return, 0, 0),    // Dead
        ]);

        let mut opt = Optimizer::new();
        let optimized = opt.optimize(program);

        assert_eq!(optimized.instructions.len(), 2);
        assert_eq!(opt.stats.dead_instructions_removed, 2);
    }
}
