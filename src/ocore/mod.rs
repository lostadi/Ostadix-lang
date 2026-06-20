//! Native O-core compiler pipeline.
//!
//! This module is intentionally independent of `crate::ir`: orchestration OIR
//! models backend execution, while O-core HIR/MIR model statically typed native
//! computation.

pub mod ast;
pub mod capability_bridge;
pub mod codegen;
pub mod driver;
pub mod hir;
pub mod lexer;
pub mod mir;
pub mod parser;
pub mod typeck;

pub use driver::{compile, CompileOptions, CompileOutput, EmitKind, Target};

/// Byte range and source location attached to syntax and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}

impl Span {
    pub fn join(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            line: self.line,
            column: self.column,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub file: String,
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.file, self.span.line, self.span.column, self.message
        )
    }
}

impl std::error::Error for Diagnostic {}
