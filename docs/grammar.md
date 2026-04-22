# MiniLang Grammar

## Lexical Elements

```
IDENTIFIER  = [a-zA-Z_][a-zA-Z0-9_]*
INTEGER     = [0-9]+
BOOL        = "true" | "false"
```

## Keywords
```
int, bool, if, else, while, return, print, func, true, false
```

## Operators (by precedence, lowest to highest)
```
||          Logical OR
&&          Logical AND
== !=       Equality
< > <= >=   Comparison
+ -         Additive
* /         Multiplicative
- !         Unary (prefix)
```

## Grammar

```ebnf
program     = { global_decl | function }* ;

global_decl = type IDENTIFIER [ "[" INTEGER "]" ] [ "=" expr ] ";" ;

function    = "func" IDENTIFIER "(" [ params ] ")" block ;

params      = param { "," param }* ;
param       = type IDENTIFIER ;

type        = "int" | "bool" ;

block       = "{" { statement }* "}" ;

statement   = var_decl
            | assignment
            | if_stmt
            | while_stmt
            | return_stmt
            | print_stmt
            | expr_stmt ;

var_decl    = type IDENTIFIER [ "[" INTEGER "]" ] [ "=" expr ] ";" ;

assignment  = IDENTIFIER [ "[" expr "]" ] "=" expr ";" ;

if_stmt     = "if" "(" expr ")" block [ "else" block ] ;

while_stmt  = "while" "(" expr ")" block ;

return_stmt = "return" expr ";" ;

print_stmt  = "print" expr ";" ;

expr_stmt   = expr ";" ;

expr        = or_expr ;

or_expr     = and_expr { "||" and_expr }* ;

and_expr    = eq_expr { "&&" eq_expr }* ;

eq_expr     = cmp_expr { ("==" | "!=") cmp_expr }* ;

cmp_expr    = add_expr { ("<" | ">" | "<=" | ">=") add_expr }* ;

add_expr    = mul_expr { ("+" | "-") mul_expr }* ;

mul_expr    = unary_expr { ("*" | "/") unary_expr }* ;

unary_expr  = ("-" | "!") unary_expr | primary ;

primary     = INTEGER
            | BOOL  
            | IDENTIFIER [ "[" expr "]" ]
            | IDENTIFIER "(" [ args ] ")"
            | "(" expr ")" ;

args        = expr { "," expr }* ;
```

## Semantic Rules

1. All functions must have return type `int`
2. A `main` function with no parameters must exist
3. Variables must be declared before use
4. Array indices must be `int` type
5. Condition expressions must be `int` or `bool` (0 = false, non-zero = true)
6. Function calls must match parameter count and types
7. Shadowing is currently rejected; visible globals, functions, parameters, and locals share one name set
