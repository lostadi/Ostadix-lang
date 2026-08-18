use std::mem::discriminant;

use super::ast::*;
use super::lexer::{lex, Token, TokenKind};
use super::{Diagnostic, Span};

pub fn parse(file: &str, source: &str) -> Result<SourceModule, Diagnostic> {
    let tokens = lex(file, source)?;
    Parser {
        file,
        tokens,
        pos: 0,
    }
    .parse_module()
}

struct Parser<'a> {
    file: &'a str,
    tokens: Vec<Token>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn parse_module(&mut self) -> Result<SourceModule, Diagnostic> {
        let start = self.expect(TokenKind::Module, "expected `module` declaration")?;
        let name = self.parse_path()?;
        self.expect(TokenKind::Semi, "expected `;` after module declaration")?;

        let mut uses = Vec::new();
        while self.at(&TokenKind::Use) {
            let use_start = self.bump().span;
            let path = self.parse_path()?;
            let alias = if self.eat(&TokenKind::As).is_some() {
                Some(self.expect_ident("expected import alias after `as`")?.0)
            } else {
                None
            };
            let end = self.expect(TokenKind::Semi, "expected `;` after use declaration")?;
            uses.push(UseDecl {
                path,
                alias,
                span: use_start.join(end.span),
            });
        }

        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            items.push(self.parse_item()?);
        }
        let end = self.current().span;
        Ok(SourceModule {
            name,
            uses,
            items,
            span: start.span.join(end),
        })
    }

    fn parse_item(&mut self) -> Result<Item, Diagnostic> {
        let attrs = self.parse_attributes()?;
        let start = attrs.first().map(|a| a.span).unwrap_or(self.current().span);
        let public = self.eat(&TokenKind::Pub).is_some();

        let mut abi = Abi::OCore;
        let extern_ = self.eat(&TokenKind::Extern).is_some();
        if extern_ {
            let token = self.bump();
            let name = match token.kind {
                TokenKind::String(s) => s,
                _ => return Err(self.error_at(token.span, "expected ABI string after `extern`")),
            };
            abi = match name.as_str() {
                "sysv64" | "C" => Abi::SysV64,
                "ocore" => Abi::OCore,
                other => {
                    return Err(self.error_at(
                        token.span,
                        format!("unsupported ABI `{other}`; expected `sysv64` or `ocore`"),
                    ))
                }
            };
        }
        let unsafe_ = self.eat(&TokenKind::Unsafe).is_some();

        let kind = if self.eat(&TokenKind::Fn).is_some() {
            ItemKind::Function(self.parse_function(unsafe_, abi, extern_)?)
        } else if extern_ || unsafe_ {
            return Err(self.error("`extern` and item-level `unsafe` are valid only on functions"));
        } else if self.eat(&TokenKind::Struct).is_some() {
            ItemKind::Struct(self.parse_struct()?)
        } else if self.eat(&TokenKind::Enum).is_some() {
            ItemKind::Enum(self.parse_enum()?)
        } else if self.eat(&TokenKind::Static).is_some() {
            ItemKind::Static(self.parse_static()?)
        } else if self.eat(&TokenKind::Const).is_some() {
            ItemKind::Const(self.parse_const()?)
        } else {
            return Err(self.error("expected function, struct, enum, static, or const item"));
        };
        let span = start.join(self.previous().span);
        Ok(Item {
            attrs,
            public,
            kind,
            span,
        })
    }

    fn parse_attributes(&mut self) -> Result<Vec<Attribute>, Diagnostic> {
        let mut attrs = Vec::new();
        while let Some(at) = self.eat(&TokenKind::At) {
            let (name, name_span) = self.expect_ident("expected attribute name after `@`")?;
            let mut args = Vec::new();
            let mut end = name_span;
            if self.eat(&TokenKind::LParen).is_some() {
                if !self.at(&TokenKind::RParen) {
                    loop {
                        let token = self.bump();
                        let arg =
                            match token.kind {
                                TokenKind::String(s) => AttrArg::String(s),
                                TokenKind::Integer(n) => AttrArg::Integer(n),
                                TokenKind::Ident(s) => AttrArg::Ident(s),
                                _ => return Err(self.error_at(
                                    token.span,
                                    "attribute arguments must be strings, integers, or identifiers",
                                )),
                            };
                        args.push(arg);
                        if self.eat(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                }
                end = self
                    .expect(TokenKind::RParen, "expected `)` after attribute arguments")?
                    .span;
            }
            attrs.push(Attribute {
                name,
                args,
                span: at.span.join(end),
            });
        }
        Ok(attrs)
    }

    fn parse_function(
        &mut self,
        unsafe_: bool,
        abi: Abi,
        extern_: bool,
    ) -> Result<Function, Diagnostic> {
        let (name, _) = self.expect_ident("expected function name")?;
        self.expect(TokenKind::LParen, "expected `(` after function name")?;
        let mut params = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                let (param_name, start) = self.expect_ident("expected parameter name")?;
                self.expect(TokenKind::Colon, "expected `:` after parameter name")?;
                let ty = self.parse_type()?;
                params.push(Param {
                    name: param_name,
                    span: start.join(ty.span),
                    ty,
                });
                if self.eat(&TokenKind::Comma).is_none() {
                    break;
                }
                if self.at(&TokenKind::RParen) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen, "expected `)` after function parameters")?;
        let return_type = if self.eat(&TokenKind::Arrow).is_some() {
            self.parse_type()?
        } else {
            self.synthetic_named_type("void")
        };
        let body = if extern_ {
            self.expect(TokenKind::Semi, "extern function declarations end with `;`")?;
            None
        } else {
            Some(self.parse_block()?)
        };
        Ok(Function {
            name,
            unsafe_,
            abi,
            params,
            return_type,
            body,
        })
    }

    fn parse_struct(&mut self) -> Result<StructDef, Diagnostic> {
        let (name, _) = self.expect_ident("expected struct name")?;
        self.expect(TokenKind::LBrace, "expected `{` after struct name")?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            let (field_name, start) = self.expect_ident("expected field name")?;
            self.expect(TokenKind::Colon, "expected `:` after field name")?;
            let ty = self.parse_type()?;
            fields.push(FieldDef {
                name: field_name,
                span: start.join(ty.span),
                ty,
            });
            if self.eat(&TokenKind::Comma).is_none() && !self.at(&TokenKind::RBrace) {
                return Err(self.error("expected `,` or `}` after struct field"));
            }
        }
        self.bump();
        Ok(StructDef { name, fields })
    }

    fn parse_enum(&mut self) -> Result<EnumDef, Diagnostic> {
        let (name, _) = self.expect_ident("expected enum name")?;
        self.expect(TokenKind::LBrace, "expected `{` after enum name")?;
        let mut variants = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            let (variant_name, start) = self.expect_ident("expected variant name")?;
            let mut payload = Vec::new();
            if self.eat(&TokenKind::LParen).is_some() {
                if !self.at(&TokenKind::RParen) {
                    loop {
                        payload.push(self.parse_type()?);
                        if self.eat(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RParen, "expected `)` after enum payload")?;
            }
            variants.push(VariantDef {
                name: variant_name,
                payload,
                span: start.join(self.previous().span),
            });
            if self.eat(&TokenKind::Comma).is_none() && !self.at(&TokenKind::RBrace) {
                return Err(self.error("expected `,` or `}` after enum variant"));
            }
        }
        self.bump();
        Ok(EnumDef { name, variants })
    }

    fn parse_static(&mut self) -> Result<StaticDef, Diagnostic> {
        let mutable = self.eat(&TokenKind::Mut).is_some();
        let (name, _) = self.expect_ident("expected static name")?;
        self.expect(TokenKind::Colon, "expected `:` after static name")?;
        let ty = self.parse_type()?;
        self.expect(TokenKind::Eq, "expected `=` in static definition")?;
        let init = self.parse_expr()?;
        self.expect(TokenKind::Semi, "expected `;` after static definition")?;
        Ok(StaticDef {
            name,
            mutable,
            ty,
            init,
        })
    }

    fn parse_const(&mut self) -> Result<ConstDef, Diagnostic> {
        let (name, _) = self.expect_ident("expected const name")?;
        self.expect(TokenKind::Colon, "expected `:` after const name")?;
        let ty = self.parse_type()?;
        self.expect(TokenKind::Eq, "expected `=` in const definition")?;
        let init = self.parse_expr()?;
        self.expect(TokenKind::Semi, "expected `;` after const definition")?;
        Ok(ConstDef { name, ty, init })
    }

    fn parse_type(&mut self) -> Result<TypeExpr, Diagnostic> {
        let start = self.current().span;
        if self.eat(&TokenKind::Star).is_some() {
            let mutable = if self.eat(&TokenKind::Mut).is_some() {
                true
            } else {
                self.expect(TokenKind::Const, "expected `const` or `mut` after `*`")?;
                false
            };
            let pointee = self.parse_type()?;
            return Ok(TypeExpr {
                span: start.join(pointee.span),
                kind: TypeExprKind::Pointer {
                    mutable,
                    pointee: Box::new(pointee),
                },
            });
        }
        if self.eat(&TokenKind::LBracket).is_some() {
            let element = self.parse_type()?;
            self.expect(TokenKind::Semi, "expected `;` in array type")?;
            let (len, _) = self.expect_integer("expected array length")?;
            let end = self.expect(TokenKind::RBracket, "expected `]` after array type")?;
            return Ok(TypeExpr {
                span: start.join(end.span),
                kind: TypeExprKind::Array {
                    element: Box::new(element),
                    len,
                },
            });
        }
        if self.eat(&TokenKind::Fn).is_some() {
            self.expect(TokenKind::LParen, "expected `(` in function pointer type")?;
            let mut params = Vec::new();
            if !self.at(&TokenKind::RParen) {
                loop {
                    params.push(self.parse_type()?);
                    if self.eat(&TokenKind::Comma).is_none() {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, "expected `)` in function pointer type")?;
            self.expect(TokenKind::Arrow, "expected `->` in function pointer type")?;
            let result = self.parse_type()?;
            return Ok(TypeExpr {
                span: start.join(result.span),
                kind: TypeExprKind::FnPointer {
                    params,
                    result: Box::new(result),
                },
            });
        }
        let path = self.parse_path()?;
        Ok(TypeExpr {
            kind: TypeExprKind::Named(path),
            span: start.join(self.previous().span),
        })
    }

    fn parse_block(&mut self) -> Result<Block, Diagnostic> {
        let start = self
            .expect(TokenKind::LBrace, "expected `{` to begin block")?
            .span;
        let mut stmts = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.error_at(start, "unclosed block"));
            }
            stmts.push(self.parse_stmt()?);
        }
        let end = self.bump().span;
        Ok(Block {
            stmts,
            span: start.join(end),
        })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        let start = self.current().span;
        let kind = if self.eat(&TokenKind::Let).is_some() {
            let mutable = self.eat(&TokenKind::Mut).is_some();
            let (name, _) = self.expect_ident("expected binding name after `let`")?;
            let ty = if self.eat(&TokenKind::Colon).is_some() {
                Some(self.parse_type()?)
            } else {
                None
            };
            self.expect(TokenKind::Eq, "O-core bindings require an initializer")?;
            let init = self.parse_expr()?;
            self.expect(TokenKind::Semi, "expected `;` after binding")?;
            StmtKind::Let {
                mutable,
                name,
                ty,
                init,
            }
        } else if self.eat(&TokenKind::If).is_some() {
            let condition = self.parse_expr()?;
            let then_block = self.parse_block()?;
            let else_block = if self.eat(&TokenKind::Else).is_some() {
                if self.eat(&TokenKind::If).is_some() {
                    let nested_start = self.previous().span;
                    let nested_condition = self.parse_expr()?;
                    let nested_then = self.parse_block()?;
                    let nested_else = if self.eat(&TokenKind::Else).is_some() {
                        Some(self.parse_block()?)
                    } else {
                        None
                    };
                    let nested = Stmt {
                        span: nested_start.join(self.previous().span),
                        kind: StmtKind::If {
                            condition: nested_condition,
                            then_block: nested_then,
                            else_block: nested_else,
                        },
                    };
                    Some(Block {
                        span: nested.span,
                        stmts: vec![nested],
                    })
                } else {
                    Some(self.parse_block()?)
                }
            } else {
                None
            };
            StmtKind::If {
                condition,
                then_block,
                else_block,
            }
        } else if self.eat(&TokenKind::While).is_some() {
            let condition = self.parse_expr()?;
            let body = self.parse_block()?;
            StmtKind::While { condition, body }
        } else if self.eat(&TokenKind::Loop).is_some() {
            StmtKind::Loop(self.parse_block()?)
        } else if self.eat(&TokenKind::Unsafe).is_some() {
            StmtKind::Unsafe(self.parse_block()?)
        } else if self.eat(&TokenKind::Return).is_some() {
            let value = if self.at(&TokenKind::Semi) {
                None
            } else {
                Some(self.parse_expr()?)
            };
            self.expect(TokenKind::Semi, "expected `;` after return")?;
            StmtKind::Return(value)
        } else if self.eat(&TokenKind::Break).is_some() {
            self.expect(TokenKind::Semi, "expected `;` after break")?;
            StmtKind::Break
        } else if self.eat(&TokenKind::Continue).is_some() {
            self.expect(TokenKind::Semi, "expected `;` after continue")?;
            StmtKind::Continue
        } else {
            let expr = self.parse_expr()?;
            self.expect(TokenKind::Semi, "expected `;` after expression")?;
            StmtKind::Expr(expr)
        };
        Ok(Stmt {
            kind,
            span: start.join(self.previous().span),
        })
    }

    fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, Diagnostic> {
        let lhs = self.parse_binary(1)?;
        let op = match self.current().kind {
            TokenKind::Eq => Some(None),
            TokenKind::PlusEq => Some(Some(BinaryOp::Add)),
            TokenKind::MinusEq => Some(Some(BinaryOp::Sub)),
            TokenKind::StarEq => Some(Some(BinaryOp::Mul)),
            TokenKind::SlashEq => Some(Some(BinaryOp::Div)),
            TokenKind::PercentEq => Some(Some(BinaryOp::Rem)),
            TokenKind::AmpEq => Some(Some(BinaryOp::BitAnd)),
            TokenKind::PipeEq => Some(Some(BinaryOp::BitOr)),
            TokenKind::CaretEq => Some(Some(BinaryOp::BitXor)),
            TokenKind::ShiftLeftEq => Some(Some(BinaryOp::ShiftLeft)),
            TokenKind::ShiftRightEq => Some(Some(BinaryOp::ShiftRight)),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let value = self.parse_assignment()?;
            let span = lhs.span.join(value.span);
            Ok(Expr {
                span,
                kind: ExprKind::Assign {
                    op,
                    target: Box::new(lhs),
                    value: Box::new(value),
                },
            })
        } else {
            Ok(lhs)
        }
    }

    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_unary()?;
        while let Some((op, precedence)) = self.binary_op() {
            if precedence < min_prec {
                break;
            }
            self.bump();
            let rhs = self.parse_binary(precedence + 1)?;
            let span = lhs.span.join(rhs.span);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    fn binary_op(&self) -> Option<(BinaryOp, u8)> {
        Some(match self.current().kind {
            TokenKind::OrOr => (BinaryOp::LogicalOr, 1),
            TokenKind::AndAnd => (BinaryOp::LogicalAnd, 2),
            TokenKind::Pipe => (BinaryOp::BitOr, 3),
            TokenKind::Caret => (BinaryOp::BitXor, 4),
            TokenKind::Amp => (BinaryOp::BitAnd, 5),
            TokenKind::EqEq => (BinaryOp::Eq, 6),
            TokenKind::NotEq => (BinaryOp::NotEq, 6),
            TokenKind::Less => (BinaryOp::Less, 7),
            TokenKind::LessEq => (BinaryOp::LessEq, 7),
            TokenKind::Greater => (BinaryOp::Greater, 7),
            TokenKind::GreaterEq => (BinaryOp::GreaterEq, 7),
            TokenKind::ShiftLeft => (BinaryOp::ShiftLeft, 8),
            TokenKind::ShiftRight => (BinaryOp::ShiftRight, 8),
            TokenKind::Plus => (BinaryOp::Add, 9),
            TokenKind::Minus => (BinaryOp::Sub, 9),
            TokenKind::Star => (BinaryOp::Mul, 10),
            TokenKind::Slash => (BinaryOp::Div, 10),
            TokenKind::Percent => (BinaryOp::Rem, 10),
            _ => return None,
        })
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        let start = self.current().span;
        let op = if self.eat(&TokenKind::Minus).is_some() {
            Some(UnaryOp::Neg)
        } else if self.eat(&TokenKind::Bang).is_some() {
            Some(UnaryOp::Not)
        } else if self.eat(&TokenKind::Tilde).is_some() {
            Some(UnaryOp::BitNot)
        } else if self.eat(&TokenKind::Star).is_some() {
            Some(UnaryOp::Deref)
        } else if self.eat(&TokenKind::Amp).is_some() {
            let mutable = self.eat(&TokenKind::Mut).is_some();
            Some(UnaryOp::AddressOf { mutable })
        } else {
            None
        };
        if let Some(op) = op {
            let operand = self.parse_unary()?;
            return Ok(Expr {
                span: start.join(operand.span),
                kind: ExprKind::Unary {
                    op,
                    operand: Box::new(operand),
                },
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.eat(&TokenKind::LParen).is_some() {
                let mut args = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    loop {
                        args.push(self.parse_expr()?);
                        if self.eat(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                }
                let end = self.expect(TokenKind::RParen, "expected `)` after arguments")?;
                let span = expr.span.join(end.span);
                expr = Expr {
                    span,
                    kind: ExprKind::Call {
                        callee: Box::new(expr),
                        args,
                    },
                };
            } else if self.eat(&TokenKind::LBracket).is_some() {
                let index = self.parse_expr()?;
                let end = self.expect(TokenKind::RBracket, "expected `]` after index")?;
                let span = expr.span.join(end.span);
                expr = Expr {
                    span,
                    kind: ExprKind::Index {
                        base: Box::new(expr),
                        index: Box::new(index),
                    },
                };
            } else if self.eat(&TokenKind::Dot).is_some() {
                let (name, end) = self.expect_ident("expected field name after `.`")?;
                let span = expr.span.join(end);
                expr = Expr {
                    span,
                    kind: ExprKind::Field {
                        base: Box::new(expr),
                        name,
                    },
                };
            } else if self.eat(&TokenKind::As).is_some() {
                let ty = self.parse_type()?;
                let span = expr.span.join(ty.span);
                expr = Expr {
                    span,
                    kind: ExprKind::Cast {
                        value: Box::new(expr),
                        ty,
                    },
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, Diagnostic> {
        let token = self.bump();
        let span = token.span;
        let kind = match token.kind {
            TokenKind::Integer(n) => ExprKind::Integer(n),
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            TokenKind::Byte(b) => ExprKind::Byte(b),
            TokenKind::String(s) => ExprKind::String(s),
            TokenKind::ByteString(v) => ExprKind::ByteString(v),
            TokenKind::Ident(first) => {
                let path = self.parse_path_tail(first);
                if self.looks_like_struct_literal() {
                    self.bump();
                    let mut fields = Vec::new();
                    while !self.at(&TokenKind::RBrace) {
                        let (name, _) = self.expect_ident("expected struct field name")?;
                        self.expect(TokenKind::Colon, "expected `:` in struct initializer")?;
                        fields.push((name, self.parse_expr()?));
                        if self.eat(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                    self.expect(TokenKind::RBrace, "expected `}` after struct initializer")?;
                    ExprKind::Struct { path, fields }
                } else {
                    ExprKind::Path(path)
                }
            }
            TokenKind::LParen => {
                let expr = self.parse_expr()?;
                self.expect(TokenKind::RParen, "expected `)`")?;
                return Ok(expr);
            }
            TokenKind::LBracket => {
                if self.at(&TokenKind::RBracket) {
                    self.bump();
                    ExprKind::Array(Vec::new())
                } else {
                    let first = self.parse_expr()?;
                    if self.eat(&TokenKind::Semi).is_some() {
                        let (len, _) = self.expect_integer("expected array repeat length")?;
                        self.expect(TokenKind::RBracket, "expected `]` after array repeat")?;
                        ExprKind::ArrayRepeat {
                            value: Box::new(first),
                            len,
                        }
                    } else {
                        let mut values = vec![first];
                        while self.eat(&TokenKind::Comma).is_some() {
                            if self.at(&TokenKind::RBracket) {
                                break;
                            }
                            values.push(self.parse_expr()?);
                        }
                        self.expect(TokenKind::RBracket, "expected `]` after array")?;
                        ExprKind::Array(values)
                    }
                }
            }
            TokenKind::Asm => {
                return self.parse_asm(span);
            }
            _ => return Err(self.error_at(span, "expected expression")),
        };
        Ok(Expr {
            kind,
            span: span.join(self.previous().span),
        })
    }

    fn parse_asm(&mut self, start: Span) -> Result<Expr, Diagnostic> {
        self.expect(TokenKind::Bang, "expected `!` after `asm`")?;
        self.expect(TokenKind::LParen, "expected `(` after `asm!`")?;
        let token = self.bump();
        let template = match token.kind {
            TokenKind::String(s) => s,
            _ => return Err(self.error_at(token.span, "asm template must be a string literal")),
        };
        let mut operands = Vec::new();
        let mut options = Vec::new();
        while self.eat(&TokenKind::Comma).is_some() {
            if self.eat(&TokenKind::Options).is_some() {
                self.expect(TokenKind::LParen, "expected `(` after options")?;
                if !self.at(&TokenKind::RParen) {
                    loop {
                        options.push(self.expect_ident("expected assembly option")?.0);
                        if self.eat(&TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RParen, "expected `)` after assembly options")?;
                continue;
            }
            let mode = self.bump();
            self.expect(TokenKind::LParen, "expected register constraint")?;
            let reg_token = self.bump();
            let register = match reg_token.kind {
                TokenKind::String(s) => s,
                _ => {
                    return Err(
                        self.error_at(reg_token.span, "assembly register must be a string literal")
                    )
                }
            };
            self.expect(TokenKind::RParen, "expected `)` after register")?;
            let operand = match mode.kind {
                TokenKind::In => AsmOperand::In {
                    register,
                    value: self.parse_expr()?,
                },
                TokenKind::Out => AsmOperand::Out {
                    register,
                    target: self.parse_expr()?,
                },
                TokenKind::InOut => {
                    let input = self.parse_expr()?;
                    self.expect(TokenKind::Arrow, "expected `->` in inout operand")?;
                    let output = self.parse_expr()?;
                    AsmOperand::InOut {
                        register,
                        input,
                        output,
                    }
                }
                _ => {
                    return Err(self.error_at(
                        mode.span,
                        "expected `in`, `out`, `inout`, or `options` in asm",
                    ))
                }
            };
            operands.push(operand);
        }
        let end = self.expect(TokenKind::RParen, "expected `)` after asm expression")?;
        Ok(Expr {
            span: start.join(end.span),
            kind: ExprKind::Asm(AsmExpr {
                template,
                operands,
                options,
            }),
        })
    }

    fn looks_like_struct_literal(&self) -> bool {
        if !self.at(&TokenKind::LBrace) {
            return false;
        }
        matches!(
            (self.tokens.get(self.pos + 1), self.tokens.get(self.pos + 2)),
            (
                Some(Token {
                    kind: TokenKind::Ident(_),
                    ..
                }),
                Some(Token {
                    kind: TokenKind::Colon,
                    ..
                })
            ) | (
                Some(Token {
                    kind: TokenKind::RBrace,
                    ..
                }),
                _
            )
        )
    }

    fn parse_path(&mut self) -> Result<Path, Diagnostic> {
        let (first, _) = self.expect_ident("expected identifier")?;
        Ok(self.parse_path_tail(first))
    }

    fn parse_path_tail(&mut self, first: String) -> Path {
        let mut path = vec![first];
        while self.eat(&TokenKind::ColonColon).is_some() {
            // Keep this routine infallible for expression parsing; malformed
            // tails are reported by leaving the unexpected token for the caller.
            match self.current().kind.clone() {
                TokenKind::Ident(s) => {
                    self.bump();
                    path.push(s);
                }
                _ => break,
            }
        }
        path
    }

    fn synthetic_named_type(&self, name: &str) -> TypeExpr {
        TypeExpr {
            kind: TypeExprKind::Named(vec![name.to_string()]),
            span: self.previous().span,
        }
    }

    fn expect_ident(&mut self, message: &str) -> Result<(String, Span), Diagnostic> {
        let token = self.bump();
        match token.kind {
            TokenKind::Ident(s) => Ok((s, token.span)),
            _ => Err(self.error_at(token.span, message)),
        }
    }

    fn expect_integer(&mut self, message: &str) -> Result<(u64, Span), Diagnostic> {
        let token = self.bump();
        match token.kind {
            TokenKind::Integer(n) => Ok((n, token.span)),
            _ => Err(self.error_at(token.span, message)),
        }
    }

    fn expect(&mut self, kind: TokenKind, message: &str) -> Result<Token, Diagnostic> {
        if self.at(&kind) {
            Ok(self.bump())
        } else {
            Err(self.error(message))
        }
    }

    fn eat(&mut self, kind: &TokenKind) -> Option<Token> {
        if self.at(kind) {
            Some(self.bump())
        } else {
            None
        }
    }

    fn at(&self, kind: &TokenKind) -> bool {
        discriminant(&self.current().kind) == discriminant(kind)
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.pos.saturating_sub(1)]
    }

    fn bump(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if !matches!(token.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        token
    }

    fn error(&self, message: impl Into<String>) -> Diagnostic {
        self.error_at(self.current().span, message)
    }

    fn error_at(&self, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            file: self.file.to_string(),
            span,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_system_language_surface() {
        let source = r#"
module kernel::main;
use kernel::serial::write as serial_write;

@packed
struct Header { magic: u32, flags: u32 }
enum State { empty, ready(u64), failed(i32) }
static mut NEXT: usize = 0x200000;

@export
@link_section(".text.kernel")
unsafe fn kernel_main(info: *const Header) -> never {
    let mut n: usize = 0;
    while n < 4 {
        unsafe { volatile_store((&mut NEXT), n); }
        n += 1;
    }
    loop { asm!("hlt", options(nomem, nostack)); }
}
"#;
        let module = parse("kernel.oc", source).unwrap();
        assert_eq!(module.name, vec!["kernel", "main"]);
        assert_eq!(module.items.len(), 4);
        let function = match &module.items[3].kind {
            ItemKind::Function(f) => f,
            _ => panic!("expected function"),
        };
        assert!(function.unsafe_);
        assert_eq!(function.body.as_ref().unwrap().stmts.len(), 3);
    }

    #[test]
    fn parses_arrays_pointers_and_function_pointers() {
        let source = r#"
module types;
struct Vtable { call: fn(*mut u8, [u64; 4]) -> i32 }
"#;
        let module = parse("types.oc", source).unwrap();
        let def = match &module.items[0].kind {
            ItemKind::Struct(s) => s,
            _ => panic!(),
        };
        assert!(matches!(
            def.fields[0].ty.kind,
            TypeExprKind::FnPointer { .. }
        ));
    }
}
