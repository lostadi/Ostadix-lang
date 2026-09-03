//! Bundle-bound admission and output checking for validated route reuse.
//!
//! A validated-selection receipt is an unsigned observation, not executable
//! authority.  These helpers let a higher-level intent layer compare that
//! observation with one freshly assembled project and then check the selected
//! route's declared output after reuse.  They never dispatch a command.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::model::{
    OExecutionResult, ProjectBundle, RoutePolicy, ValidatedSelectionReceiptV1,
    VALIDATED_SELECTION_EQUIVALENCE_V1, VALIDATED_SELECTION_RULE_V1,
};
use super::runtime::{resolve_selection, validated_selection_observation};

pub const SELECTION_REUSE_CONTRACT_SCHEMA_V1: &str = "ostadix.project-selection-reuse-contract/v1";
pub const SELECTION_REUSE_OUTPUT_CHECK_SCHEMA_V1: &str =
    "ostadix.project-selection-reuse-output-check/v1";
pub const SELECTION_REUSE_EFFECT_BOUNDARY_V1: &str = "declared_pure_transitive_routes/v1";

/// Exact project and comparison coordinates admitted for one reuse decision.
///
/// This is deliberately bundle-bound rather than runtime-bound.  It does not
/// claim that ambient executables, PATH, device state, the network, or other
/// undeclared inputs are unchanged. [`check_selection_reuse_output`] is the
/// fail-closed output postcondition for that remaining uncertainty, but it is
/// not transactional and cannot undo undeclared effects that already occurred.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionReuseContractV1 {
    pub schema: String,
    pub project_name: String,
    pub bundle_sha256: String,
    pub target: String,
    pub ordered_alternatives: Vec<String>,
    pub evidence_policy: String,
    pub equivalence_contract: String,
    pub selection_rule: String,
    /// Admission requires every alternative and transitive prerequisite to
    /// carry the bundle author's explicit `pure = true` declaration. This is
    /// a declaration, not sandbox proof, and is bound by the bundle digest.
    pub effect_boundary: String,
    pub reference_route_id: String,
    pub selected_route_id: String,
    pub expected_declared_output_sha256: String,
    pub benchmark_hgraph_sha256: String,
    pub benchmark_deployment_sha256: String,
    pub route_declaration_sha256: Vec<String>,
}

impl SelectionReuseContractV1 {
    /// Construct a contract only after the current project still matches the
    /// exact ordered benchmark selection recorded by `receipt`.
    pub fn from_current_project(
        bundle: &ProjectBundle,
        receipt: &ValidatedSelectionReceiptV1,
        benchmark_hgraph_sha256: impl Into<String>,
        benchmark_deployment_sha256: impl Into<String>,
        route_declaration_sha256: Vec<String>,
    ) -> Result<Self, String> {
        receipt.validate()?;
        let bundle_sha256 = hex::encode(Sha256::digest(
            super::bundle::serialize(bundle)
                .map_err(|_| "failed to serialize the current project bundle".to_string())?,
        ));
        if bundle_sha256 != receipt.bundle_sha256 || bundle.name != receipt.project_name {
            return Err(
                "the current project bundle does not match the validated selection run".to_string(),
            );
        }

        let selection = resolve_selection(
            bundle,
            Some(&receipt.target),
            Some(RoutePolicy::BenchmarkValidateAndSelect),
        )
        .map_err(|_| {
            "the current project no longer exposes the recorded optimization route set".to_string()
        })?;
        let receipt_alternatives = receipt
            .candidates
            .iter()
            .map(|candidate| candidate.route_id.clone())
            .collect::<Vec<_>>();
        if selection.target != receipt.target
            || selection.policy != RoutePolicy::BenchmarkValidateAndSelect
            || selection.alternatives != receipt_alternatives
            || selection.alternatives.first() != Some(&receipt.reference_route_id)
        {
            return Err(
                "the current ordered optimization alternatives do not match the validated selection run"
                    .to_string(),
            );
        }
        validate_selection_reuse_effect_boundary(bundle, &selection.alternatives)?;
        let selected = receipt
            .candidates
            .iter()
            .find(|candidate| candidate.route_id == receipt.selected_route_id)
            .filter(|candidate| candidate.disposition.is_eligible())
            .ok_or_else(|| {
                "the validated selection does not contain an eligible selected route".to_string()
            })?;

        let contract = Self {
            schema: SELECTION_REUSE_CONTRACT_SCHEMA_V1.to_string(),
            project_name: bundle.name.clone(),
            bundle_sha256,
            target: receipt.target.clone(),
            ordered_alternatives: selection.alternatives,
            evidence_policy: RoutePolicy::BenchmarkValidateAndSelect.token(),
            equivalence_contract: receipt.equivalence_contract.clone(),
            selection_rule: receipt.selection_rule.clone(),
            effect_boundary: SELECTION_REUSE_EFFECT_BOUNDARY_V1.to_string(),
            reference_route_id: receipt.reference_route_id.clone(),
            selected_route_id: receipt.selected_route_id.clone(),
            expected_declared_output_sha256: selected.declared_output_sha256.clone(),
            benchmark_hgraph_sha256: benchmark_hgraph_sha256.into(),
            benchmark_deployment_sha256: benchmark_deployment_sha256.into(),
            route_declaration_sha256,
        };
        contract.validate()?;
        Ok(contract)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SELECTION_REUSE_CONTRACT_SCHEMA_V1
            || self.evidence_policy != RoutePolicy::BenchmarkValidateAndSelect.token()
            || self.equivalence_contract != VALIDATED_SELECTION_EQUIVALENCE_V1
            || self.selection_rule != VALIDATED_SELECTION_RULE_V1
            || self.effect_boundary != SELECTION_REUSE_EFFECT_BOUNDARY_V1
        {
            return Err("selection-reuse contract has unsupported coordinates".to_string());
        }
        for (value, label) in [
            (&self.project_name, "project name"),
            (&self.target, "selection target"),
            (&self.reference_route_id, "reference route id"),
            (&self.selected_route_id, "selected route id"),
        ] {
            validate_text(value, label)?;
        }
        for (value, label) in [
            (&self.bundle_sha256, "bundle digest"),
            (
                &self.expected_declared_output_sha256,
                "expected output digest",
            ),
            (&self.benchmark_hgraph_sha256, "benchmark HGraph digest"),
            (
                &self.benchmark_deployment_sha256,
                "benchmark deployment digest",
            ),
        ] {
            validate_lower_hex_64(value, label)?;
        }
        if self.ordered_alternatives.len() < 2
            || self.ordered_alternatives.first() != Some(&self.reference_route_id)
            || !self
                .ordered_alternatives
                .iter()
                .any(|route| route == &self.selected_route_id)
        {
            return Err(
                "selection-reuse contract has an invalid reference, winner, or alternative set"
                    .to_string(),
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for route in &self.ordered_alternatives {
            validate_text(route, "alternative route id")?;
            if !seen.insert(route) {
                return Err("selection-reuse contract repeats an alternative".to_string());
            }
        }
        for digest in &self.route_declaration_sha256 {
            let digest = digest.strip_prefix("sha256:").ok_or_else(|| {
                "selection-reuse route declaration identity lacks sha256 prefix".to_string()
            })?;
            validate_lower_hex_64(digest, "route declaration digest")?;
        }
        Ok(())
    }

    /// Domain-separated identity used by embedders as a safe lookup key.
    pub fn sha256(&self) -> Result<String, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("failed to encode selection-reuse contract: {error}"))?;
        let mut hasher = Sha256::new();
        hasher.update(b"ostadix.project-selection-reuse-contract/v1\0");
        hasher.update(bytes);
        Ok(hex::encode(hasher.finalize()))
    }
}

/// Confirm the author-declared effect boundary required before a validated
/// winner may be substituted on a later execution. This checks declarations;
/// it does not sandbox or independently prove hosted command behavior.
pub fn validate_selection_reuse_effect_boundary(
    bundle: &ProjectBundle,
    alternatives: &[String],
) -> Result<(), String> {
    let mut pending = alternatives.to_vec();
    let mut seen = std::collections::BTreeSet::new();
    while let Some(route_id) = pending.pop() {
        if !seen.insert(route_id.clone()) {
            continue;
        }
        let route = bundle.route(&route_id).ok_or_else(|| {
            format!("selection-reuse route `{route_id}` is absent from the project bundle")
        })?;
        if !route.effects.pure
            || route.effects.unknown
            || !route.effects.reads.is_empty()
            || !route.effects.writes.is_empty()
        {
            return Err(format!(
                "selection-reuse route `{route_id}` is not explicitly declared pure"
            ));
        }
        pending.extend(route.prerequisites.iter().cloned());
    }
    Ok(())
}

/// Result of checking one reused route against the previously validated
/// declared-output boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionReuseOutputCheckV1 {
    pub schema: String,
    pub status: SelectionReuseOutputStatusV1,
    pub expected_declared_output_sha256: String,
    pub observed_declared_output_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionReuseOutputStatusV1 {
    Matched,
    RouteFailed,
    DeclaredOutputMismatch,
    ObservationInvalid,
}

impl SelectionReuseOutputCheckV1 {
    pub const fn matched(&self) -> bool {
        matches!(self.status, SelectionReuseOutputStatusV1::Matched)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SELECTION_REUSE_OUTPUT_CHECK_SCHEMA_V1 {
            return Err("selection-reuse output check has an unsupported schema".to_string());
        }
        validate_lower_hex_64(
            &self.expected_declared_output_sha256,
            "expected output digest",
        )?;
        if let Some(observed) = &self.observed_declared_output_sha256 {
            validate_lower_hex_64(observed, "observed output digest")?;
        }
        match self.status {
            SelectionReuseOutputStatusV1::Matched
                if self.observed_declared_output_sha256.as_deref()
                    == Some(self.expected_declared_output_sha256.as_str()) => {}
            SelectionReuseOutputStatusV1::DeclaredOutputMismatch
                if self.observed_declared_output_sha256.is_some()
                    && self.observed_declared_output_sha256.as_deref()
                        != Some(self.expected_declared_output_sha256.as_str()) => {}
            SelectionReuseOutputStatusV1::RouteFailed
            | SelectionReuseOutputStatusV1::ObservationInvalid
                if self.observed_declared_output_sha256.is_none() => {}
            _ => {
                return Err(
                    "selection-reuse output status disagrees with its digest evidence".to_string(),
                )
            }
        }
        Ok(())
    }
}

/// Check the only executed route against the source receipt's selected output.
/// This function observes data already returned by execution and never retries
/// or dispatches another route.
pub fn check_selection_reuse_output(
    bundle: &ProjectBundle,
    contract: &SelectionReuseContractV1,
    result: &OExecutionResult,
) -> SelectionReuseOutputCheckV1 {
    let expected = contract.expected_declared_output_sha256.clone();
    let finish = |status, observed| SelectionReuseOutputCheckV1 {
        schema: SELECTION_REUSE_OUTPUT_CHECK_SCHEMA_V1.to_string(),
        status,
        expected_declared_output_sha256: expected.clone(),
        observed_declared_output_sha256: observed,
    };

    let bundle_matches = super::bundle::serialize(bundle)
        .map(|bytes| hex::encode(Sha256::digest(bytes)) == contract.bundle_sha256)
        .unwrap_or(false);
    if contract.validate().is_err()
        || !bundle_matches
        || result.route_id != contract.selected_route_id
    {
        return finish(SelectionReuseOutputStatusV1::ObservationInvalid, None);
    }
    if !result.succeeded() {
        return finish(SelectionReuseOutputStatusV1::RouteFailed, None);
    }
    let Some(route) = bundle.route(&contract.selected_route_id) else {
        return finish(SelectionReuseOutputStatusV1::ObservationInvalid, None);
    };
    let observation = match validated_selection_observation(result, route) {
        Ok(observation) => observation,
        Err(_) => return finish(SelectionReuseOutputStatusV1::ObservationInvalid, None),
    };
    let observed = match observation.declared_output_sha256() {
        Ok(observed) => observed,
        Err(_) => return finish(SelectionReuseOutputStatusV1::ObservationInvalid, None),
    };
    if observed == expected {
        finish(SelectionReuseOutputStatusV1::Matched, Some(observed))
    } else {
        finish(
            SelectionReuseOutputStatusV1::DeclaredOutputMismatch,
            Some(observed),
        )
    }
}

fn validate_text(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.contains('\0') {
        Err(format!("selection-reuse {label} is empty or contains NUL"))
    } else {
        Ok(())
    }
}

fn validate_lower_hex_64(value: &str, label: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(format!("selection-reuse {label} is not lowercase sha256"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{
        ArtifactCaptureStatus, ExecutionProvenance, OutputCapture, RouteExecutionDisposition,
        RouteProvenance, RouteSpec,
    };

    #[test]
    fn output_check_fails_closed_for_unsuccessful_route() {
        let mut bundle = ProjectBundle::empty("fixture");
        bundle
            .routes
            .push(RouteSpec::new("fast", RouteProvenance::CliOverride));
        let bundle_sha256 = hex::encode(Sha256::digest(
            super::super::bundle::serialize(&bundle).unwrap(),
        ));
        let contract = SelectionReuseContractV1 {
            schema: SELECTION_REUSE_CONTRACT_SCHEMA_V1.to_string(),
            project_name: "fixture".to_string(),
            bundle_sha256,
            target: "main".to_string(),
            ordered_alternatives: vec!["reference".to_string(), "fast".to_string()],
            evidence_policy: RoutePolicy::BenchmarkValidateAndSelect.token(),
            equivalence_contract: VALIDATED_SELECTION_EQUIVALENCE_V1.to_string(),
            selection_rule: VALIDATED_SELECTION_RULE_V1.to_string(),
            effect_boundary: SELECTION_REUSE_EFFECT_BOUNDARY_V1.to_string(),
            reference_route_id: "reference".to_string(),
            selected_route_id: "fast".to_string(),
            expected_declared_output_sha256: "2".repeat(64),
            benchmark_hgraph_sha256: "3".repeat(64),
            benchmark_deployment_sha256: "4".repeat(64),
            route_declaration_sha256: Vec::new(),
        };
        let result = OExecutionResult {
            route_id: "fast".to_string(),
            exit_code: Some(1),
            stdout: Vec::new(),
            stdout_capture: OutputCapture::complete(&[]),
            stderr: Vec::new(),
            stderr_capture: OutputCapture::complete(&[]),
            value: None,
            artifacts: Vec::new(),
            artifact_requirements: Vec::new(),
            artifact_capture: ArtifactCaptureStatus::Complete,
            disposition: RouteExecutionDisposition::Executed,
            duration_ns: 1,
            provenance: ExecutionProvenance {
                workspace: std::path::PathBuf::from("test-workspace"),
                command: vec!["test-command".to_string()],
                cwd: std::path::PathBuf::from("test-workspace"),
            },
        };
        let checked = check_selection_reuse_output(&bundle, &contract, &result);
        assert_eq!(checked.status, SelectionReuseOutputStatusV1::RouteFailed);
        assert!(checked.observed_declared_output_sha256.is_none());
        checked.validate().unwrap();
    }
}
