use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::eval::Policy;
use crate::hgraph::HGraph;
use crate::ir::{ExecutionPlan, OIrProgram};

use super::analyze::{
    backend_catalog_projection_sha256, digest_fields, graph_sha256, oir_sha256,
    validate_canonical_solved_graph,
};
use super::fact::ANALYZER_ID_V4;

pub const EXECUTION_INTENT_SCHEMA_V1: &str = "oexec.execution-intent/v1";
const EXECUTION_INTENT_DIGEST_DOMAIN_V1: &str = "ostadix-execution-intent/v1";

/// Stable, authority-free identity of one analyzed source-level execution
/// intent. This projection deliberately excludes runtime discovery, backend
/// artifacts, process identity, environment state, evidence bundles, and
/// admission records. A matching digest therefore establishes sameness of the
/// modeled intent, not permission or live realizability.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionIntentV1 {
    pub schema: String,
    pub source_sha256: String,
    pub oir_sha256: String,
    pub plan_sha256: String,
    pub analyzed_graph_sha256: String,
    pub backend_catalog_projection_sha256: String,
    pub analyzer_id: String,
    pub analyzer_sha256: String,
    pub base_policy: String,
    pub execution_intent_sha256: String,
}

impl ExecutionIntentV1 {
    /// Project stable analyzed semantics without creating execution authority.
    /// Callers must pass the exact source bytes whose lowered program is being
    /// identified. Runtime dispatch must still compile and verify a fresh
    /// `AdmittedExecution` after any required-intent comparison succeeds.
    pub fn compile(
        source: &[u8],
        program: &OIrProgram,
        plan: &ExecutionPlan,
        graph: &HGraph,
        base_policy: Policy,
    ) -> Result<Self> {
        Self::compile_with_source_sha256(&source_sha256(source), program, plan, graph, base_policy)
    }

    /// Compile from an already-computed exact source digest. The CLI uses
    /// this form so shebang stripping does not require retaining a second full
    /// copy of a potentially large source file.
    pub fn compile_with_source_sha256(
        source_sha256: &str,
        program: &OIrProgram,
        plan: &ExecutionPlan,
        graph: &HGraph,
        base_policy: Policy,
    ) -> Result<Self> {
        validate_sha256("source", source_sha256)?;
        validate_canonical_solved_graph(program, plan, graph)
            .context("execution intent rejected noncanonical static execution input")?;

        let mut intent = Self {
            schema: EXECUTION_INTENT_SCHEMA_V1.to_string(),
            source_sha256: source_sha256.to_string(),
            oir_sha256: oir_sha256(program),
            plan_sha256: sha256_bytes(plan.to_text().as_bytes()),
            analyzed_graph_sha256: graph_sha256(graph),
            backend_catalog_projection_sha256: backend_catalog_projection_sha256(plan),
            analyzer_id: ANALYZER_ID_V4.to_string(),
            analyzer_sha256: sha256_bytes(ANALYZER_ID_V4.as_bytes()),
            base_policy: policy_name(base_policy).to_string(),
            execution_intent_sha256: String::new(),
        };
        intent.execution_intent_sha256 = intent.recompute_sha256();
        intent.validate()?;
        Ok(intent)
    }

    /// Validate schema, field shape, analyzer identity, and the canonical
    /// digest. Validation does not turn this descriptive projection into an
    /// admission record or capability.
    pub fn validate(&self) -> Result<()> {
        if self.schema != EXECUTION_INTENT_SCHEMA_V1 {
            bail!(
                "unsupported execution-intent schema `{}` (expected `{EXECUTION_INTENT_SCHEMA_V1}`)",
                self.schema
            );
        }
        for (label, digest) in [
            ("source", &self.source_sha256),
            ("OIR", &self.oir_sha256),
            ("plan", &self.plan_sha256),
            ("analyzed graph", &self.analyzed_graph_sha256),
            (
                "backend catalog projection",
                &self.backend_catalog_projection_sha256,
            ),
            ("analyzer", &self.analyzer_sha256),
            ("execution intent", &self.execution_intent_sha256),
        ] {
            validate_sha256(label, digest)?;
        }
        if self.analyzer_id != ANALYZER_ID_V4 {
            bail!(
                "execution intent names unsupported analyzer `{}` (expected `{ANALYZER_ID_V4}`)",
                self.analyzer_id
            );
        }
        if self.analyzer_sha256 != sha256_bytes(self.analyzer_id.as_bytes()) {
            bail!("execution intent analyzer identity digest does not match its analyzer ID");
        }
        if !matches!(self.base_policy.as_str(), "eager" | "lazy" | "autonomous") {
            bail!(
                "execution intent names unsupported base policy `{}`",
                self.base_policy
            );
        }
        if self.execution_intent_sha256 != self.recompute_sha256() {
            bail!("execution intent digest does not match its canonical fields");
        }
        Ok(())
    }

    /// Compare an expected same-intent gate against this freshly recomputed
    /// projection. This is intentionally only a mismatch check; it neither
    /// authorizes execution nor replaces live V4 admission.
    pub fn verify_required(
        &self,
        expected_source_sha256: &str,
        expected_execution_intent_sha256: &str,
    ) -> Result<()> {
        validate_sha256("required source", expected_source_sha256)?;
        validate_sha256(
            "required execution intent",
            expected_execution_intent_sha256,
        )?;
        if self.source_sha256 != expected_source_sha256 {
            bail!(
                "required source SHA-256 mismatch: expected {}, recomputed {}",
                expected_source_sha256,
                self.source_sha256
            );
        }
        if self.execution_intent_sha256 != expected_execution_intent_sha256 {
            bail!(
                "required execution-intent SHA-256 mismatch: expected {}, recomputed {}",
                expected_execution_intent_sha256,
                self.execution_intent_sha256
            );
        }
        Ok(())
    }

    fn recompute_sha256(&self) -> String {
        digest_fields(
            EXECUTION_INTENT_DIGEST_DOMAIN_V1,
            &[
                &self.schema,
                &self.source_sha256,
                &self.oir_sha256,
                &self.plan_sha256,
                &self.analyzed_graph_sha256,
                &self.backend_catalog_projection_sha256,
                &self.analyzer_id,
                &self.analyzer_sha256,
                &self.base_policy,
            ],
        )
    }
}

pub fn source_sha256(source: &[u8]) -> String {
    sha256_bytes(source)
}

fn policy_name(policy: Policy) -> &'static str {
    match policy {
        Policy::Eager => "eager",
        Policy::Lazy => "lazy",
        Policy::Autonomous => "autonomous",
    }
}

fn validate_sha256(label: &str, digest: &str) -> Result<()> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} SHA-256 must contain exactly 64 hexadecimal characters");
    }
    if digest.bytes().any(|byte| byte.is_ascii_uppercase()) {
        bail!("{label} SHA-256 must use canonical lowercase hexadecimal");
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::OIr;

    fn intent_for(program: &OIrProgram) -> ExecutionIntentV1 {
        let plan = program.plan();
        let mut graph = program.hgraph_for_plan(&plan).unwrap();
        crate::hgraph::solve::solve_types(&mut graph).unwrap();
        ExecutionIntentV1::compile(b"text^(stable)_text", program, &plan, &graph, Policy::Eager)
            .unwrap()
    }

    #[test]
    fn intent_is_stable_and_json_round_trips() {
        let program = OIrProgram {
            nodes: vec![OIr::Text("stable".to_string())],
        };
        let first = intent_for(&program);
        let second = intent_for(&program);
        assert_eq!(first, second);

        let json = serde_json::to_string(&first).unwrap();
        let decoded: ExecutionIntentV1 = serde_json::from_str(&json).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, first);
    }

    #[test]
    fn source_and_policy_are_digest_bound() {
        let program = OIrProgram {
            nodes: vec![OIr::Text("stable".to_string())],
        };
        let plan = program.plan();
        let mut graph = program.hgraph_for_plan(&plan).unwrap();
        crate::hgraph::solve::solve_types(&mut graph).unwrap();
        let eager = ExecutionIntentV1::compile(
            b"text^(stable)_text",
            &program,
            &plan,
            &graph,
            Policy::Eager,
        )
        .unwrap();
        let changed_source = ExecutionIntentV1::compile(
            b"text^(stable)_text\n",
            &program,
            &plan,
            &graph,
            Policy::Eager,
        )
        .unwrap();
        let lazy = ExecutionIntentV1::compile(
            b"text^(stable)_text",
            &program,
            &plan,
            &graph,
            Policy::Lazy,
        )
        .unwrap();

        assert_ne!(eager.source_sha256, changed_source.source_sha256);
        assert_ne!(
            eager.execution_intent_sha256,
            changed_source.execution_intent_sha256
        );
        assert_ne!(eager.execution_intent_sha256, lazy.execution_intent_sha256);
    }

    #[test]
    fn validation_rejects_tampered_projection() {
        let program = OIrProgram {
            nodes: vec![OIr::Text("stable".to_string())],
        };
        let mut intent = intent_for(&program);
        intent.plan_sha256 = "00".repeat(32);
        let error = intent.validate().unwrap_err().to_string();
        assert!(
            error.contains("does not match its canonical fields"),
            "{error}"
        );
    }

    #[test]
    fn intent_rejects_an_unsolved_or_structurally_divergent_graph() {
        let program = OIrProgram {
            nodes: vec![OIr::Exec {
                lang: "python".to_string(),
                env_id: 0,
                attr: None,
                backend: crate::ir::BackendRegistry::global().interface_for("python"),
                body: vec![OIr::Text("__oval_result__ = 1".to_string())],
            }],
        };
        let plan = program.plan();
        let graph = program.hgraph_for_plan(&plan).unwrap();

        let error = ExecutionIntentV1::compile(
            b"python^(__oval_result__ = 1)_python",
            &program,
            &plan,
            &graph,
            Policy::Eager,
        )
        .expect_err("a stable intent must require the canonical solved graph");
        assert!(
            format!("{error:#}").contains("canonical solved HGraph"),
            "{error:#}"
        );
    }
}
