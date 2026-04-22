# MiniLang Complete Reconstruction Specification

This document contains everything needed to rebuild MiniLang from scratch.

---

## Part 1: Token Specification

### 1.1 Token Kinds

```rust
pub enum TokenKind {
    // Literals
    IntLiteral(i32),
    True,
    False,
    
    // Identifier
    Identifier(String),
    
    // Keywords
    Func,
    If,
    Else,
    While,
    Return,
    Print,
    Int,      // type keyword
    Bool,     // type keyword
    
    // Operators
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Eq,       // ==
    Ne,       // !=
    Lt,       // <
    Gt,       // >
    Le,       // <=
    Ge,       // >=
    And,      // &&
    Or,       // ||
    Not,      // !
    Assign,   // =
    
    // Delimiters
    LParen,   // (
    RParen,   // )
    LBrace,   // {
    RBrace,   // }
    LBracket, // [
    RBracket, // ]
    Comma,    // ,
    Semicolon,// ;
    
    // Special
    Eof,
}
```

### 1.2 Span (Source Location)

```rust
pub struct Span {
    pub line: usize,    // 1-indexed
    pub column: usize,  // 1-indexed
}
```

### 1.3 Token

```rust
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
```

### 1.4 Lexer Rules

```
1. Skip whitespace: ' ', '\t', '\n', '\r'
2. Skip comments: "//" until end of line
3. Identifiers: [a-zA-Z_][a-zA-Z0-9_]*
4. Integers: [0-9]+ (parse as i64)
5. Two-char operators: ==, !=, <=, >=, &&, ||
6. Single-char operators: +, -, *, /, <, >, !, =
7. Delimiters: (, ), {, }, [, ], ,, ;
8. Keywords: check identifier against keyword list
```

---

## Part 2: AST Specification

### 2.1 Types

```rust
pub enum Type {
    Int,
    Bool,
}
```

### 2.2 Binary Operators

```rust
pub enum BinaryOp {
    Add,    // +
    Sub,    // -
    Mul,    // *
    Div,    // /
    Eq,     // ==
    Ne,     // !=
    Lt,     // <
    Gt,     // >
    Le,     // <=
    Ge,     // >=
    And,    // &&
    Or,     // ||
}
```

### 2.3 Unary Operators

```rust
pub enum UnaryOp {
    Neg,    // -
    Not,    // !
}
```

### 2.4 Expressions

```rust
pub enum Expr {
    IntLiteral {
        value: i64,
        span: Span,
    },
    BoolLiteral {
        value: bool,
        span: Span,
    },
    Identifier {
        name: String,
        span: Span,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    Call {
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
    ArrayIndex {
        array_name: String,
        index: Box<Expr>,
        span: Span,
    },
}
```

### 2.5 Statements

```rust
pub enum Stmt {
    VarDecl {
        var_type: Type,
        name: String,
        array_size: Option<u32>,  // None = scalar, Some(n) = array[n]
        init_expr: Option<Expr>,
        span: Span,
    },
    Assign {
        target: String,
        index_expr: Option<Expr>,  // None = scalar, Some = array[index]
        value: Expr,
        span: Span,
    },
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
        span: Span,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    Return {
        value: Expr,
        span: Span,
    },
    Print {
        value: Expr,
        span: Span,
    },
    ExprStmt {
        expr: Expr,
        span: Span,
    },
}
```

### 2.6 Top-Level Declarations

```rust
pub struct Param {
    pub param_type: Type,
    pub name: String,
}

pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

pub struct GlobalDecl {
    pub var_type: Type,
    pub name: String,
    pub array_size: Option<u32>,
    pub init_expr: Option<Expr>,
    pub span: Span,
}

pub struct Program {
    pub globals: Vec<GlobalDecl>,
    pub functions: Vec<Function>,
}
```

---

## Part 3: Parser Specification

### 3.1 Operator Precedence (lowest to highest)

| Level | Operators | Associativity |
|-------|-----------|---------------|
| 1 | \|\| | Left |
| 2 | && | Left |
| 3 | == != | Left |
| 4 | < > <= >= | Left |
| 5 | + - | Left |
| 6 | * / | Left |
| 7 | - ! (unary) | Right (prefix) |

### 3.2 Grammar (EBNF)

```ebnf
program     = { global_decl | function }* EOF ;

global_decl = type IDENTIFIER [ "[" INT_LITERAL "]" ] [ "=" expr ] ";" ;

function    = "func" IDENTIFIER "(" [ params ] ")" "{" { statement }* "}" ;

params      = param { "," param }* ;
param       = type IDENTIFIER ;

type        = "int" | "bool" ;

statement   = var_decl
            | assignment
            | if_stmt
            | while_stmt
            | return_stmt
            | print_stmt
            | expr_stmt ;

var_decl    = type IDENTIFIER [ "[" INT_LITERAL "]" ] [ "=" expr ] ";" ;

assignment  = IDENTIFIER [ "[" expr "]" ] "=" expr ";" ;

if_stmt     = "if" "(" expr ")" "{" { statement }* "}" [ "else" "{" { statement }* "}" ] ;

while_stmt  = "while" "(" expr ")" "{" { statement }* "}" ;

return_stmt = "return" expr ";" ;

print_stmt  = "print" expr ";" ;

expr_stmt   = expr ";" ;

(* Expression parsing uses precedence climbing *)
expr        = or_expr ;
or_expr     = and_expr { "||" and_expr }* ;
and_expr    = eq_expr { "&&" eq_expr }* ;
eq_expr     = cmp_expr { ("==" | "!=") cmp_expr }* ;
cmp_expr    = add_expr { ("<" | ">" | "<=" | ">=") add_expr }* ;
add_expr    = mul_expr { ("+" | "-") mul_expr }* ;
mul_expr    = unary_expr { ("*" | "/") unary_expr }* ;
unary_expr  = ("-" | "!") unary_expr | primary ;

primary     = INT_LITERAL
            | "true"
            | "false"
            | IDENTIFIER "(" [ args ] ")"      (* function call *)
            | IDENTIFIER "[" expr "]"          (* array index *)
            | IDENTIFIER                       (* variable *)
            | "(" expr ")" ;

args        = expr { "," expr }* ;
```

### 3.3 Parsing Technique

- Recursive descent for statements and declarations
- Precedence climbing (Pratt parsing) for expressions
- Lookahead: 1 token (LL(1) except for identifier disambiguation)
- Identifier disambiguation: peek for `(` (call), `[` (array), or neither (variable)

---

## Part 4: Semantic Analysis Specification

### 4.1 Symbol Tables

```rust
struct Symbol {
    name: String,
    symbol_type: Type,
    is_array: bool,
    array_size: Option<u32>,
    is_function: bool,
    param_count: Option<usize>,
}

struct Scope {
    symbols: HashMap<String, Symbol>,
    parent: Option<Box<Scope>>,
}
```

### 4.2 Semantic Rules

| Rule | Check |
|------|-------|
| S1 | All identifiers must be declared before use |
| S2 | No duplicate declarations in same scope |
| S3 | Function `main` must exist with no parameters |
| S4 | All functions return `int` (implicit) |
| S5 | Array indices must be int-typed expressions |
| S6 | Arithmetic operands must be int |
| S7 | Comparison operands must be int |
| S8 | Logical operands must be int or bool (0 = false) |
| S9 | Assignment LHS must match RHS type |
| S10 | Function call argument count must match |
| S11 | Function call argument types must match |
| S12 | Return expression must be int |
| S13 | Condition expressions must be int or bool |
| S14 | Array size must be positive constant |
| S15 | Cannot assign to function name |

### 4.3 Type Inference

```
IntLiteral      → Int
BoolLiteral     → Int (true=1, false=0)
Identifier      → lookup in symbol table
Binary(+,-,*,/) → Int (operands must be Int)
Binary(==,!=,<,>,<=,>=) → Int (0 or 1)
Binary(&&,||)   → Int (0 or 1)
Unary(-)        → Int
Unary(!)        → Int (0 or 1)
Call            → Int (all functions return Int)
ArrayIndex      → Int (arrays contain Int)
```

---

## Part 5: Bytecode Specification

### 5.1 Instruction Format

```rust
pub struct Instruction {
    pub opcode: Opcode,
    pub arg1: i32,
    pub arg2: i32,
}
```

### 5.2 Opcode Enumeration

```rust
#[repr(u8)]
pub enum Opcode {
    // Constants and variables
    LoadConst = 0,      // Push arg1 as constant
    LoadLocal = 1,      // Push local[arg1]
    StoreLocal = 2,     // Pop → local[arg1]
    LoadGlobal = 3,     // Push global[arg1]
    StoreGlobal = 4,    // Pop → global[arg1]
    
    // Arithmetic
    Add = 10,           // Push pop() + pop()
    Sub = 11,           // Push pop1 - pop2 (pop1 is left operand)
    Mul = 12,           // Push pop() * pop()
    Div = 13,           // Push pop1 / pop2 (trap if pop2 == 0)
    Neg = 14,           // Push -pop()
    
    // Comparison
    Eq = 20,            // Push (pop1 == pop2) ? 1 : 0
    Ne = 21,            // Push (pop1 != pop2) ? 1 : 0
    Lt = 22,            // Push (pop1 < pop2) ? 1 : 0
    Gt = 23,            // Push (pop1 > pop2) ? 1 : 0
    Le = 24,            // Push (pop1 <= pop2) ? 1 : 0
    Ge = 25,            // Push (pop1 >= pop2) ? 1 : 0
    
    // Logical
    And = 30,           // Logical AND (compiled as jumps for short-circuit)
    Or = 31,            // Logical OR (compiled as jumps for short-circuit)
    Not = 32,           // Push (pop() == 0) ? 1 : 0
    
    // Control flow
    Jump = 40,          // PC = arg1
    JumpIfFalse = 41,   // if pop() == 0 then PC = arg1
    JumpIfTrue = 42,    // if pop() != 0 then PC = arg1
    Call = 43,          // Call function arg1 with arg2 arguments
    Return = 44,        // Return pop() to caller
    
    // Arrays
    ArrayLoad = 50,     // Push global_array[arg1 + pop()] (arg2 = size for bounds)
    ArrayStore = 51,    // global_array[arg1 + pop_index] = pop_value
    LocalArrayLoad = 52,  // Load from local array reference
    LocalArrayStore = 53, // Store to local array reference
    AllocArray = 54,    // Allocate array of size arg1, push reference
    ArrayNew = 55,      // (GC only) Allocate heap array
    
    // I/O
    Print = 60,         // Print pop() as "OUTPUT: {value}"
    
    // Stack
    Pop = 70,           // Discard top of stack
    Dup = 71,           // Duplicate top of stack
    
    // Termination
    Halt = 80,          // Stop execution
}
```

### 5.3 Compiled Program Structure

```rust
pub struct FunctionInfo {
    pub name: String,
    pub id: usize,
    pub entry_pc: usize,    // Index into instructions
    pub param_count: usize,
    pub local_count: usize, // Including params
}

pub struct GlobalInfo {
    pub name: String,
    pub slot: usize,        // Index into globals array
    pub is_array: bool,
    pub array_size: usize,
}

pub struct CompiledProgram {
    pub instructions: Vec<Instruction>,
    pub functions: HashMap<usize, FunctionInfo>,
    pub globals: HashMap<String, GlobalInfo>,
    pub main_func_id: usize,
    pub constants: Vec<i32>,
}
```

### 5.4 Compilation Rules

#### Constants
```
IntLiteral(v)   → LoadConst v
BoolLiteral(true)  → LoadConst 1
BoolLiteral(false) → LoadConst 0
```

#### Variables
```
Identifier(x) where x is local  → LoadLocal slot
Identifier(x) where x is global → LoadGlobal slot
```

#### Binary Operators
```
Binary(op, left, right):
    compile(left)
    compile(right)
    emit(op_to_opcode(op))

op_to_opcode:
    Add → Add
    Sub → Sub
    Mul → Mul
    Div → Div
    Eq  → Eq
    Ne  → Ne
    Lt  → Lt
    Gt  → Gt
    Le  → Le
    Ge  → Ge
```

#### Short-Circuit Operators
```
Binary(And, left, right):
    compile(left)
    emit(JumpIfFalse, end_label)
    compile(right)
    emit(JumpIfFalse, end_label)
    emit(LoadConst, 1)
    emit(Jump, done_label)
    end_label:
    emit(LoadConst, 0)
    done_label:

Binary(Or, left, right):
    compile(left)
    emit(JumpIfTrue, true_label)
    compile(right)
    emit(JumpIfTrue, true_label)
    emit(LoadConst, 0)
    emit(Jump, done_label)
    true_label:
    emit(LoadConst, 1)
    done_label:
```

#### Unary Operators
```
Unary(Neg, operand):
    compile(operand)
    emit(Neg)

Unary(Not, operand):
    compile(operand)
    emit(Not)
```

#### Array Access
```
ArrayIndex(arr, index) where arr is global:
    compile(index)
    emit(ArrayLoad, arr_slot, arr_size)

ArrayIndex(arr, index) where arr is local:
    compile(index)
    emit(LocalArrayLoad, arr_slot, arr_size)
```

#### Function Call
```
Call(name, args):
    for arg in args:
        compile(arg)
    emit(Call, func_id, len(args))
```

#### Statements
```
VarDecl(name, init) where local:
    if is_array:
        emit(AllocArray, size)
        emit(StoreLocal, slot)
    else if init:
        compile(init)
        emit(StoreLocal, slot)

Assign(target, value) where local:
    compile(value)
    emit(StoreLocal, slot)

Assign(target, index, value) where local array:
    compile(index)
    compile(value)
    emit(LocalArrayStore, slot, size)

If(cond, then, else):
    compile(cond)
    emit(JumpIfFalse, else_label)
    compile_stmts(then)
    emit(Jump, end_label)
    else_label:
    compile_stmts(else)
    end_label:

While(cond, body):
    loop_label:
    compile(cond)
    emit(JumpIfFalse, end_label)
    compile_stmts(body)
    emit(Jump, loop_label)
    end_label:

Return(expr):
    compile(expr)
    emit(Return)

Print(expr):
    compile(expr)
    emit(Print)
```

#### Function Compilation
```
Function(name, params, body):
    record entry_pc = current_pc
    collect_local_decls(body)  // First pass
    for stmt in body:
        compile_stmt(stmt)
    emit(LoadConst, 0)  // Default return
    emit(Return)
```

---

## Part 6: VM Specification

### 6.1 VM State

```rust
pub struct Vm<'a> {
    program: &'a CompiledProgram,
    
    // Execution state
    pc: usize,                  // Program counter
    cycles: u64,                // Instruction count
    
    // Memory
    stack: Vec<i64>,            // Operand stack
    locals: Vec<i64>,           // Local variable storage
    globals: Vec<i64>,          // Global variable storage
    
    // Call stack
    call_stack: Vec<CallFrame>,
    
    // Configuration
    max_cycles: u64,            // Default: 100,000
    debug: bool,
}

pub struct CallFrame {
    return_pc: usize,           // Where to return
    base_ptr: usize,            // Base of locals for this frame
    func_id: usize,
    local_init: Vec<bool>,      // Track initialized locals
}
```

### 6.2 Execution Loop

```
while pc < instructions.len() && cycles < max_cycles:
    cycles += 1
    instr = instructions[pc]
    
    match instr.opcode:
        LoadConst:
            push(arg1 as i64)
            pc += 1
        
        LoadLocal:
            if not local_init[base_ptr + arg1]:
                trap(UndefinedLocal)
            push(locals[base_ptr + arg1])
            pc += 1
        
        StoreLocal:
            locals[base_ptr + arg1] = pop()
            local_init[base_ptr + arg1] = true
            pc += 1
        
        LoadGlobal:
            push(globals[arg1])
            pc += 1
        
        StoreGlobal:
            globals[arg1] = pop()
            pc += 1
        
        Add:
            b = pop()
            a = pop()
            push(normalize_i32(a + b))
            pc += 1
        
        Sub:
            b = pop()
            a = pop()
            push(normalize_i32(a - b))
            pc += 1
        
        Mul:
            b = pop()
            a = pop()
            push(normalize_i32(a * b))
            pc += 1
        
        Div:
            b = pop()
            a = pop()
            if b == 0:
                trap(DivideByZero)
            push(a / b)  // Rust i64 division truncates toward zero
            pc += 1
        
        Neg:
            a = pop()
            push(normalize_i32(-a))
            pc += 1
        
        Eq:
            b = pop()
            a = pop()
            push(if a == b { 1 } else { 0 })
            pc += 1
        
        Ne:
            b = pop()
            a = pop()
            push(if a != b { 1 } else { 0 })
            pc += 1
        
        Lt:
            b = pop()
            a = pop()
            push(if a < b { 1 } else { 0 })
            pc += 1
        
        Gt:
            b = pop()
            a = pop()
            push(if a > b { 1 } else { 0 })
            pc += 1
        
        Le:
            b = pop()
            a = pop()
            push(if a <= b { 1 } else { 0 })
            pc += 1
        
        Ge:
            b = pop()
            a = pop()
            push(if a >= b { 1 } else { 0 })
            pc += 1
        
        Not:
            a = pop()
            push(if a == 0 { 1 } else { 0 })
            pc += 1
        
        Jump:
            pc = arg1 as usize
        
        JumpIfFalse:
            cond = pop()
            if cond == 0:
                pc = arg1 as usize
            else:
                pc += 1
        
        JumpIfTrue:
            cond = pop()
            if cond != 0:
                pc = arg1 as usize
            else:
                pc += 1
        
        Call:
            func_id = arg1 as usize
            arg_count = arg2 as usize
            func = functions[func_id]
            
            if call_stack.len() >= 100:
                trap(StackOverflow)
            
            // Pop arguments
            args = []
            for _ in 0..arg_count:
                args.push(pop())
            args.reverse()
            
            // Create new frame
            new_base = locals.len()
            frame = CallFrame {
                return_pc: pc + 1,
                base_ptr: new_base,
                func_id: func_id,
                local_init: vec![false; func.local_count],
            }
            
            // Initialize parameters
            for (i, arg) in args.enumerate():
                locals.push(arg)
                frame.local_init[i] = true
            
            // Allocate remaining locals
            for _ in arg_count..func.local_count:
                locals.push(0)
            
            call_stack.push(frame)
            pc = func.entry_pc
        
        Return:
            return_value = pop()
            frame = call_stack.pop()
            
            // Deallocate locals
            locals.truncate(frame.base_ptr)
            
            if call_stack.is_empty():
                // Return from main
                return VmResult { return_value, success: true }
            else:
                push(return_value)
                pc = frame.return_pc
        
        ArrayLoad:
            base_slot = arg1 as usize
            array_size = arg2 as usize
            index = pop() as usize
            
            if index >= array_size:
                trap(ArrayOutOfBounds)
            
            push(globals[base_slot + index])
            pc += 1
        
        ArrayStore:
            base_slot = arg1 as usize
            array_size = arg2 as usize
            value = pop()
            index = pop() as usize
            
            if index >= array_size:
                trap(ArrayOutOfBounds)
            
            globals[base_slot + index] = value
            pc += 1
        
        Print:
            value = pop()
            println!("OUTPUT: {}", value)
            pc += 1
        
        Pop:
            pop()
            pc += 1
        
        Dup:
            value = *stack.last()
            push(value)
            pc += 1
        
        Halt:
            return VmResult { return_value: pop(), success: true }

if cycles >= max_cycles:
    trap(CycleLimit)
```

### 6.3 32-Bit Normalization

```rust
fn normalize_i32(value: i64) -> i64 {
    let masked = value & 0xFFFFFFFF;
    if masked > 0x7FFFFFFF {
        (masked as i64) - 0x100000000
    } else {
        masked as i64
    }
}
```

### 6.4 Trap Codes

```rust
#[repr(u8)]
pub enum TrapCode {
    None = 0,
    DivideByZero = 1,
    UndefinedLocal = 2,
    ArrayOutOfBounds = 3,
    StackOverflow = 4,
    CycleLimit = 5,
    UndefinedFunction = 6,
    StackUnderflow = 7,
    InvalidInstruction = 8,
}
```

---

## Part 7: GC VM Specification

### 7.1 GC Value Representation

```rust
pub enum GcValue {
    Int(i64),
    ArrayRef(u32),  // Index into heap_arrays
}

pub struct HeapArray {
    pub data: Vec<i64>,
    pub marked: bool,
    pub ref_count: u32,  // For debugging
}
```

### 7.2 GC VM Additional State

```rust
pub struct GcVm<'a> {
    // ... same as Vm ...
    
    // Heap
    heap_arrays: Vec<Option<HeapArray>>,
    free_list: Vec<u32>,  // Recycled array slots
    
    // GC state
    gc_threshold: usize,  // Default: 8
    gc_collections: usize,
    gc_objects_freed: usize,
}
```

### 7.3 Array Allocation

```rust
fn alloc_array(&mut self, size: usize) -> u32 {
    // Check if GC needed
    if self.heap_arrays.len() >= self.gc_threshold {
        self.collect_garbage();
    }
    
    // Reuse from free list or allocate new
    let id = if let Some(id) = self.free_list.pop() {
        self.heap_arrays[id as usize] = Some(HeapArray {
            data: vec![0; size],
            marked: false,
            ref_count: 1,
        });
        id
    } else {
        let id = self.heap_arrays.len() as u32;
        self.heap_arrays.push(Some(HeapArray {
            data: vec![0; size],
            marked: false,
            ref_count: 1,
        }));
        id
    };
    
    self.heap_arrays_allocated += 1;
    id
}
```

### 7.4 Mark-Sweep Collection

```rust
fn collect_garbage(&mut self) {
    self.gc_collections += 1;
    
    // Clear marks
    for arr in &mut self.heap_arrays {
        if let Some(ref mut a) = arr {
            a.marked = false;
        }
    }
    
    // Collect roots
    let mut roots = Vec::new();
    
    // Root: operand stack
    for val in &self.operand_stack {
        if let GcValue::ArrayRef(id) = val {
            roots.push(*id);
        }
    }
    
    // Root: globals
    for val in &self.globals {
        if let GcValue::ArrayRef(id) = val {
            roots.push(*id);
        }
    }
    
    // Root: call frame locals
    for frame in &self.call_stack {
        for val in &frame.locals {
            if let GcValue::ArrayRef(id) = val {
                roots.push(*id);
            }
        }
    }
    
    // Mark phase
    for id in roots {
        if let Some(ref mut arr) = self.heap_arrays[id as usize] {
            arr.marked = true;
        }
    }
    
    // Sweep phase
    let mut freed = 0;
    for (i, slot) in self.heap_arrays.iter_mut().enumerate() {
        if let Some(ref arr) = slot {
            if !arr.marked {
                *slot = None;
                self.free_list.push(i as u32);
                freed += 1;
            }
        }
    }
    
    self.gc_objects_freed += freed;
}
```

---

## Part 8: JIT Specification (x86-64)

### 8.1 Register Usage

| Register | Usage |
|----------|-------|
| RAX | Return value, scratch |
| RCX | Scratch |
| RDX | Scratch (division) |
| RBX | Callee-saved (unused) |
| RSP | Stack pointer |
| RBP | Frame pointer |
| RSI | Scratch |
| RDI | Scratch |
| R8-R15 | Scratch |

### 8.2 Stack Frame Layout

```
High addresses
┌─────────────────┐
│   Return addr   │  [RBP + 8]
├─────────────────┤
│   Saved RBP     │  [RBP]
├─────────────────┤
│   Local 0       │  [RBP - 8]
├─────────────────┤
│   Local 1       │  [RBP - 16]
├─────────────────┤
│   ...           │
├─────────────────┤
│   Local 31      │  [RBP - 256]
├─────────────────┤
│   Global 0      │  [RBP - 512]
├─────────────────┤
│   ...           │
├─────────────────┤
│   Global 255    │  [RBP - 2560]
├─────────────────┤
│  Array scratch  │  [RBP - 4096] and below
└─────────────────┘
Low addresses
```

### 8.3 Prologue

```asm
push rbp
mov rbp, rsp
sub rsp, 8192       ; Reserve 8KB
```

### 8.4 Epilogue

```asm
mov rsp, rbp
pop rbp
ret
```

### 8.5 Opcode → x86-64 Translation

#### LoadConst
```asm
mov eax, imm32      ; B8 <imm32>
push rax            ; 50
```

#### LoadLocal
```asm
mov rax, [rbp - 8 - slot*8]
push rax
```

#### StoreLocal
```asm
pop rax
mov [rbp - 8 - slot*8], rax
```

#### Add
```asm
pop rcx             ; Right operand
pop rax             ; Left operand
add rax, rcx
push rax
```

#### Sub
```asm
pop rcx
pop rax
sub rax, rcx
push rax
```

#### Mul
```asm
pop rcx
pop rax
imul rax, rcx
push rax
```

#### Div
```asm
pop rcx             ; Divisor
pop rax             ; Dividend
cqo                 ; Sign-extend RAX → RDX:RAX
idiv rcx            ; RAX = quotient
push rax
```

#### Neg
```asm
pop rax
neg rax
push rax
```

#### Comparison (Eq, Ne, Lt, Gt, Le, Ge)
```asm
pop rcx
pop rax
cmp rax, rcx
set<cc> al          ; sete, setne, setl, setg, setle, setge
movzx rax, al
push rax
```

#### Not
```asm
pop rax
test rax, rax
sete al
movzx rax, al
push rax
```

#### Jump
```asm
jmp rel32           ; E9 <rel32>
```

#### JumpIfFalse
```asm
pop rax
test rax, rax
je rel32            ; 0F 84 <rel32>
```

#### JumpIfTrue
```asm
pop rax
test rax, rax
jne rel32           ; 0F 85 <rel32>
```

#### Return
```asm
pop rax             ; Return value in RAX
mov rsp, rbp
pop rbp
ret
```

### 8.6 Jump Patching

```rust
struct PendingJump {
    code_offset: usize,  // Offset of rel32 in code buffer
    target_label: usize, // Target PC (bytecode)
}

fn patch_jumps(&mut self) {
    for (code_offset, target_label) in &self.pending_jumps {
        let target_offset = self.labels[target_label];
        let rel32 = (target_offset as i32) - (code_offset as i32) - 4;
        // Write rel32 at code_offset
    }
}
```

### 8.7 Executable Memory

```rust
fn allocate_executable(code: &[u8]) -> *mut u8 {
    unsafe {
        let ptr = mmap(
            null_mut(),
            code.len(),
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        );
        
        ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len());
        
        mprotect(ptr, code.len(), PROT_READ | PROT_EXEC);
        
        ptr
    }
}
```

---

## Part 9: Optimizer Specification

### 9.1 Optimization Passes

1. **Constant Folding**: Evaluate constant expressions at compile time
2. **Dead Code Elimination**: Remove unreachable instructions after Return/Jump
3. **Strength Reduction**: Replace expensive operations with cheaper ones
4. **Peephole Optimization**: Pattern-based local improvements

### 9.2 Constant Folding

```
Pattern: LoadConst a, LoadConst b, BinOp
Replace: LoadConst (a op b)

Example:
  LoadConst 10
  LoadConst 20
  Add
→ LoadConst 30
```

### 9.3 Dead Code Elimination

```
Pattern: Return/Jump followed by non-jump-target instructions
Replace: Remove following instructions until next jump target
```

### 9.4 Strength Reduction

```
x + 0 → x
x - 0 → x
x * 0 → 0
x * 1 → x
x * 2 → x + x (or shift)
x / 1 → x
```

### 9.5 Peephole Patterns

```
LoadLocal n, StoreLocal n → (remove both)
Push x, Pop → (remove both)
Jump to next instruction → (remove)
```

---

## Part 10: Test Cases

### 10.1 Lexer Tests

```
Input: "func main() { return 42; }"
Tokens: [Func, Identifier("main"), LParen, RParen, LBrace, Return, IntLiteral(42), Semicolon, RBrace]

Input: "// comment\n42"
Tokens: [IntLiteral(42)]

Input: "a && b || c"
Tokens: [Identifier("a"), And, Identifier("b"), Or, Identifier("c")]
```

### 10.2 Parser Tests

```
Input: "func main() { return 42; }"
AST: Program { functions: [Function { name: "main", body: [Return { value: IntLiteral(42) }] }] }

Input: "func main() { if (x > 0) { return 1; } else { return 0; } }"
AST: Program { functions: [Function { name: "main", body: [If { condition: Binary(Gt, Identifier(x), IntLiteral(0)), then_body: [...], else_body: Some([...]) }] }] }
```

### 10.3 VM Tests

| Program | Expected Result |
|---------|-----------------|
| `func main() { return 42; }` | 42 |
| `func main() { return 10 + 20; }` | 30 |
| `func main() { int x = 5; return x * 2; }` | 10 |
| `func main() { if (1 > 0) { return 1; } return 0; }` | 1 |
| `func main() { int i = 0; while (i < 5) { i = i + 1; } return i; }` | 5 |
| `func f(int x) { return x * 2; } func main() { return f(21); }` | 42 |
| `func f(int n) { if (n <= 1) { return 1; } return n * f(n-1); } func main() { return f(5); }` | 120 |
| `int a[5]; func main() { a[0] = 10; a[1] = 20; return a[0] + a[1]; }` | 30 |
| `func main() { return 2147483647 + 1; }` | -2147483648 (overflow) |
| `func main() { return 1 / 0; }` | TRAP(DivideByZero) |
| `func main() { int x; return x; }` | TRAP(UndefinedLocal) |
| `int a[5]; func main() { return a[10]; }` | TRAP(ArrayOutOfBounds) |

---

## Part 11: File Structure

```
minilang-rs/
├── Cargo.toml
├── src/
│   ├── lib.rs          # Public API exports
│   ├── main.rs         # CLI entry point
│   ├── token.rs        # Token definitions (~50 lines)
│   ├── lexer.rs        # Lexical analysis (~250 lines)
│   ├── ast.rs          # AST definitions (~180 lines)
│   ├── parser.rs       # Recursive descent parser (~630 lines)
│   ├── sema.rs         # Semantic analysis (~400 lines)
│   ├── compiler.rs     # AST → Bytecode (~640 lines)
│   ├── optimizer.rs    # Bytecode optimization (~350 lines)
│   ├── vm.rs           # Stack-based interpreter (~640 lines)
│   ├── gc_vm.rs        # GC-enabled interpreter (~920 lines)
│   ├── gc.rs           # GC primitives (~280 lines)
│   ├── alloc.rs        # Memory allocators (~380 lines)
│   ├── jit.rs          # x86-64 JIT compiler (~970 lines)
│   └── repl.rs         # Interactive REPL (~430 lines)
├── tests/
│   └── integration_tests.rs
├── examples/
│   ├── hello.lang
│   ├── fibonacci.lang
│   └── ...
└── docs/
    ├── grammar.md
    ├── runtime.md
    ├── system-artifact.md
    └── reconstruction.md (this file)
```

---

## Part 12: Reconstruction Order

1. **token.rs** - Define TokenKind, Span, Token
2. **lexer.rs** - Implement tokenization
3. **ast.rs** - Define Expr, Stmt, Program
4. **parser.rs** - Implement recursive descent
5. **sema.rs** - Implement semantic checks
6. **compiler.rs** - Define Opcode, implement compilation
7. **vm.rs** - Implement fetch-decode-execute loop
8. **Test everything above** - Should pass basic tests
9. **optimizer.rs** - Add optimizations
10. **gc_vm.rs** - Add GC support
11. **jit.rs** - Add x86-64 codegen
12. **repl.rs** - Add interactive mode
13. **main.rs** - Wire up CLI
14. **alloc.rs** - Add allocators (optional, for stats)
15. **gc.rs** - Add GC primitives (used by gc_vm)

---

This document provides everything needed to rebuild MiniLang from scratch. Each section is self-contained and can be implemented independently.
