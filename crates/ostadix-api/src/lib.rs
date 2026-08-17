//! Stable, deliberately narrow embedding surface for Ostadix-lang.
//!
//! The facade owns its runtime and error vocabulary. Implementation modules
//! remain private so downstream code does not become coupled to the evaluator,
//! parser, or backend-catalog layouts.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use o_lang::api::Parser;
use o_lang::eval::Evaluator;
use o_lang::ir::BackendRegistry;

pub use o_lang::api::{
    BackendAuthority, BigInt, CapabilityKind, DecimalSpecial, FloatFormat, FloatSpecial, GraphNode,
    GroupMode, NativeBoundary, NativeCodecSafety, NativeIdentity, NodeId, OBytes, OKeyword,
    ONative, ONumber, OSymbol, OText, OValue, RehydratePolicy, RequestKind, RuntimeBoundary,
    SeqKind, SetKind, SnapshotKind,
};

/// The stable stage at which a facade evaluation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeStage {
    Parse,
    Evaluate,
}

impl fmt::Display for RuntimeStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Parse => "parse",
            Self::Evaluate => "evaluate",
        })
    }
}

/// A stable facade-owned diagnostic that does not expose runtime error types.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RuntimeError {
    stage: RuntimeStage,
    message: String,
}

impl RuntimeError {
    fn new(stage: RuntimeStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }

    pub const fn stage(&self) -> RuntimeStage {
        self.stage
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.stage, self.message)
    }
}

impl Error for RuntimeError {}

/// An owned Ostadix runtime configured for one explicit backend-shim directory.
pub struct Runtime {
    backends: HashSet<String>,
    evaluator: Evaluator,
}

impl Runtime {
    /// Construct a runtime over the caller-selected backend-shim directory.
    pub fn new(shim_dir: impl Into<PathBuf>) -> Self {
        let backends = BackendRegistry::global().registered_backend_tags();
        let evaluator = Evaluator::new(shim_dir.into()).with_registered_backends(backends.clone());
        Self {
            backends,
            evaluator,
        }
    }

    /// Parse and evaluate one complete O source document. A leading shebang is
    /// excluded from executable syntax by the same rule as the O CLI. Each
    /// call receives a fresh lexical scope, while this owned runtime retains
    /// the evaluator's process registry and persistent backend actors.
    pub fn evaluate(&mut self, source: &str) -> Result<OValue, RuntimeError> {
        let source = strip_initial_shebang(source);
        let nodes = Parser::new(source, &self.backends)
            .parse()
            .map_err(|error| RuntimeError::new(RuntimeStage::Parse, format!("{error:#}")))?;
        self.evaluator
            .eval_document(nodes)
            .map_err(|error| RuntimeError::new(RuntimeStage::Evaluate, format!("{error:#}")))
    }
}

fn strip_initial_shebang(source: &str) -> &str {
    if !source.starts_with("#!") {
        return source;
    }
    source
        .find('\n')
        .map_or("", |newline| &source[newline + 1..])
}
