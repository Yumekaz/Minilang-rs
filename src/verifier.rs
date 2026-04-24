//! Bytecode verifier for MiniLang.
//!
//! The verifier is intentionally separate from the VM. It checks structural
//! bytecode safety before execution and produces a report that explains stack
//! behavior, possible runtime traps, and backend eligibility.

use crate::compiler::{CompiledProgram, FunctionInfo, GlobalInfo, Instruction, Opcode};
use crate::limits;
use crate::vm::TrapCode;
use std::collections::{HashMap, HashSet, VecDeque};

/// One verification failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationError {
    pub pc: Option<usize>,
    pub message: String,
}

impl VerificationError {
    fn new(pc: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            pc,
            message: message.into(),
        }
    }
}

/// Backend eligibility result with an optional reason when rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    pub eligible: bool,
    pub reason: Option<String>,
}

impl BackendStatus {
    fn yes() -> Self {
        Self {
            eligible: true,
            reason: None,
        }
    }

    fn no(reason: impl Into<String>) -> Self {
        Self {
            eligible: false,
            reason: Some(reason.into()),
        }
    }
}

/// Execution backend eligibility summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendEligibility {
    pub vm: BackendStatus,
    pub gc_vm: BackendStatus,
    pub optimized_vm: BackendStatus,
    pub jit: BackendStatus,
}

/// Per-function verifier summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionVerification {
    pub id: usize,
    pub name: String,
    pub entry_pc: usize,
    pub end_pc: usize,
    pub reachable_instructions: usize,
    pub max_stack_depth: usize,
}

/// Full verifier output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub valid: bool,
    pub errors: Vec<VerificationError>,
    pub functions: Vec<FunctionVerification>,
    pub possible_traps: Vec<TrapCode>,
    pub max_stack_depth: usize,
    pub max_frame_depth: Option<usize>,
    pub backend_eligibility: BackendEligibility,
}

/// Bytecode verifier entry point.
#[derive(Debug, Default)]
pub struct Verifier;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AbstractState {
    stack_depth: usize,
    initialized_locals: Vec<bool>,
}

#[derive(Debug, Clone)]
struct FunctionRange {
    id: usize,
    info: FunctionInfo,
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct FunctionCheck {
    summary: FunctionVerification,
    possible_traps: Vec<TrapCode>,
    call_edges: Vec<(usize, usize)>,
    errors: Vec<VerificationError>,
}

#[derive(Debug)]
struct Step {
    state: AbstractState,
    successors: Vec<usize>,
    call_edges: Vec<(usize, usize)>,
}

struct StepContext<'a, 'b> {
    program: &'a CompiledProgram,
    range: &'a FunctionRange,
    pc: usize,
    errors: &'b mut Vec<VerificationError>,
    possible_traps: &'b mut Vec<TrapCode>,
}

impl Verifier {
    pub fn new() -> Self {
        Self
    }

    /// Verify a compiled program and return a structured report.
    pub fn verify(&self, program: &CompiledProgram) -> VerificationReport {
        let mut errors = Vec::new();
        let mut possible_traps = Vec::new();

        self.verify_global_metadata(program.globals.values(), &mut errors);

        if program.functions.is_empty() {
            errors.push(VerificationError::new(None, "program has no functions"));
        }

        if !program.functions.contains_key(&program.main_func_id) {
            errors.push(VerificationError::new(
                None,
                format!(
                    "main function id {} is missing from function metadata",
                    program.main_func_id
                ),
            ));
        }

        if program.instructions.len() > limits::MAX_INSTRUCTIONS {
            errors.push(VerificationError::new(
                None,
                format!(
                    "instruction count {} exceeds limit {}",
                    program.instructions.len(),
                    limits::MAX_INSTRUCTIONS
                ),
            ));
        }

        let ranges = self.function_ranges(program, &mut errors);
        let mut functions = Vec::new();
        let mut call_edges = HashMap::<usize, Vec<usize>>::new();

        for range in &ranges {
            let check = self.verify_function(program, range);
            errors.extend(check.errors);
            for trap in check.possible_traps {
                push_trap(&mut possible_traps, trap);
            }
            for (caller, callee) in check.call_edges {
                call_edges.entry(caller).or_default().push(callee);
            }
            functions.push(check.summary);
        }

        let max_stack_depth = functions
            .iter()
            .map(|f| f.max_stack_depth)
            .max()
            .unwrap_or(0);
        let max_frame_depth = self.max_frame_depth(program.main_func_id, &call_edges);
        match max_frame_depth {
            Some(depth) if depth > limits::MAX_FRAMES => {
                push_trap(&mut possible_traps, TrapCode::StackOverflow);
            }
            None => {
                push_trap(&mut possible_traps, TrapCode::StackOverflow);
            }
            Some(_) => {}
        }

        let valid = errors.is_empty();
        let backend_eligibility = self.backend_eligibility(program, valid);

        VerificationReport {
            valid,
            errors,
            functions,
            possible_traps,
            max_stack_depth,
            max_frame_depth,
            backend_eligibility,
        }
    }

    fn verify_global_metadata<'g>(
        &self,
        globals: impl Iterator<Item = &'g GlobalInfo>,
        errors: &mut Vec<VerificationError>,
    ) {
        let mut claimed = vec![false; limits::MAX_GLOBAL_SLOTS];

        for info in globals {
            let width = if info.is_array {
                if info.array_size == 0 {
                    errors.push(VerificationError::new(
                        None,
                        format!("global array '{}' has zero size", info.name),
                    ));
                    continue;
                }
                info.array_size
            } else {
                1
            };

            let Some(end) = info.slot.checked_add(width) else {
                errors.push(VerificationError::new(
                    None,
                    format!("global '{}' storage range overflows usize", info.name),
                ));
                continue;
            };

            if end > limits::MAX_GLOBAL_SLOTS {
                errors.push(VerificationError::new(
                    None,
                    format!(
                        "global '{}' uses slots {}..{}, exceeding limit {}",
                        info.name,
                        info.slot,
                        end,
                        limits::MAX_GLOBAL_SLOTS
                    ),
                ));
                continue;
            }

            for (slot, is_claimed) in claimed.iter_mut().enumerate().skip(info.slot).take(width) {
                if *is_claimed {
                    errors.push(VerificationError::new(
                        None,
                        format!("global '{}' overlaps slot {}", info.name, slot),
                    ));
                }
                *is_claimed = true;
            }
        }
    }

    fn function_ranges(
        &self,
        program: &CompiledProgram,
        errors: &mut Vec<VerificationError>,
    ) -> Vec<FunctionRange> {
        let mut entries = Vec::new();
        let mut seen_entries = HashSet::new();

        for (id, info) in &program.functions {
            if info.param_count > info.local_count {
                errors.push(VerificationError::new(
                    Some(info.entry_pc),
                    format!(
                        "function '{}' has {} params but only {} local slots",
                        info.name, info.param_count, info.local_count
                    ),
                ));
            }

            if info.local_count > limits::MAX_LOCAL_SLOTS {
                errors.push(VerificationError::new(
                    Some(info.entry_pc),
                    format!(
                        "function '{}' needs {} local slots, limit is {}",
                        info.name,
                        info.local_count,
                        limits::MAX_LOCAL_SLOTS
                    ),
                ));
            }

            if info.entry_pc >= program.instructions.len() {
                errors.push(VerificationError::new(
                    Some(info.entry_pc),
                    format!("function '{}' entry PC is out of bounds", info.name),
                ));
                continue;
            }

            if !seen_entries.insert(info.entry_pc) {
                errors.push(VerificationError::new(
                    Some(info.entry_pc),
                    format!("duplicate function entry PC {}", info.entry_pc),
                ));
            }

            entries.push((info.entry_pc, *id, info.clone()));
        }

        entries.sort_by_key(|(entry_pc, _, _)| *entry_pc);

        let mut ranges = entries
            .windows(2)
            .map(|window| {
                let (start, id, info) = &window[0];
                let end = window[1].0;

                FunctionRange {
                    id: *id,
                    info: info.clone(),
                    start: *start,
                    end,
                }
            })
            .collect::<Vec<_>>();

        if let Some((start, id, info)) = entries.last() {
            ranges.push(FunctionRange {
                id: *id,
                info: info.clone(),
                start: *start,
                end: program.instructions.len(),
            });
        }

        ranges
    }

    fn verify_function(&self, program: &CompiledProgram, range: &FunctionRange) -> FunctionCheck {
        let mut errors = Vec::new();
        let mut possible_traps = Vec::new();
        let mut call_edges = Vec::new();
        let mut states = HashMap::<usize, AbstractState>::new();
        let mut reachable = HashSet::<usize>::new();
        let mut worklist = VecDeque::new();
        let mut max_stack_depth = 0usize;

        let mut initialized_locals = vec![false; range.info.local_count];
        for initialized in initialized_locals.iter_mut().take(range.info.param_count) {
            *initialized = true;
        }

        states.insert(
            range.start,
            AbstractState {
                stack_depth: 0,
                initialized_locals,
            },
        );
        worklist.push_back(range.start);

        while let Some(pc) = worklist.pop_front() {
            if !pc_in_range(range, pc) {
                errors.push(VerificationError::new(
                    Some(pc),
                    format!(
                        "control flow in function '{}' leaves range {}..{}",
                        range.info.name, range.start, range.end
                    ),
                ));
                continue;
            }

            let state = states[&pc].clone();
            reachable.insert(pc);
            max_stack_depth = max_stack_depth.max(state.stack_depth);

            let instr = &program.instructions[pc];
            let mut step_context = StepContext {
                program,
                range,
                pc,
                errors: &mut errors,
                possible_traps: &mut possible_traps,
            };
            let step = self.step_instruction(&mut step_context, instr, state);

            let Some(step) = step else {
                continue;
            };

            max_stack_depth = max_stack_depth.max(step.state.stack_depth);
            for edge in step.call_edges {
                call_edges.push(edge);
            }

            if step.state.stack_depth > limits::MAX_OPERAND_STACK {
                errors.push(VerificationError::new(
                    Some(pc),
                    format!(
                        "operand stack depth {} exceeds limit {}",
                        step.state.stack_depth,
                        limits::MAX_OPERAND_STACK
                    ),
                ));
            }

            for successor in step.successors {
                if !pc_in_range(range, successor) {
                    errors.push(VerificationError::new(
                        Some(pc),
                        format!(
                            "successor PC {} leaves function '{}' range {}..{}",
                            successor, range.info.name, range.start, range.end
                        ),
                    ));
                    continue;
                }

                match states.get_mut(&successor) {
                    Some(existing) => {
                        if existing.stack_depth != step.state.stack_depth {
                            errors.push(VerificationError::new(
                                Some(successor),
                                format!(
                                    "inconsistent stack depth at PC {}: saw {} and {}",
                                    successor, existing.stack_depth, step.state.stack_depth
                                ),
                            ));
                            continue;
                        }

                        let mut changed = false;
                        for (dst, src) in existing
                            .initialized_locals
                            .iter_mut()
                            .zip(&step.state.initialized_locals)
                        {
                            let merged = *dst && *src;
                            if *dst != merged {
                                *dst = merged;
                                changed = true;
                            }
                        }

                        if changed {
                            worklist.push_back(successor);
                        }
                    }
                    None => {
                        states.insert(successor, step.state.clone());
                        worklist.push_back(successor);
                    }
                }
            }
        }

        FunctionCheck {
            summary: FunctionVerification {
                id: range.id,
                name: range.info.name.clone(),
                entry_pc: range.start,
                end_pc: range.end,
                reachable_instructions: reachable.len(),
                max_stack_depth,
            },
            possible_traps,
            call_edges,
            errors,
        }
    }

    fn step_instruction(
        &self,
        cx: &mut StepContext<'_, '_>,
        instr: &Instruction,
        mut state: AbstractState,
    ) -> Option<Step> {
        let mut successors = Vec::new();
        let mut call_edges = Vec::new();
        let mut terminal = false;

        match instr.opcode {
            Opcode::LoadConst => {
                state.stack_depth += 1;
            }
            Opcode::LoadLocal => {
                let slot = self.local_slot(cx.range, cx.pc, instr.arg1, cx.errors)?;
                if !state.initialized_locals[slot] {
                    push_trap(cx.possible_traps, TrapCode::UndefinedLocal);
                }
                state.stack_depth += 1;
            }
            Opcode::StoreLocal => {
                self.pop_stack(cx.pc, &mut state, 1, cx.errors)?;
                let slot = self.local_slot(cx.range, cx.pc, instr.arg1, cx.errors)?;
                state.initialized_locals[slot] = true;
            }
            Opcode::LoadGlobal => {
                self.global_slot(cx.pc, instr.arg1, cx.errors)?;
                state.stack_depth += 1;
            }
            Opcode::StoreGlobal => {
                self.pop_stack(cx.pc, &mut state, 1, cx.errors)?;
                self.global_slot(cx.pc, instr.arg1, cx.errors)?;
            }
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Eq
            | Opcode::Ne
            | Opcode::Lt
            | Opcode::Gt
            | Opcode::Le
            | Opcode::Ge
            | Opcode::And
            | Opcode::Or => {
                self.pop_stack(cx.pc, &mut state, 2, cx.errors)?;
                state.stack_depth += 1;
            }
            Opcode::Div => {
                self.pop_stack(cx.pc, &mut state, 2, cx.errors)?;
                state.stack_depth += 1;
                push_trap(cx.possible_traps, TrapCode::DivideByZero);
            }
            Opcode::Neg | Opcode::Not => {
                self.pop_stack(cx.pc, &mut state, 1, cx.errors)?;
                state.stack_depth += 1;
            }
            Opcode::Jump => {
                successors.push(self.jump_target(cx.range, cx.pc, instr.arg1, cx.errors)?);
                if (instr.arg1 as usize) <= cx.pc {
                    push_trap(cx.possible_traps, TrapCode::CycleLimit);
                }
                terminal = true;
            }
            Opcode::JumpIfFalse | Opcode::JumpIfTrue => {
                self.pop_stack(cx.pc, &mut state, 1, cx.errors)?;
                let target = self.jump_target(cx.range, cx.pc, instr.arg1, cx.errors)?;
                if target <= cx.pc {
                    push_trap(cx.possible_traps, TrapCode::CycleLimit);
                }
                successors.push(target);
                successors.push(cx.pc + 1);
                terminal = true;
            }
            Opcode::Call => {
                if instr.arg1 < 0 {
                    cx.errors.push(VerificationError::new(
                        Some(cx.pc),
                        format!("negative function id {}", instr.arg1),
                    ));
                    return None;
                }
                if instr.arg2 < 0 {
                    cx.errors.push(VerificationError::new(
                        Some(cx.pc),
                        format!("negative call argument count {}", instr.arg2),
                    ));
                    return None;
                }

                let callee_id = instr.arg1 as usize;
                let argc = instr.arg2 as usize;
                let Some(callee) = cx.program.functions.get(&callee_id) else {
                    cx.errors.push(VerificationError::new(
                        Some(cx.pc),
                        format!("call to undefined function id {}", callee_id),
                    ));
                    return None;
                };

                if argc != callee.param_count {
                    cx.errors.push(VerificationError::new(
                        Some(cx.pc),
                        format!(
                            "call to '{}' has {} args but expects {}",
                            callee.name, argc, callee.param_count
                        ),
                    ));
                    return None;
                }

                self.pop_stack(cx.pc, &mut state, argc, cx.errors)?;
                state.stack_depth += 1;
                call_edges.push((cx.range.id, callee_id));
            }
            Opcode::Return => {
                self.pop_stack(cx.pc, &mut state, 1, cx.errors)?;
                terminal = true;
            }
            Opcode::ArrayLoad => {
                self.global_array_range(cx.pc, instr.arg1, instr.arg2, cx.errors)?;
                self.pop_stack(cx.pc, &mut state, 1, cx.errors)?;
                state.stack_depth += 1;
                push_trap(cx.possible_traps, TrapCode::ArrayOutOfBounds);
            }
            Opcode::ArrayStore => {
                self.global_array_range(cx.pc, instr.arg1, instr.arg2, cx.errors)?;
                self.pop_stack(cx.pc, &mut state, 2, cx.errors)?;
                push_trap(cx.possible_traps, TrapCode::ArrayOutOfBounds);
            }
            Opcode::LocalArrayLoad => {
                self.array_size(cx.pc, instr.arg2, cx.errors)?;
                let slot = self.local_slot(cx.range, cx.pc, instr.arg1, cx.errors)?;
                if !state.initialized_locals[slot] {
                    push_trap(cx.possible_traps, TrapCode::UndefinedLocal);
                }
                self.pop_stack(cx.pc, &mut state, 1, cx.errors)?;
                state.stack_depth += 1;
                push_trap(cx.possible_traps, TrapCode::ArrayOutOfBounds);
            }
            Opcode::LocalArrayStore => {
                self.array_size(cx.pc, instr.arg2, cx.errors)?;
                let slot = self.local_slot(cx.range, cx.pc, instr.arg1, cx.errors)?;
                if !state.initialized_locals[slot] {
                    push_trap(cx.possible_traps, TrapCode::UndefinedLocal);
                }
                self.pop_stack(cx.pc, &mut state, 2, cx.errors)?;
                push_trap(cx.possible_traps, TrapCode::ArrayOutOfBounds);
            }
            Opcode::ArrayNew | Opcode::AllocArray => {
                self.array_size(cx.pc, instr.arg1, cx.errors)?;
                state.stack_depth += 1;
            }
            Opcode::Print | Opcode::Pop => {
                self.pop_stack(cx.pc, &mut state, 1, cx.errors)?;
            }
            Opcode::Dup => {
                if state.stack_depth == 0 {
                    cx.errors.push(VerificationError::new(
                        Some(cx.pc),
                        "stack underflow on dup",
                    ));
                    return None;
                }
                state.stack_depth += 1;
            }
            Opcode::Halt => {
                terminal = true;
            }
        }

        if !terminal {
            successors.push(cx.pc + 1);
        }

        Some(Step {
            state,
            successors,
            call_edges,
        })
    }

    fn pop_stack(
        &self,
        pc: usize,
        state: &mut AbstractState,
        count: usize,
        errors: &mut Vec<VerificationError>,
    ) -> Option<()> {
        if state.stack_depth < count {
            errors.push(VerificationError::new(
                Some(pc),
                format!(
                    "stack underflow: instruction needs {} value(s), stack has {}",
                    count, state.stack_depth
                ),
            ));
            return None;
        }
        state.stack_depth -= count;
        Some(())
    }

    fn local_slot(
        &self,
        range: &FunctionRange,
        pc: usize,
        raw_slot: i32,
        errors: &mut Vec<VerificationError>,
    ) -> Option<usize> {
        if raw_slot < 0 {
            errors.push(VerificationError::new(
                Some(pc),
                format!("negative local slot {}", raw_slot),
            ));
            return None;
        }

        let slot = raw_slot as usize;
        if slot >= range.info.local_count {
            errors.push(VerificationError::new(
                Some(pc),
                format!(
                    "local slot {} is outside function '{}' local count {}",
                    slot, range.info.name, range.info.local_count
                ),
            ));
            return None;
        }

        Some(slot)
    }

    fn global_slot(
        &self,
        pc: usize,
        raw_slot: i32,
        errors: &mut Vec<VerificationError>,
    ) -> Option<usize> {
        if raw_slot < 0 {
            errors.push(VerificationError::new(
                Some(pc),
                format!("negative global slot {}", raw_slot),
            ));
            return None;
        }

        let slot = raw_slot as usize;
        if slot >= limits::MAX_GLOBAL_SLOTS {
            errors.push(VerificationError::new(
                Some(pc),
                format!(
                    "global slot {} exceeds limit {}",
                    slot,
                    limits::MAX_GLOBAL_SLOTS
                ),
            ));
            return None;
        }

        Some(slot)
    }

    fn array_size(
        &self,
        pc: usize,
        raw_size: i32,
        errors: &mut Vec<VerificationError>,
    ) -> Option<usize> {
        if raw_size <= 0 {
            errors.push(VerificationError::new(
                Some(pc),
                format!("array size must be positive, got {}", raw_size),
            ));
            return None;
        }

        Some(raw_size as usize)
    }

    fn global_array_range(
        &self,
        pc: usize,
        raw_slot: i32,
        raw_size: i32,
        errors: &mut Vec<VerificationError>,
    ) -> Option<(usize, usize)> {
        let slot = self.global_slot(pc, raw_slot, errors)?;
        let size = self.array_size(pc, raw_size, errors)?;
        let end = slot.saturating_add(size);
        if end > limits::MAX_GLOBAL_SLOTS {
            errors.push(VerificationError::new(
                Some(pc),
                format!(
                    "global array range {}..{} exceeds limit {}",
                    slot,
                    end,
                    limits::MAX_GLOBAL_SLOTS
                ),
            ));
            return None;
        }
        Some((slot, size))
    }

    fn jump_target(
        &self,
        range: &FunctionRange,
        pc: usize,
        raw_target: i32,
        errors: &mut Vec<VerificationError>,
    ) -> Option<usize> {
        if raw_target < 0 {
            errors.push(VerificationError::new(
                Some(pc),
                format!("negative jump target {}", raw_target),
            ));
            return None;
        }

        let target = raw_target as usize;
        if !pc_in_range(range, target) {
            errors.push(VerificationError::new(
                Some(pc),
                format!(
                    "jump target {} is outside function '{}' range {}..{}",
                    target, range.info.name, range.start, range.end
                ),
            ));
            return None;
        }

        Some(target)
    }

    fn max_frame_depth(
        &self,
        main_func_id: usize,
        call_edges: &HashMap<usize, Vec<usize>>,
    ) -> Option<usize> {
        fn visit(
            func_id: usize,
            call_edges: &HashMap<usize, Vec<usize>>,
            visiting: &mut HashSet<usize>,
            memo: &mut HashMap<usize, Option<usize>>,
        ) -> Option<usize> {
            if let Some(depth) = memo.get(&func_id) {
                return *depth;
            }

            if !visiting.insert(func_id) {
                memo.insert(func_id, None);
                return None;
            }

            let mut max_child_depth = 0usize;
            for callee in call_edges.get(&func_id).into_iter().flatten() {
                let child_depth = visit(*callee, call_edges, visiting, memo)?;
                max_child_depth = max_child_depth.max(child_depth);
            }

            visiting.remove(&func_id);
            let depth = Some(max_child_depth + 1);
            memo.insert(func_id, depth);
            depth
        }

        visit(
            main_func_id,
            call_edges,
            &mut HashSet::new(),
            &mut HashMap::new(),
        )
    }

    fn backend_eligibility(&self, program: &CompiledProgram, valid: bool) -> BackendEligibility {
        if !valid {
            let rejected = BackendStatus::no("verification errors");
            return BackendEligibility {
                vm: rejected.clone(),
                gc_vm: rejected.clone(),
                optimized_vm: rejected.clone(),
                jit: rejected,
            };
        }

        BackendEligibility {
            vm: BackendStatus::yes(),
            gc_vm: BackendStatus::yes(),
            optimized_vm: BackendStatus::yes(),
            jit: self.jit_status(program),
        }
    }

    fn jit_status(&self, program: &CompiledProgram) -> BackendStatus {
        jit_status_for_target(program)
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn jit_status_for_target(program: &CompiledProgram) -> BackendStatus {
    if program.functions.len() != 1 {
        return BackendStatus::no("JIT requires exactly one function");
    }

    let Some(main_func) = program.functions.get(&program.main_func_id) else {
        return BackendStatus::no("missing main function metadata");
    };

    let mut saw_return = false;
    for instr in program.instructions.iter().skip(main_func.entry_pc) {
        if !jit_supports_opcode(instr.opcode) {
            return BackendStatus::no(format!("unsupported opcode {:?}", instr.opcode));
        }
        if matches!(instr.opcode, Opcode::Return) {
            saw_return = true;
            break;
        }
    }

    if saw_return {
        BackendStatus::yes()
    } else {
        BackendStatus::no("main function has no return")
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn jit_status_for_target(_program: &CompiledProgram) -> BackendStatus {
    BackendStatus::no("current build target is not linux x86-64")
}

impl std::fmt::Display for VerificationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Verification Report")?;
        writeln!(
            f,
            "  status: {}",
            if self.valid { "valid" } else { "invalid" }
        )?;
        writeln!(f, "  max stack depth: {}", self.max_stack_depth)?;
        match self.max_frame_depth {
            Some(depth) => writeln!(f, "  max frame depth: {}", depth)?,
            None => writeln!(f, "  max frame depth: recursive/unbounded")?,
        }

        if self.possible_traps.is_empty() {
            writeln!(f, "  possible traps: none")?;
        } else {
            let traps = self
                .possible_traps
                .iter()
                .map(|trap| format!("{:?}", trap))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(f, "  possible traps: {}", traps)?;
        }

        writeln!(f)?;
        writeln!(f, "Backends")?;
        write_backend(f, "VM", &self.backend_eligibility.vm)?;
        write_backend(f, "GC VM", &self.backend_eligibility.gc_vm)?;
        write_backend(f, "Optimized VM", &self.backend_eligibility.optimized_vm)?;
        write_backend(f, "JIT", &self.backend_eligibility.jit)?;

        writeln!(f)?;
        writeln!(f, "Functions")?;
        for func in &self.functions {
            writeln!(
                f,
                "  {}#{}: entry={}, range={}..{}, reachable={}, max_stack={}",
                func.name,
                func.id,
                func.entry_pc,
                func.entry_pc,
                func.end_pc,
                func.reachable_instructions,
                func.max_stack_depth
            )?;
        }

        if !self.errors.is_empty() {
            writeln!(f)?;
            writeln!(f, "Errors")?;
            for error in &self.errors {
                match error.pc {
                    Some(pc) => writeln!(f, "  [pc {}] {}", pc, error.message)?,
                    None => writeln!(f, "  {}", error.message)?,
                }
            }
        }

        Ok(())
    }
}

fn write_backend(
    f: &mut std::fmt::Formatter<'_>,
    name: &str,
    status: &BackendStatus,
) -> std::fmt::Result {
    if status.eligible {
        writeln!(f, "  {}: yes", name)
    } else {
        writeln!(
            f,
            "  {}: no ({})",
            name,
            status.reason.as_deref().unwrap_or("unknown reason")
        )
    }
}

fn jit_supports_opcode(opcode: Opcode) -> bool {
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

fn push_trap(traps: &mut Vec<TrapCode>, trap: TrapCode) {
    if !traps.contains(&trap) {
        traps.push(trap);
    }
}

fn pc_in_range(range: &FunctionRange, pc: usize) -> bool {
    (range.start..range.end).contains(&pc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{Compiler, Instruction};
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::sema::SemanticAnalyzer;

    fn compile_source(source: &str) -> CompiledProgram {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().expect("parse failed");
        let mut analyzer = SemanticAnalyzer::new();
        analyzer.analyze(&program).expect("sema failed");
        Compiler::new().compile(&program).0
    }

    fn single_function(instructions: Vec<Instruction>, local_count: usize) -> CompiledProgram {
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

        CompiledProgram {
            instructions,
            functions,
            globals: HashMap::new(),
            main_func_id: 0,
            constants: Vec::new(),
        }
    }

    #[test]
    fn verifies_compiled_program() {
        let program = compile_source("func main() { int x = 1 + 2; return x; }");
        let report = Verifier::new().verify(&program);

        assert!(report.valid, "{:#?}", report.errors);
        assert!(report.max_stack_depth > 0);
        assert_eq!(report.functions.len(), 1);
    }

    #[test]
    fn rejects_stack_underflow() {
        let program = single_function(
            vec![
                Instruction::simple(Opcode::Add),
                Instruction::simple(Opcode::Return),
            ],
            0,
        );
        let report = Verifier::new().verify(&program);

        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|error| error.message.contains("stack underflow")));
    }

    #[test]
    fn rejects_bad_jump_target() {
        let program = single_function(
            vec![
                Instruction::new(Opcode::LoadConst, 1, 0),
                Instruction::new(Opcode::JumpIfFalse, 99, 0),
                Instruction::new(Opcode::LoadConst, 0, 0),
                Instruction::simple(Opcode::Return),
            ],
            0,
        );
        let report = Verifier::new().verify(&program);

        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|error| error.message.contains("jump target")));
    }

    #[test]
    fn reports_possible_undefined_local_trap() {
        let program = compile_source("func main() { int x; return x; }");
        let report = Verifier::new().verify(&program);

        assert!(report.valid, "{:#?}", report.errors);
        assert!(report.possible_traps.contains(&TrapCode::UndefinedLocal));
    }

    #[test]
    fn reports_recursive_frame_depth_as_unbounded() {
        let program =
            compile_source("func f(int n) { return f(n + 1); } func main() { return f(0); }");
        let report = Verifier::new().verify(&program);

        assert!(report.valid, "{:#?}", report.errors);
        assert_eq!(report.max_frame_depth, None);
        assert!(report.possible_traps.contains(&TrapCode::StackOverflow));
    }
}
