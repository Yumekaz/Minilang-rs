//! Recursive descent parser for MiniLang.
//!
//! Transforms a token stream into an Abstract Syntax Tree (AST).

use crate::ast::*;
use crate::token::{Span, Token, TokenKind};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("Parse error at {span}: {message}")]
    Error { message: String, span: Span },
}

pub type ParseResult<T> = Result<T, ParseError>;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn current(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .unwrap_or(&self.tokens[self.tokens.len() - 1])
    }

    fn peek(&self) -> &Token {
        self.tokens
            .get(self.pos + 1)
            .unwrap_or(&self.tokens[self.tokens.len() - 1])
    }

    fn advance(&mut self) -> &Token {
        let token = self.current();
        if !matches!(token.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        &self.tokens[self.pos - 1]
    }

    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.current().kind) == std::mem::discriminant(kind)
    }

    fn match_token(&mut self, kind: &TokenKind) -> Option<&Token> {
        if self.check(kind) {
            self.pos += 1;
            Some(&self.tokens[self.pos - 1])
        } else {
            None
        }
    }

    fn expect(&mut self, kind: &TokenKind, msg: &str) -> ParseResult<&Token> {
        if self.check(kind) {
            self.pos += 1;
            Ok(&self.tokens[self.pos - 1])
        } else {
            Err(ParseError::Error {
                message: msg.to_string(),
                span: self.current().span,
            })
        }
    }

    fn error<T>(&self, msg: &str) -> ParseResult<T> {
        Err(ParseError::Error {
            message: msg.to_string(),
            span: self.current().span,
        })
    }

    pub fn parse(&mut self) -> ParseResult<Program> {
        let mut globals = Vec::new();
        let mut functions = Vec::new();

        while !matches!(self.current().kind, TokenKind::Eof) {
            if matches!(self.current().kind, TokenKind::Func) {
                functions.push(self.parse_function()?);
            } else if matches!(self.current().kind, TokenKind::Int | TokenKind::Bool) {
                globals.push(self.parse_global()?);
            } else {
                return self.error("Expected function or global declaration");
            }
        }

        Ok(Program { globals, functions })
    }

    fn parse_type(&mut self) -> ParseResult<Type> {
        let kind = &self.current().kind.clone();
        match kind {
            TokenKind::Int => {
                self.advance();
                Ok(Type::Int)
            }
            TokenKind::Bool => {
                self.advance();
                Ok(Type::Bool)
            }
            _ => self.error("Expected type (int or bool)"),
        }
    }

    fn parse_global(&mut self) -> ParseResult<GlobalVar> {
        let span = self.current().span;
        let var_type = self.parse_type()?;

        let name = match &self.current().kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return self.error("Expected identifier"),
        };
        self.advance();

        let mut array_size = None;
        let mut init_expr = None;

        if self.match_token(&TokenKind::LBracket).is_some() {
            if let TokenKind::IntLiteral(size) = self.current().kind {
                array_size = Some(size);
                self.advance();
            } else {
                return self.error("Expected array size");
            }
            self.expect(&TokenKind::RBracket, "Expected ']'")?;
        } else if self.match_token(&TokenKind::Assign).is_some() {
            init_expr = Some(self.parse_expr()?);
        }

        self.expect(&TokenKind::Semicolon, "Expected ';'")?;

        Ok(GlobalVar {
            var_type,
            name,
            init_expr,
            array_size,
            span,
        })
    }

    fn parse_function(&mut self) -> ParseResult<Function> {
        let span = self.current().span;
        self.expect(&TokenKind::Func, "Expected 'func'")?;

        let name = match &self.current().kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return self.error("Expected function name"),
        };
        self.advance();

        self.expect(&TokenKind::LParen, "Expected '('")?;

        let mut params = Vec::new();
        if !matches!(self.current().kind, TokenKind::RParen) {
            params.push(self.parse_param()?);
            while self.match_token(&TokenKind::Comma).is_some() {
                params.push(self.parse_param()?);
            }
        }

        self.expect(&TokenKind::RParen, "Expected ')'")?;

        let body = self.parse_block()?;

        Ok(Function {
            name,
            params,
            body,
            span,
        })
    }

    fn parse_param(&mut self) -> ParseResult<Param> {
        let span = self.current().span;
        let param_type = self.parse_type()?;

        let name = match &self.current().kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return self.error("Expected parameter name"),
        };
        self.advance();

        Ok(Param {
            param_type,
            name,
            span,
        })
    }

    fn parse_block(&mut self) -> ParseResult<Vec<Stmt>> {
        self.expect(&TokenKind::LBrace, "Expected '{'")?;

        let mut stmts = Vec::new();
        while !matches!(self.current().kind, TokenKind::RBrace | TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }

        self.expect(&TokenKind::RBrace, "Expected '}'")?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> ParseResult<Stmt> {
        match &self.current().kind {
            TokenKind::Int | TokenKind::Bool => self.parse_var_decl(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Print => self.parse_print(),
            TokenKind::Identifier(_) => self.parse_assign_or_expr(),
            _ => self.error("Expected statement"),
        }
    }

    fn parse_var_decl(&mut self) -> ParseResult<Stmt> {
        let span = self.current().span;
        let var_type = self.parse_type()?;

        let name = match &self.current().kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return self.error("Expected identifier"),
        };
        self.advance();

        let mut array_size = None;
        let mut init_expr = None;

        if self.match_token(&TokenKind::LBracket).is_some() {
            if let TokenKind::IntLiteral(size) = self.current().kind {
                array_size = Some(size);
                self.advance();
            } else {
                return self.error("Expected array size");
            }
            self.expect(&TokenKind::RBracket, "Expected ']'")?;
        } else if self.match_token(&TokenKind::Assign).is_some() {
            init_expr = Some(self.parse_expr()?);
        }

        self.expect(&TokenKind::Semicolon, "Expected ';'")?;

        Ok(Stmt::VarDecl {
            var_type,
            name,
            init_expr,
            array_size,
            span,
        })
    }

    fn parse_assign_or_expr(&mut self) -> ParseResult<Stmt> {
        let span = self.current().span;
        let name = match &self.current().kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return self.error("Expected identifier"),
        };
        self.advance();

        // Array index assignment: arr[i] = value;
        if self.match_token(&TokenKind::LBracket).is_some() {
            let index_expr = self.parse_expr()?;
            self.expect(&TokenKind::RBracket, "Expected ']'")?;
            self.expect(&TokenKind::Assign, "Expected '='")?;
            let value = self.parse_expr()?;
            self.expect(&TokenKind::Semicolon, "Expected ';'")?;
            return Ok(Stmt::Assign {
                target: name,
                index_expr: Some(index_expr),
                value,
                span,
            });
        }

        // Simple assignment: x = value;
        if self.match_token(&TokenKind::Assign).is_some() {
            let value = self.parse_expr()?;
            self.expect(&TokenKind::Semicolon, "Expected ';'")?;
            return Ok(Stmt::Assign {
                target: name,
                index_expr: None,
                value,
                span,
            });
        }

        // Function call as statement: foo();
        if self.match_token(&TokenKind::LParen).is_some() {
            let args = self.parse_args()?;
            self.expect(&TokenKind::RParen, "Expected ')'")?;
            self.expect(&TokenKind::Semicolon, "Expected ';'")?;
            return Ok(Stmt::ExprStmt {
                expr: Expr::Call { name, args, span },
                span,
            });
        }

        self.error("Expected assignment or function call")
    }

    fn parse_if(&mut self) -> ParseResult<Stmt> {
        let span = self.current().span;
        self.expect(&TokenKind::If, "Expected 'if'")?;
        self.expect(&TokenKind::LParen, "Expected '('")?;
        let condition = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "Expected ')'")?;

        let then_body = self.parse_block()?;

        let else_body = if self.match_token(&TokenKind::Else).is_some() {
            Some(self.parse_block()?)
        } else {
            None
        };

        Ok(Stmt::If {
            condition,
            then_body,
            else_body,
            span,
        })
    }

    fn parse_while(&mut self) -> ParseResult<Stmt> {
        let span = self.current().span;
        self.expect(&TokenKind::While, "Expected 'while'")?;
        self.expect(&TokenKind::LParen, "Expected '('")?;
        let condition = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "Expected ')'")?;

        let body = self.parse_block()?;

        Ok(Stmt::While {
            condition,
            body,
            span,
        })
    }

    fn parse_return(&mut self) -> ParseResult<Stmt> {
        let span = self.current().span;
        self.expect(&TokenKind::Return, "Expected 'return'")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semicolon, "Expected ';'")?;

        Ok(Stmt::Return { value, span })
    }

    fn parse_print(&mut self) -> ParseResult<Stmt> {
        let span = self.current().span;
        self.expect(&TokenKind::Print, "Expected 'print'")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semicolon, "Expected ';'")?;

        Ok(Stmt::Print { value, span })
    }

    fn parse_expr(&mut self) -> ParseResult<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_and()?;

        while self.match_token(&TokenKind::Or).is_some() {
            let span = left.span();
            let right = self.parse_and()?;
            left = Expr::Binary {
                op: BinaryOp::Or,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }

        Ok(left)
    }

    fn parse_and(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_equality()?;

        while self.match_token(&TokenKind::And).is_some() {
            let span = left.span();
            let right = self.parse_equality()?;
            left = Expr::Binary {
                op: BinaryOp::And,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }

        Ok(left)
    }

    fn parse_equality(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_comparison()?;

        loop {
            let op = if self.match_token(&TokenKind::Eq).is_some() {
                BinaryOp::Eq
            } else if self.match_token(&TokenKind::Ne).is_some() {
                BinaryOp::Ne
            } else {
                break;
            };

            let span = left.span();
            let right = self.parse_comparison()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }

        Ok(left)
    }

    fn parse_comparison(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_additive()?;

        loop {
            let op = if self.match_token(&TokenKind::Lt).is_some() {
                BinaryOp::Lt
            } else if self.match_token(&TokenKind::Gt).is_some() {
                BinaryOp::Gt
            } else if self.match_token(&TokenKind::Le).is_some() {
                BinaryOp::Le
            } else if self.match_token(&TokenKind::Ge).is_some() {
                BinaryOp::Ge
            } else {
                break;
            };

            let span = left.span();
            let right = self.parse_additive()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }

        Ok(left)
    }

    fn parse_additive(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_multiplicative()?;

        loop {
            let op = if self.match_token(&TokenKind::Plus).is_some() {
                BinaryOp::Add
            } else if self.match_token(&TokenKind::Minus).is_some() {
                BinaryOp::Sub
            } else {
                break;
            };

            let span = left.span();
            let right = self.parse_multiplicative()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }

        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> ParseResult<Expr> {
        let mut left = self.parse_unary()?;

        loop {
            let op = if self.match_token(&TokenKind::Star).is_some() {
                BinaryOp::Mul
            } else if self.match_token(&TokenKind::Slash).is_some() {
                BinaryOp::Div
            } else {
                break;
            };

            let span = left.span();
            let right = self.parse_unary()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }

        Ok(left)
    }

    fn parse_unary(&mut self) -> ParseResult<Expr> {
        let span = self.current().span;

        if self.match_token(&TokenKind::Minus).is_some() {
            let operand = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: UnaryOp::Neg,
                operand: Box::new(operand),
                span,
            });
        }

        if self.match_token(&TokenKind::Not).is_some() {
            let operand = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: UnaryOp::Not,
                operand: Box::new(operand),
                span,
            });
        }

        self.parse_primary()
    }

    fn parse_primary(&mut self) -> ParseResult<Expr> {
        let span = self.current().span;

        // Integer literal
        if let TokenKind::IntLiteral(value) = self.current().kind {
            self.advance();
            return Ok(Expr::IntLiteral { value, span });
        }

        // Boolean literal
        if let TokenKind::BoolLiteral(value) = self.current().kind {
            self.advance();
            return Ok(Expr::BoolLiteral { value, span });
        }

        // Identifier, function call, or array index
        if let TokenKind::Identifier(name) = self.current().kind.clone() {
            self.advance();

            // Function call
            if self.match_token(&TokenKind::LParen).is_some() {
                let args = self.parse_args()?;
                self.expect(&TokenKind::RParen, "Expected ')'")?;
                return Ok(Expr::Call { name, args, span });
            }

            // Array index
            if self.match_token(&TokenKind::LBracket).is_some() {
                let index = self.parse_expr()?;
                self.expect(&TokenKind::RBracket, "Expected ']'")?;
                return Ok(Expr::ArrayIndex {
                    array_name: name,
                    index: Box::new(index),
                    span,
                });
            }

            // Simple identifier
            return Ok(Expr::Identifier { name, span });
        }

        // Parenthesized expression
        if self.match_token(&TokenKind::LParen).is_some() {
            let expr = self.parse_expr()?;
            self.expect(&TokenKind::RParen, "Expected ')'")?;
            return Ok(expr);
        }

        self.error(&format!("Unexpected token: {:?}", self.current().kind))
    }

    fn parse_args(&mut self) -> ParseResult<Vec<Expr>> {
        let mut args = Vec::new();

        if matches!(self.current().kind, TokenKind::RParen) {
            return Ok(args);
        }

        args.push(self.parse_expr()?);
        while self.match_token(&TokenKind::Comma).is_some() {
            args.push(self.parse_expr()?);
        }

        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(source: &str) -> ParseResult<Program> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        parser.parse()
    }

    #[test]
    fn test_simple_function() {
        let program = parse("func main() { return 42; }").unwrap();
        assert_eq!(program.functions.len(), 1);
        assert_eq!(program.functions[0].name, "main");
    }

    #[test]
    fn test_arithmetic() {
        let program = parse("func main() { return 1 + 2 * 3; }").unwrap();
        // Should parse as 1 + (2 * 3) due to precedence
        if let Stmt::Return { value, .. } = &program.functions[0].body[0] {
            assert!(matches!(
                value,
                Expr::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            ));
        } else {
            panic!("Expected return statement");
        }
    }

    #[test]
    fn test_global_and_function() {
        let program = parse("int g = 10; func main() { return g; }").unwrap();
        assert_eq!(program.globals.len(), 1);
        assert_eq!(program.functions.len(), 1);
    }
}
