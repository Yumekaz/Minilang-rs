//! Independent AST interpreter used as a source-level oracle.
//!
//! This interpreter executes the parsed AST directly. It deliberately does not
//! reuse bytecode or VM execution, which lets Qydrel compare the compiler output
//! against a second implementation of the language semantics.

use crate::ast::{BinaryOp, Expr, Function, Program, Stmt, UnaryOp};
use crate::compare::{compare_backends, BackendComparisonReport, BackendRunStatus};
use crate::compiler::CompiledProgram;
use crate::limits;
use crate::vm::TrapCode;
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleOutcome {
    pub success: bool,
    pub return_value: i64,
    pub trap_code: TrapCode,
    pub trap_message: String,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleComparisonReport {
    pub equivalent: bool,
    pub oracle: OracleOutcome,
    pub backend_report: BackendComparisonReport,
    pub mismatches: Vec<String>,
}

#[derive(Debug, Clone)]
enum Slot {
    Scalar(Option<i64>),
    Array(Vec<i64>),
}

#[derive(Debug, Clone)]
struct Frame {
    scopes: Vec<HashMap<String, Slot>>,
}

#[derive(Debug, Clone)]
enum Control {
    Continue,
    Return(i64),
}

struct AstOracle<'a> {
    program: &'a Program,
    functions: HashMap<&'a str, &'a Function>,
    globals: HashMap<String, Slot>,
    frames: Vec<Frame>,
    output: Vec<String>,
    cycles: u64,
}

pub fn run_ast_oracle(program: &Program) -> OracleOutcome {
    AstOracle::new(program).run()
}

pub fn compare_ast_oracle(program: &Program, compiled: &CompiledProgram) -> OracleComparisonReport {
    let oracle = run_ast_oracle(program);
    let backend_report = compare_backends(compiled);
    let mut mismatches = Vec::new();

    for run in &backend_report.runs {
        let BackendRunStatus::Executed(outcome) = &run.status else {
            continue;
        };

        if outcome.success != oracle.success
            || outcome.return_value != oracle.return_value
            || outcome.trap_code != oracle.trap_code
            || outcome.output != oracle.output
        {
            mismatches.push(format!(
                "{} differs from AST oracle: success={}, return={}, trap={:?}, output={:?} vs {}",
                run.name,
                outcome.success,
                outcome.return_value,
                outcome.trap_code,
                outcome.output,
                oracle.summary()
            ));
        }
    }

    OracleComparisonReport {
        equivalent: backend_report.equivalent && mismatches.is_empty(),
        oracle,
        backend_report,
        mismatches,
    }
}

impl OracleOutcome {
    fn ok(return_value: i64, output: Vec<String>) -> Self {
        Self {
            success: true,
            return_value,
            trap_code: TrapCode::None,
            trap_message: String::new(),
            output,
        }
    }

    fn trap(trap_code: TrapCode, trap_message: impl Into<String>, output: Vec<String>) -> Self {
        Self {
            success: false,
            return_value: 0,
            trap_code,
            trap_message: trap_message.into(),
            output,
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "success={}, return={}, trap={:?}, output={:?}",
            self.success, self.return_value, self.trap_code, self.output
        )
    }
}

impl<'a> AstOracle<'a> {
    fn new(program: &'a Program) -> Self {
        let functions = program
            .functions
            .iter()
            .map(|function| (function.name.as_str(), function))
            .collect();

        let mut globals = HashMap::new();
        for global in &program.globals {
            let slot = match global.array_size {
                Some(size) => Slot::Array(vec![0; size as usize]),
                None => Slot::Scalar(Some(0)),
            };
            globals.insert(global.name.clone(), slot);
        }

        Self {
            program,
            functions,
            globals,
            frames: Vec::new(),
            output: Vec::new(),
            cycles: 0,
        }
    }

    fn run(mut self) -> OracleOutcome {
        let result = self
            .initialize_globals()
            .and_then(|_| self.call_function("main", &[]));

        match result {
            Ok(return_value) => OracleOutcome::ok(return_value, self.output),
            Err((trap_code, message)) => OracleOutcome::trap(trap_code, message, self.output),
        }
    }

    fn initialize_globals(&mut self) -> Result<(), (TrapCode, String)> {
        for global in self.program.globals.clone() {
            if let Some(init_expr) = &global.init_expr {
                let value = self.eval_expr(init_expr)?;
                self.assign_global_scalar(&global.name, value)?;
            }
        }
        Ok(())
    }

    fn call_function(&mut self, name: &str, args: &[i64]) -> Result<i64, (TrapCode, String)> {
        self.tick()?;
        if self.frames.len() >= limits::MAX_FRAMES {
            return Err((
                TrapCode::StackOverflow,
                format!("Call stack overflow (max {} frames)", limits::MAX_FRAMES),
            ));
        }

        let function = (**self.functions.get(name).ok_or((
            TrapCode::UndefinedFunction,
            format!("Undefined function {}", name),
        ))?)
        .clone();

        if args.len() != function.params.len() {
            return Err((
                TrapCode::InvalidInstruction,
                format!(
                    "Call has {} arguments but function {} expects {}",
                    args.len(),
                    name,
                    function.params.len()
                ),
            ));
        }

        let mut root_scope = HashMap::new();
        for (param, value) in function.params.iter().zip(args.iter()) {
            root_scope.insert(
                param.name.clone(),
                Slot::Scalar(Some(normalize_i32(*value))),
            );
        }
        self.frames.push(Frame {
            scopes: vec![root_scope],
        });

        let result = match self.exec_block(&function.body, false)? {
            Control::Return(value) => value,
            Control::Continue => 0,
        };
        self.frames.pop();
        Ok(normalize_i32(result))
    }

    fn exec_block(&mut self, stmts: &[Stmt], _scoped: bool) -> Result<Control, (TrapCode, String)> {
        for stmt in stmts {
            match self.exec_stmt(stmt) {
                Ok(Control::Continue) => {}
                other => {
                    return other;
                }
            }
        }

        Ok(Control::Continue)
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<Control, (TrapCode, String)> {
        self.tick()?;
        match stmt {
            Stmt::VarDecl {
                name,
                init_expr,
                array_size,
                ..
            } => {
                if let Some(size) = array_size {
                    self.declare_or_update(name, Slot::Array(vec![0; *size as usize]))?;
                } else if let Some(expr) = init_expr {
                    let value = self.eval_expr(expr)?;
                    self.declare_or_update(name, Slot::Scalar(Some(value)))?;
                } else if self.lookup_slot(name).is_none() {
                    self.current_scope_mut()?
                        .insert(name.clone(), Slot::Scalar(None));
                }
                Ok(Control::Continue)
            }
            Stmt::Assign {
                target,
                index_expr,
                value,
                ..
            } => {
                let value = self.eval_expr(value)?;
                match index_expr {
                    Some(index_expr) => {
                        let index = checked_array_index(self.eval_expr(index_expr)?)?;
                        self.assign_array(target, index, value)?;
                    }
                    None => self.assign_scalar(target, value)?,
                }
                Ok(Control::Continue)
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                if self.eval_expr(condition)? != 0 {
                    self.exec_block(then_body, true)
                } else if let Some(else_body) = else_body {
                    self.exec_block(else_body, true)
                } else {
                    Ok(Control::Continue)
                }
            }
            Stmt::While {
                condition, body, ..
            } => {
                while self.eval_expr(condition)? != 0 {
                    match self.exec_block(body, true)? {
                        Control::Continue => {}
                        Control::Return(value) => return Ok(Control::Return(value)),
                    }
                }
                Ok(Control::Continue)
            }
            Stmt::Return { value, .. } => Ok(Control::Return(self.eval_expr(value)?)),
            Stmt::Print { value, .. } => {
                let value = self.eval_expr(value)?;
                self.output.push(format!("OUTPUT: {}", value));
                Ok(Control::Continue)
            }
            Stmt::ExprStmt { expr, .. } => {
                let _ = self.eval_expr(expr)?;
                Ok(Control::Continue)
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<i64, (TrapCode, String)> {
        self.tick()?;
        match expr {
            Expr::IntLiteral { value, .. } => Ok(i64::from(*value)),
            Expr::BoolLiteral { value, .. } => Ok(bool_value(*value)),
            Expr::Identifier { name, .. } => self.read_scalar(name),
            Expr::Binary {
                op, left, right, ..
            } => match op {
                BinaryOp::And => {
                    let left = self.eval_expr(left)?;
                    if left == 0 {
                        Ok(0)
                    } else {
                        Ok(bool_value(self.eval_expr(right)? != 0))
                    }
                }
                BinaryOp::Or => {
                    let left = self.eval_expr(left)?;
                    if left != 0 {
                        Ok(1)
                    } else {
                        Ok(bool_value(self.eval_expr(right)? != 0))
                    }
                }
                _ => {
                    let left = self.eval_expr(left)?;
                    let right = self.eval_expr(right)?;
                    self.eval_binary(*op, left, right)
                }
            },
            Expr::Unary { op, operand, .. } => {
                let operand = self.eval_expr(operand)?;
                match op {
                    UnaryOp::Neg => Ok(normalize_i32(-operand)),
                    UnaryOp::Not => Ok(bool_value(operand == 0)),
                }
            }
            Expr::Call { name, args, .. } => {
                let values = args
                    .iter()
                    .map(|arg| self.eval_expr(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call_function(name, &values)
            }
            Expr::ArrayIndex {
                array_name, index, ..
            } => {
                let index = checked_array_index(self.eval_expr(index)?)?;
                self.read_array(array_name, index)
            }
        }
    }

    fn eval_binary(&self, op: BinaryOp, left: i64, right: i64) -> Result<i64, (TrapCode, String)> {
        match op {
            BinaryOp::Add => Ok(normalize_i32(left.wrapping_add(right))),
            BinaryOp::Sub => Ok(normalize_i32(left.wrapping_sub(right))),
            BinaryOp::Mul => Ok(normalize_i32(left.wrapping_mul(right))),
            BinaryOp::Div => {
                if right == 0 {
                    Err((TrapCode::DivideByZero, "Division by zero".to_string()))
                } else {
                    Ok(normalize_i32(left / right))
                }
            }
            BinaryOp::Eq => Ok(bool_value(left == right)),
            BinaryOp::Ne => Ok(bool_value(left != right)),
            BinaryOp::Lt => Ok(bool_value(left < right)),
            BinaryOp::Gt => Ok(bool_value(left > right)),
            BinaryOp::Le => Ok(bool_value(left <= right)),
            BinaryOp::Ge => Ok(bool_value(left >= right)),
            BinaryOp::And | BinaryOp::Or => unreachable!("short-circuited before eval_binary"),
        }
    }

    fn read_scalar(&self, name: &str) -> Result<i64, (TrapCode, String)> {
        match self.lookup_slot(name) {
            Some(Slot::Scalar(Some(value))) => Ok(*value),
            Some(Slot::Scalar(None)) => Err((
                TrapCode::UndefinedLocal,
                format!("Undefined local {}", name),
            )),
            Some(Slot::Array(_)) => Err((
                TrapCode::InvalidInstruction,
                format!("Cannot use array {} without index", name),
            )),
            None => Err((
                TrapCode::InvalidInstruction,
                format!("Undefined variable {}", name),
            )),
        }
    }

    fn read_array(&self, name: &str, index: usize) -> Result<i64, (TrapCode, String)> {
        match self.lookup_slot(name) {
            Some(Slot::Array(values)) => values.get(index).copied().ok_or_else(|| {
                (
                    TrapCode::ArrayOutOfBounds,
                    format!(
                        "Array index {} out of bounds (size {})",
                        index,
                        values.len()
                    ),
                )
            }),
            Some(Slot::Scalar(_)) => Err((
                TrapCode::InvalidInstruction,
                format!("Cannot index non-array {}", name),
            )),
            None => Err((
                TrapCode::InvalidInstruction,
                format!("Undefined variable {}", name),
            )),
        }
    }

    fn assign_scalar(&mut self, name: &str, value: i64) -> Result<(), (TrapCode, String)> {
        let value = normalize_i32(value);
        if let Some(slot) = self.lookup_slot_mut(name) {
            match slot {
                Slot::Scalar(existing) => {
                    *existing = Some(value);
                    Ok(())
                }
                Slot::Array(_) => Err((
                    TrapCode::InvalidInstruction,
                    format!("Cannot assign array {} without index", name),
                )),
            }
        } else {
            Err((
                TrapCode::InvalidInstruction,
                format!("Undefined variable {}", name),
            ))
        }
    }

    fn assign_global_scalar(&mut self, name: &str, value: i64) -> Result<(), (TrapCode, String)> {
        match self.globals.get_mut(name) {
            Some(Slot::Scalar(existing)) => {
                *existing = Some(normalize_i32(value));
                Ok(())
            }
            Some(Slot::Array(_)) => Err((
                TrapCode::InvalidInstruction,
                format!("Cannot initialize array {} as scalar", name),
            )),
            None => Err((
                TrapCode::InvalidInstruction,
                format!("Undefined global {}", name),
            )),
        }
    }

    fn assign_array(
        &mut self,
        name: &str,
        index: usize,
        value: i64,
    ) -> Result<(), (TrapCode, String)> {
        if let Some(slot) = self.lookup_slot_mut(name) {
            match slot {
                Slot::Array(values) => {
                    let len = values.len();
                    let Some(element) = values.get_mut(index) else {
                        return Err((
                            TrapCode::ArrayOutOfBounds,
                            format!("Array index {} out of bounds (size {})", index, len),
                        ));
                    };
                    *element = normalize_i32(value);
                    Ok(())
                }
                Slot::Scalar(_) => Err((
                    TrapCode::InvalidInstruction,
                    format!("Cannot index non-array {}", name),
                )),
            }
        } else {
            Err((
                TrapCode::InvalidInstruction,
                format!("Undefined variable {}", name),
            ))
        }
    }

    fn declare_or_update(&mut self, name: &str, slot: Slot) -> Result<(), (TrapCode, String)> {
        if let Some(existing) = self.lookup_slot_mut(name) {
            *existing = slot;
        } else {
            self.current_scope_mut()?.insert(name.to_string(), slot);
        }
        Ok(())
    }

    fn lookup_slot(&self, name: &str) -> Option<&Slot> {
        for frame in self.frames.iter().rev() {
            for scope in frame.scopes.iter().rev() {
                if let Some(slot) = scope.get(name) {
                    return Some(slot);
                }
            }
        }
        self.globals.get(name)
    }

    fn lookup_slot_mut(&mut self, name: &str) -> Option<&mut Slot> {
        for frame in self.frames.iter_mut().rev() {
            for scope in frame.scopes.iter_mut().rev() {
                if let Some(slot) = scope.get_mut(name) {
                    return Some(slot);
                }
            }
        }
        self.globals.get_mut(name)
    }

    fn current_frame_mut(&mut self) -> Result<&mut Frame, (TrapCode, String)> {
        self.frames.last_mut().ok_or((
            TrapCode::InvalidInstruction,
            "No active call frame".to_string(),
        ))
    }

    fn current_scope_mut(&mut self) -> Result<&mut HashMap<String, Slot>, (TrapCode, String)> {
        self.current_frame_mut()?
            .scopes
            .last_mut()
            .ok_or((TrapCode::InvalidInstruction, "No active scope".to_string()))
    }

    fn tick(&mut self) -> Result<(), (TrapCode, String)> {
        self.cycles = self.cycles.saturating_add(1);
        if self.cycles > limits::MAX_CYCLES {
            Err((
                TrapCode::CycleLimit,
                format!("Cycle limit exceeded ({})", limits::MAX_CYCLES),
            ))
        } else {
            Ok(())
        }
    }
}

fn checked_array_index(raw_index: i64) -> Result<usize, (TrapCode, String)> {
    if raw_index < 0 {
        return Err((
            TrapCode::ArrayOutOfBounds,
            format!("Array index {} out of bounds", raw_index),
        ));
    }
    Ok(raw_index as usize)
}

fn bool_value(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

#[inline]
fn normalize_i32(value: i64) -> i64 {
    let masked = value & 0xFFFF_FFFF;
    if masked > 0x7FFF_FFFF {
        masked - 0x1_0000_0000
    } else {
        masked
    }
}

impl fmt::Display for OracleComparisonReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "AST Oracle Comparison")?;
        writeln!(
            f,
            "  status: {}",
            if self.equivalent {
                "equivalent"
            } else {
                "mismatch"
            }
        )?;
        writeln!(f, "  oracle: {}", self.oracle.summary())?;
        writeln!(f)?;
        write!(f, "{}", self.backend_report)?;

        if !self.mismatches.is_empty() {
            writeln!(f)?;
            writeln!(f, "Oracle Mismatches")?;
            for mismatch in &self.mismatches {
                writeln!(f, "  {}", mismatch)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Compiler, Lexer, Parser, SemanticAnalyzer};

    fn parse_and_compile(source: &str) -> (Program, CompiledProgram) {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().expect("parse failed");
        SemanticAnalyzer::new()
            .analyze(&program)
            .expect("semantic analysis failed");
        let compiled = Compiler::new().compile(&program).0;
        (program, compiled)
    }

    #[test]
    fn oracle_executes_scalars_functions_and_arrays() {
        let (program, compiled) = parse_and_compile(
            "int g = 2;\n\
             int arr[3];\n\
             func add(int x) { return x + g; }\n\
             func main() {\n\
               int local[2];\n\
               local[0] = add(5);\n\
               arr[1] = local[0] + 3;\n\
               print arr[1];\n\
               return arr[1];\n\
             }\n",
        );

        let report = compare_ast_oracle(&program, &compiled);
        assert!(report.equivalent, "{report:#?}");
        assert_eq!(report.oracle.return_value, 10);
        assert_eq!(report.oracle.output, vec!["OUTPUT: 10"]);
    }

    #[test]
    fn oracle_matches_divide_by_zero_trap() {
        let (program, compiled) = parse_and_compile("func main() { return 10 / 0; }\n");
        let report = compare_ast_oracle(&program, &compiled);
        assert!(report.equivalent, "{report:#?}");
        assert!(!report.oracle.success);
        assert_eq!(report.oracle.trap_code, TrapCode::DivideByZero);
    }

    #[test]
    fn oracle_matches_undefined_local_trap() {
        let (program, compiled) = parse_and_compile("func main() { int x; return x; }\n");
        let report = compare_ast_oracle(&program, &compiled);
        assert!(report.equivalent, "{report:#?}");
        assert!(!report.oracle.success);
        assert_eq!(report.oracle.trap_code, TrapCode::UndefinedLocal);
    }
}
