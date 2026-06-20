use super::{Diagnostic, Span};

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Integer(u64),
    String(String),
    ByteString(Vec<u8>),
    Byte(u8),
    Module,
    Use,
    As,
    Pub,
    Fn,
    Unsafe,
    Extern,
    Struct,
    Enum,
    Static,
    Const,
    Mut,
    Let,
    If,
    Else,
    While,
    Loop,
    Break,
    Continue,
    Return,
    True,
    False,
    Asm,
    Options,
    In,
    Out,
    InOut,
    At,
    Bang,
    Tilde,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Eq,
    EqEq,
    NotEq,
    Less,
    LessEq,
    Greater,
    GreaterEq,
    AndAnd,
    OrOr,
    ShiftLeft,
    ShiftRight,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShiftLeftEq,
    ShiftRightEq,
    Arrow,
    Colon,
    ColonColon,
    Semi,
    Comma,
    Dot,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Eof,
}

pub fn lex(file: &str, source: &str) -> Result<Vec<Token>, Diagnostic> {
    Lexer::new(file, source).lex_all()
}

struct Lexer<'a> {
    file: &'a str,
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    fn new(file: &'a str, source: &'a str) -> Self {
        Self {
            file,
            source,
            bytes: source.as_bytes(),
            pos: 0,
            line: 1,
            column: 1,
        }
    }

    fn lex_all(mut self) -> Result<Vec<Token>, Diagnostic> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia()?;
            let start = self.mark();
            if self.pos == self.bytes.len() {
                out.push(Token {
                    kind: TokenKind::Eof,
                    span: self.span_from(start),
                });
                return Ok(out);
            }
            let kind = self.next_token()?;
            out.push(Token {
                kind,
                span: self.span_from(start),
            });
        }
    }

    fn next_token(&mut self) -> Result<TokenKind, Diagnostic> {
        let b = self.peek().unwrap();
        if is_ident_start(b) {
            if b == b'b' && self.peek_n(1) == Some(b'"') {
                self.bump();
                return self.lex_byte_string();
            }
            if b == b'b' && self.peek_n(1) == Some(b'\'') {
                self.bump();
                return self.lex_byte();
            }
            return Ok(self.lex_ident());
        }
        if b.is_ascii_digit() {
            return self.lex_integer();
        }
        if b == b'"' {
            return self.lex_string().map(TokenKind::String);
        }

        macro_rules! two_or_one {
            ($second:expr, $two:expr, $one:expr) => {{
                self.bump();
                if self.peek() == Some($second) {
                    self.bump();
                    Ok($two)
                } else {
                    Ok($one)
                }
            }};
        }

        match b {
            b'@' => self.one(TokenKind::At),
            b'~' => self.one(TokenKind::Tilde),
            b'+' => two_or_one!(b'=', TokenKind::PlusEq, TokenKind::Plus),
            b'-' => {
                self.bump();
                if self.peek() == Some(b'>') {
                    self.bump();
                    Ok(TokenKind::Arrow)
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::MinusEq)
                } else {
                    Ok(TokenKind::Minus)
                }
            }
            b'*' => two_or_one!(b'=', TokenKind::StarEq, TokenKind::Star),
            b'/' => two_or_one!(b'=', TokenKind::SlashEq, TokenKind::Slash),
            b'%' => two_or_one!(b'=', TokenKind::PercentEq, TokenKind::Percent),
            b'^' => two_or_one!(b'=', TokenKind::CaretEq, TokenKind::Caret),
            b'!' => two_or_one!(b'=', TokenKind::NotEq, TokenKind::Bang),
            b'=' => two_or_one!(b'=', TokenKind::EqEq, TokenKind::Eq),
            b'&' => {
                self.bump();
                if self.peek() == Some(b'&') {
                    self.bump();
                    Ok(TokenKind::AndAnd)
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::AmpEq)
                } else {
                    Ok(TokenKind::Amp)
                }
            }
            b'|' => {
                self.bump();
                if self.peek() == Some(b'|') {
                    self.bump();
                    Ok(TokenKind::OrOr)
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::PipeEq)
                } else {
                    Ok(TokenKind::Pipe)
                }
            }
            b'<' => {
                self.bump();
                if self.peek() == Some(b'<') {
                    self.bump();
                    if self.peek() == Some(b'=') {
                        self.bump();
                        Ok(TokenKind::ShiftLeftEq)
                    } else {
                        Ok(TokenKind::ShiftLeft)
                    }
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::LessEq)
                } else {
                    Ok(TokenKind::Less)
                }
            }
            b'>' => {
                self.bump();
                if self.peek() == Some(b'>') {
                    self.bump();
                    if self.peek() == Some(b'=') {
                        self.bump();
                        Ok(TokenKind::ShiftRightEq)
                    } else {
                        Ok(TokenKind::ShiftRight)
                    }
                } else if self.peek() == Some(b'=') {
                    self.bump();
                    Ok(TokenKind::GreaterEq)
                } else {
                    Ok(TokenKind::Greater)
                }
            }
            b':' => two_or_one!(b':', TokenKind::ColonColon, TokenKind::Colon),
            b';' => self.one(TokenKind::Semi),
            b',' => self.one(TokenKind::Comma),
            b'.' => self.one(TokenKind::Dot),
            b'(' => self.one(TokenKind::LParen),
            b')' => self.one(TokenKind::RParen),
            b'{' => self.one(TokenKind::LBrace),
            b'}' => self.one(TokenKind::RBrace),
            b'[' => self.one(TokenKind::LBracket),
            b']' => self.one(TokenKind::RBracket),
            _ => Err(self.error(format!("unexpected byte `{}`", b as char))),
        }
    }

    fn lex_ident(&mut self) -> TokenKind {
        let start = self.pos;
        self.bump();
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let s = &self.source[start..self.pos];
        match s {
            "module" => TokenKind::Module,
            "use" => TokenKind::Use,
            "as" => TokenKind::As,
            "pub" => TokenKind::Pub,
            "fn" => TokenKind::Fn,
            "unsafe" => TokenKind::Unsafe,
            "extern" => TokenKind::Extern,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "static" => TokenKind::Static,
            "const" => TokenKind::Const,
            "mut" => TokenKind::Mut,
            "let" => TokenKind::Let,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "loop" => TokenKind::Loop,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "return" => TokenKind::Return,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "asm" => TokenKind::Asm,
            "options" => TokenKind::Options,
            "in" => TokenKind::In,
            "out" => TokenKind::Out,
            "inout" => TokenKind::InOut,
            _ => TokenKind::Ident(s.to_string()),
        }
    }

    fn lex_integer(&mut self) -> Result<TokenKind, Diagnostic> {
        let start = self.pos;
        let radix = if self.peek() == Some(b'0') {
            match self.peek_n(1) {
                Some(b'x') | Some(b'X') => {
                    self.bump();
                    self.bump();
                    16
                }
                Some(b'b') | Some(b'B') => {
                    self.bump();
                    self.bump();
                    2
                }
                Some(b'o') | Some(b'O') => {
                    self.bump();
                    self.bump();
                    8
                }
                _ => 10,
            }
        } else {
            10
        };
        let digits_start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'_' || (b as char).is_digit(radix) {
                self.bump();
            } else {
                break;
            }
        }
        if self.pos == digits_start {
            return Err(self.error("expected digits after integer base prefix"));
        }
        let raw = self.source[digits_start..self.pos].replace('_', "");
        let value = u64::from_str_radix(&raw, radix).map_err(|_| {
            self.error(format!(
                "integer literal is outside u64: {}",
                &self.source[start..self.pos]
            ))
        })?;
        Ok(TokenKind::Integer(value))
    }

    fn lex_string(&mut self) -> Result<String, Diagnostic> {
        self.bump();
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error("unterminated string literal")),
                Some(b'"') => {
                    self.bump();
                    return Ok(out);
                }
                Some(b'\n') => return Err(self.error("newline in string literal")),
                Some(b'\\') => {
                    self.bump();
                    out.push(self.escape_char()? as char);
                }
                Some(b) if b.is_ascii() => {
                    self.bump();
                    out.push(b as char);
                }
                Some(_) => {
                    let ch = self.source[self.pos..].chars().next().unwrap();
                    for _ in 0..ch.len_utf8() {
                        self.bump();
                    }
                    out.push(ch);
                }
            }
        }
    }

    fn lex_byte_string(&mut self) -> Result<TokenKind, Diagnostic> {
        let s = self.lex_string()?;
        if !s.is_ascii() {
            return Err(self.error("byte strings must contain only ASCII in v0.1"));
        }
        Ok(TokenKind::ByteString(s.into_bytes()))
    }

    fn lex_byte(&mut self) -> Result<TokenKind, Diagnostic> {
        self.bump();
        let value = match self.peek() {
            Some(b'\\') => {
                self.bump();
                self.escape_char()?
            }
            Some(b) if b.is_ascii() => {
                self.bump();
                b
            }
            _ => return Err(self.error("byte literal must contain one ASCII byte")),
        };
        if self.peek() != Some(b'\'') {
            return Err(self.error("unterminated byte literal"));
        }
        self.bump();
        Ok(TokenKind::Byte(value))
    }

    fn escape_char(&mut self) -> Result<u8, Diagnostic> {
        let b = self
            .peek()
            .ok_or_else(|| self.error("unterminated escape"))?;
        self.bump();
        match b {
            b'n' => Ok(b'\n'),
            b'r' => Ok(b'\r'),
            b't' => Ok(b'\t'),
            b'0' => Ok(0),
            b'\\' => Ok(b'\\'),
            b'"' => Ok(b'"'),
            b'\'' => Ok(b'\''),
            _ => Err(self.error(format!("unsupported escape `\\{}`", b as char))),
        }
    }

    fn skip_trivia(&mut self) -> Result<(), Diagnostic> {
        loop {
            while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
                self.bump();
            }
            if self.peek() == Some(b'/') && self.peek_n(1) == Some(b'/') {
                while self.peek().is_some_and(|b| b != b'\n') {
                    self.bump();
                }
                continue;
            }
            if self.peek() == Some(b'/') && self.peek_n(1) == Some(b'*') {
                self.bump();
                self.bump();
                let mut depth = 1usize;
                while depth != 0 {
                    match (self.peek(), self.peek_n(1)) {
                        (None, _) => return Err(self.error("unterminated block comment")),
                        (Some(b'/'), Some(b'*')) => {
                            self.bump();
                            self.bump();
                            depth += 1;
                        }
                        (Some(b'*'), Some(b'/')) => {
                            self.bump();
                            self.bump();
                            depth -= 1;
                        }
                        _ => self.bump(),
                    }
                }
                continue;
            }
            return Ok(());
        }
    }

    fn one(&mut self, kind: TokenKind) -> Result<TokenKind, Diagnostic> {
        self.bump();
        Ok(kind)
    }

    fn mark(&self) -> (usize, usize, usize) {
        (self.pos, self.line, self.column)
    }

    fn span_from(&self, mark: (usize, usize, usize)) -> Span {
        Span {
            start: mark.0,
            end: self.pos,
            line: mark.1,
            column: mark.2,
        }
    }

    fn error(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            file: self.file.to_string(),
            span: Span {
                start: self.pos,
                end: self.pos.saturating_add(1).min(self.bytes.len()),
                line: self.line,
                column: self.column,
            },
            message: message.into(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_n(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    fn bump(&mut self) {
        if let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }
    }
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

fn is_ident_continue(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_core_tokens_and_literals() {
        let tokens = lex(
            "test.oc",
            r#"module kernel::boot; let x: u64 = 0xff_00; let s = b"ok\n";"#,
        )
        .unwrap();
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Integer(0xff00)));
        assert!(tokens
            .iter()
            .any(|t| t.kind == TokenKind::ByteString(b"ok\n".to_vec())));
        assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);
    }

    #[test]
    fn nested_comments_are_ignored() {
        let tokens = lex("test.oc", "/* a /* b */ c */ fn f() {}").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Fn);
    }
}
