//! Lexical analyzer for MiniLang.
//!
//! Transforms source code into a stream of tokens.

use crate::token::{Span, Token, TokenKind};

/// Lexer for MiniLang source code
pub struct Lexer<'a> {
    source: &'a [u8],
    pos: usize,
    line: u32,
    column: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            pos: 0,
            line: 1,
            column: 1,
        }
    }

    /// Tokenize the entire source into a vector of tokens
    pub fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token();
            let is_eof = matches!(token.kind, TokenKind::Eof);
            tokens.push(token);
            if is_eof {
                break;
            }
        }
        tokens
    }

    fn current(&self) -> Option<u8> {
        self.source.get(self.pos).copied()
    }

    fn peek(&self) -> Option<u8> {
        self.source.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let ch = self.current()?;
        self.pos += 1;
        if ch == b'\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace
            while matches!(self.current(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.advance();
            }

            // Skip single-line comments
            if self.current() == Some(b'/') && self.peek() == Some(b'/') {
                while self.current().is_some() && self.current() != Some(b'\n') {
                    self.advance();
                }
                continue;
            }

            break;
        }
    }

    fn make_token(&self, kind: TokenKind) -> Token {
        Token::new(kind, Span::new(self.line, self.column))
    }

    fn scan_number(&mut self) -> Token {
        let span = Span::new(self.line, self.column);
        let mut value: i32 = 0;

        while let Some(ch) = self.current() {
            if ch.is_ascii_digit() {
                value = value.wrapping_mul(10).wrapping_add((ch - b'0') as i32);
                self.advance();
            } else {
                break;
            }
        }

        Token::new(TokenKind::IntLiteral(value), span)
    }

    fn scan_identifier_or_keyword(&mut self) -> Token {
        let span = Span::new(self.line, self.column);
        let start = self.pos;

        while let Some(ch) = self.current() {
            if ch.is_ascii_alphanumeric() || ch == b'_' {
                self.advance();
            } else {
                break;
            }
        }

        let text = std::str::from_utf8(&self.source[start..self.pos]).unwrap();

        let kind = match text {
            "int" => TokenKind::Int,
            "bool" => TokenKind::Bool,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "func" => TokenKind::Func,
            "return" => TokenKind::Return,
            "print" => TokenKind::Print,
            "true" => TokenKind::BoolLiteral(true),
            "false" => TokenKind::BoolLiteral(false),
            _ => TokenKind::Identifier(text.to_string()),
        };

        Token::new(kind, span)
    }

    pub fn next_token(&mut self) -> Token {
        self.skip_whitespace_and_comments();

        let Some(ch) = self.current() else {
            return self.make_token(TokenKind::Eof);
        };

        // Numbers
        if ch.is_ascii_digit() {
            return self.scan_number();
        }

        // Identifiers and keywords
        if ch.is_ascii_alphabetic() || ch == b'_' {
            return self.scan_identifier_or_keyword();
        }

        // Operators and delimiters
        let span = Span::new(self.line, self.column);

        // Two-character operators
        if ch == b'=' && self.peek() == Some(b'=') {
            self.advance();
            self.advance();
            return Token::new(TokenKind::Eq, span);
        }
        if ch == b'!' && self.peek() == Some(b'=') {
            self.advance();
            self.advance();
            return Token::new(TokenKind::Ne, span);
        }
        if ch == b'<' && self.peek() == Some(b'=') {
            self.advance();
            self.advance();
            return Token::new(TokenKind::Le, span);
        }
        if ch == b'>' && self.peek() == Some(b'=') {
            self.advance();
            self.advance();
            return Token::new(TokenKind::Ge, span);
        }
        if ch == b'&' && self.peek() == Some(b'&') {
            self.advance();
            self.advance();
            return Token::new(TokenKind::And, span);
        }
        if ch == b'|' && self.peek() == Some(b'|') {
            self.advance();
            self.advance();
            return Token::new(TokenKind::Or, span);
        }

        // Single-character tokens
        self.advance();
        let kind = match ch {
            b'+' => TokenKind::Plus,
            b'-' => TokenKind::Minus,
            b'*' => TokenKind::Star,
            b'/' => TokenKind::Slash,
            b'<' => TokenKind::Lt,
            b'>' => TokenKind::Gt,
            b'!' => TokenKind::Not,
            b'=' => TokenKind::Assign,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b';' => TokenKind::Semicolon,
            b',' => TokenKind::Comma,
            _ => TokenKind::Error(format!("Unexpected character: {}", ch as char)),
        };

        Token::new(kind, span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_tokens() {
        let mut lexer = Lexer::new("func main() { return 42; }");
        let tokens = lexer.tokenize();

        assert!(matches!(tokens[0].kind, TokenKind::Func));
        assert!(matches!(tokens[1].kind, TokenKind::Identifier(ref s) if s == "main"));
        assert!(matches!(tokens[2].kind, TokenKind::LParen));
        assert!(matches!(tokens[3].kind, TokenKind::RParen));
        assert!(matches!(tokens[4].kind, TokenKind::LBrace));
        assert!(matches!(tokens[5].kind, TokenKind::Return));
        assert!(matches!(tokens[6].kind, TokenKind::IntLiteral(42)));
        assert!(matches!(tokens[7].kind, TokenKind::Semicolon));
        assert!(matches!(tokens[8].kind, TokenKind::RBrace));
        assert!(matches!(tokens[9].kind, TokenKind::Eof));
    }

    #[test]
    fn test_operators() {
        let mut lexer = Lexer::new("== != <= >= && || + - * /");
        let tokens = lexer.tokenize();

        assert!(matches!(tokens[0].kind, TokenKind::Eq));
        assert!(matches!(tokens[1].kind, TokenKind::Ne));
        assert!(matches!(tokens[2].kind, TokenKind::Le));
        assert!(matches!(tokens[3].kind, TokenKind::Ge));
        assert!(matches!(tokens[4].kind, TokenKind::And));
        assert!(matches!(tokens[5].kind, TokenKind::Or));
    }

    #[test]
    fn test_comments() {
        let mut lexer = Lexer::new("42 // this is a comment\n123");
        let tokens = lexer.tokenize();

        assert!(matches!(tokens[0].kind, TokenKind::IntLiteral(42)));
        assert!(matches!(tokens[1].kind, TokenKind::IntLiteral(123)));
    }
}
