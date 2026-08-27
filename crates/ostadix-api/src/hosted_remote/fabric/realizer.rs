//! Provider-side realization of the single Fabric V1 trusted-inline profile.
//!
//! The transport/provider must authenticate the channel, issuer, lease, and
//! target before calling this module.  The realizer nevertheless rebuilds the
//! exact source semantics and implementation identity itself.  Its output is
//! still only an M2 provisional candidate; it never publishes into HGraph or
//! converts a renderer failure into an O-language failure.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::backend_catalog::{BackendRegistry, SpliceRenderer};
use crate::environment::EnvironmentRefV2;
use crate::eval_core::render_with;
use crate::evidence::ExecutionIntentV1;
use crate::execution_contract::Policy;
use crate::execution_fabric::{
    encode_execution_candidate_v1, CandidateOutcomeV1, CandidateOutputV1, ExecutionCandidateV1,
    ExecutionCapsuleV1, OutputValueKindV1, RendererPartV1, Sha256DigestV1, SourceClosedRendererV1,
    TrustedInlineRendererV1,
};
use crate::execution_fabric_authority::{
    FabricSubmissionV1, FABRIC_SOURCE_CLOSURE_DIALECT_V1, FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1,
};
use crate::hgraph::solve::solve_types;
use crate::ir::{OIr, OIrProgram, PlanNodeId};
use crate::parser::Parser;
use crate::value::{OText, OValue};
use crate::world::{PortableOValue, PortableValueRecord, MAX_OVALUE_TEXT_BYTES};

use super::profile::{trusted_inline_fabric_profile_v1, TrustedInlineFabricProfileErrorV1};

#[derive(Debug, Error)]
pub(crate) enum TrustedInlineRealizerErrorV1 {
    #[error("trusted-inline submission is invalid: {0}")]
    InvalidSubmission(String),
    #[error("trusted-inline source closure is invalid: {0}")]
    InvalidSource(String),
    #[error("trusted-inline source or implementation binding mismatch: {0}")]
    BindingMismatch(String),
    #[error("unsupported trusted-inline realization profile: {0}")]
    UnsupportedProfile(String),
    #[error("trusted-inline implementation identity failed: {0}")]
    ImplementationIdentity(String),
    #[error("trusted-inline portable input is invalid: {0}")]
    InvalidInput(String),
    #[error("trusted-inline output contract failed: {0}")]
    OutputContract(String),
    #[error("trusted-inline clock failed: {0}")]
    Clock(String),
    #[error("trusted-inline runtime bound failed: {0}")]
    Runtime(String),
    #[error("trusted-inline candidate construction failed: {0}")]
    Candidate(String),
}

/// Independently reconstructed execution material that has not yet been
/// granted node-local execution authority.
///
/// Fields are private and this type is deliberately not `Clone`: the provider
/// must derive its durable acceptance binding from
/// [`PreparedTrustedInlineAttemptV1::submission`], then move the same prepared
/// value into [`TrustedInlineRealizerV1::realize`].
/// Merely preparing this value does not consume the lease nonce or authorize
/// execution.
#[derive(Debug)]
pub(crate) struct PreparedTrustedInlineAttemptV1 {
    submission: FabricSubmissionV1,
    capsule: ExecutionCapsuleV1,
    verified: VerifiedRendererV1,
}

impl PreparedTrustedInlineAttemptV1 {
    /// The exact immutable submission whose source and implementation were
    /// independently reconstructed.  Provider authentication, final lease
    /// freshness, and durable acceptance must all use this retained value.
    pub(crate) fn submission(&self) -> &FabricSubmissionV1 {
        &self.submission
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TrustedInlineRealizerV1;

impl TrustedInlineRealizerV1 {
    pub(crate) const fn new() -> Self {
        Self
    }

    /// Independently parse, lower, digest-match, and bind one authenticated
    /// submission without converting portable values into live `OValue`s or
    /// beginning execution.
    ///
    /// The provider must revalidate the retained signed lease, atomically
    /// consume its nonce, and durably record `Accepted` after this returns and
    /// before passing the prepared value to [`Self::realize`].
    pub(crate) fn prepare(
        &self,
        submission: &FabricSubmissionV1,
    ) -> Result<PreparedTrustedInlineAttemptV1, TrustedInlineRealizerErrorV1> {
        submission
            .validate()
            .map_err(|error| TrustedInlineRealizerErrorV1::InvalidSubmission(error.to_string()))?;
        let capsule = submission
            .decoded_capsule()
            .map_err(|error| TrustedInlineRealizerErrorV1::InvalidSubmission(error.to_string()))?;
        let verified = verify_source_and_implementation(submission, &capsule)?;
        Ok(PreparedTrustedInlineAttemptV1 {
            submission: submission.clone(),
            capsule,
            verified,
        })
    }

    /// Execute one durably accepted prepared attempt and return canonical
    /// provisional-candidate bytes.  Every error is an
    /// infrastructure/authority abort; this path never emits
    /// `CandidateOutcomeV1::Failed`.
    ///
    /// The local monotonic clock enforces only the signed maximum-runtime
    /// budget.  The wall-clock completion value is truthful evidence, not proof
    /// of coordinator-observed timeliness.
    pub(crate) fn realize(
        &self,
        prepared: PreparedTrustedInlineAttemptV1,
    ) -> Result<Vec<u8>, TrustedInlineRealizerErrorV1> {
        let PreparedTrustedInlineAttemptV1 {
            submission: _,
            capsule,
            verified,
        } = prepared;
        let started = Instant::now();
        let output = execute_renderer(&capsule, &verified)?;
        let elapsed = started.elapsed();
        let maximum_runtime = Duration::from_millis(capsule.limits().max_runtime_ms());
        if elapsed > maximum_runtime {
            return Err(TrustedInlineRealizerErrorV1::Runtime(format!(
                "elapsed {elapsed:?} exceeds capsule maximum {maximum_runtime:?}"
            )));
        }

        let completed_wall = unix_time_now()?;
        let completed_unix_ms = unix_millis(completed_wall)?;
        if completed_unix_ms == 0 {
            return Err(TrustedInlineRealizerErrorV1::Clock(
                "candidate completion time must be nonzero".to_string(),
            ));
        }
        let (utf8, value_kind) = match output {
            OValue::Text { v } => (v.utf8, OutputValueKindV1::Text),
            OValue::Html { v } => (v, OutputValueKindV1::Html),
            other => {
                return Err(TrustedInlineRealizerErrorV1::OutputContract(format!(
                    "renderer produced unsupported output {}",
                    other.type_name()
                )))
            }
        };
        if value_kind != capsule.output().value_kind() {
            return Err(TrustedInlineRealizerErrorV1::OutputContract(
                "renderer output kind disagrees with the frozen contract".to_string(),
            ));
        }
        let portable = PortableValueRecord::Core(
            PortableOValue::text(OText {
                utf8,
                encoding: Some("utf-8".to_string()),
            })
            .map_err(|error| TrustedInlineRealizerErrorV1::OutputContract(error.to_string()))?,
        );
        let candidate_output = CandidateOutputV1::new(
            capsule.output().slot(),
            &portable,
            value_kind,
            capsule.output().fidelity(),
        )
        .map_err(|error| TrustedInlineRealizerErrorV1::OutputContract(error.to_string()))?;
        let candidate = ExecutionCandidateV1::new(
            &capsule,
            CandidateOutcomeV1::Succeeded {
                output: candidate_output,
            },
            completed_unix_ms,
        )
        .map_err(|error| TrustedInlineRealizerErrorV1::Candidate(error.to_string()))?;
        encode_execution_candidate_v1(&candidate)
            .map_err(|error| TrustedInlineRealizerErrorV1::Candidate(error.to_string()))
    }
}

#[derive(Debug)]
struct VerifiedRendererV1 {
    renderer: TrustedInlineRendererV1,
    splice_renderer: SpliceRenderer,
    parts: Vec<RendererPartV1>,
}

fn verify_source_and_implementation(
    submission: &FabricSubmissionV1,
    capsule: &ExecutionCapsuleV1,
) -> Result<VerifiedRendererV1, TrustedInlineRealizerErrorV1> {
    let closure = submission.header().source_closure();
    if closure.dialect() != FABRIC_SOURCE_CLOSURE_DIALECT_V1 {
        return Err(TrustedInlineRealizerErrorV1::InvalidSource(format!(
            "dialect must be exactly {FABRIC_SOURCE_CLOSURE_DIALECT_V1}"
        )));
    }
    if closure.root_operation() != FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1 {
        return Err(TrustedInlineRealizerErrorV1::InvalidSource(format!(
            "root operation must be exactly {FABRIC_SOURCE_CLOSURE_ROOT_OPERATION_V1}"
        )));
    }
    let base_policy = Policy::from_name(closure.base_policy()).ok_or_else(|| {
        TrustedInlineRealizerErrorV1::InvalidSource(format!(
            "unsupported base policy {}",
            closure.base_policy()
        ))
    })?;

    let registry = BackendRegistry::global();
    let tags = registry.registered_backend_tags();
    let mut parser = Parser::new(closure.source_utf8(), &tags);
    let parsed = parser.parse_with_origins().map_err(|error| {
        TrustedInlineRealizerErrorV1::InvalidSource(format!("source parse failed: {error:#}"))
    })?;
    if parsed.source_len() != closure.source_utf8().len()
        || parsed.source_sha256() != closure.source_sha256()
    {
        return Err(TrustedInlineRealizerErrorV1::BindingMismatch(
            "parser source identity does not match the retained source closure".to_string(),
        ));
    }

    let program = OIrProgram::lower(parsed.nodes());
    if program.nodes.len() != 1 {
        return Err(TrustedInlineRealizerErrorV1::UnsupportedProfile(
            "source closure must lower to exactly one top-level operation".to_string(),
        ));
    }
    let OIr::Exec {
        lang,
        env_id,
        attr,
        backend,
        body,
    } = &program.nodes[0]
    else {
        return Err(TrustedInlineRealizerErrorV1::UnsupportedProfile(
            "source closure root must be one Exec operation".to_string(),
        ));
    };
    if lang != &backend.canonical {
        return Err(TrustedInlineRealizerErrorV1::UnsupportedProfile(
            "lowered backend tag is not its canonical catalog name".to_string(),
        ));
    }
    if !EnvironmentRefV2::from_encoded(*env_id).is_fresh() {
        return Err(TrustedInlineRealizerErrorV1::UnsupportedProfile(
            "trusted-inline realization requires a fresh environment".to_string(),
        ));
    }
    if attr.is_some() {
        return Err(TrustedInlineRealizerErrorV1::UnsupportedProfile(
            "trusted-inline realization does not admit block attributes".to_string(),
        ));
    }
    let profile = trusted_inline_fabric_profile_v1(backend).map_err(realizer_profile_error)?;
    let renderer = profile.renderer();
    let splice_renderer = profile.splice_renderer();

    // A validated M2 region already carries the frozen part-count and literal
    // bounds. Match its exact role/order/content before allocating a plan or
    // HGraph so a bounded source fragment cannot defer those limits until after
    // expensive reconstruction.
    let retained_parts = capsule.region().parts();
    if body.len() != retained_parts.len() {
        return Err(TrustedInlineRealizerErrorV1::BindingMismatch(
            "lowered lexical renderer part count differs from the capsule region".to_string(),
        ));
    }
    let mut parts = Vec::with_capacity(retained_parts.len());
    for (child, retained) in body.iter().zip(retained_parts) {
        let reconstructed = match child {
            OIr::Text(utf8) => RendererPartV1::literal(utf8.clone()),
            // The parser lowers `$slot` to OIR's lexical `Load` form.  This is
            // only a source placeholder for the frozen spliced-input role; the
            // provider never executes it as a scope lookup.
            OIr::Load(slot) => RendererPartV1::input(slot.clone()),
            _ => {
                return Err(TrustedInlineRealizerErrorV1::UnsupportedProfile(
                    "trusted-inline Exec body may contain only direct literal text and lexical input placeholders"
                        .to_string(),
                ))
            }
        };
        if &reconstructed != retained {
            return Err(TrustedInlineRealizerErrorV1::BindingMismatch(
                "lowered lexical renderer role/order/content differs from the capsule region"
                    .to_string(),
            ));
        }
        parts.push(reconstructed);
    }

    let plan = program.plan();
    if plan.roots.len() != 1 || plan.roots[0] != PlanNodeId(0) {
        return Err(TrustedInlineRealizerErrorV1::UnsupportedProfile(
            "trusted-inline plan must have the single root P0".to_string(),
        ));
    }
    let expected_nodes = body.len().checked_add(1).ok_or_else(|| {
        TrustedInlineRealizerErrorV1::InvalidSource("plan node count overflowed".to_string())
    })?;
    if plan.nodes.len() != expected_nodes || program.flatten_for_plan().len() != expected_nodes {
        return Err(TrustedInlineRealizerErrorV1::UnsupportedProfile(
            "trusted-inline plan is not the flat Exec plus direct literal/input body".to_string(),
        ));
    }

    let mut graph = program.hgraph_for_plan(&plan).map_err(|error| {
        TrustedInlineRealizerErrorV1::InvalidSource(format!(
            "source HGraph projection failed: {error}"
        ))
    })?;
    solve_types(&mut graph).map_err(|error| {
        TrustedInlineRealizerErrorV1::InvalidSource(format!("source HGraph solve failed: {error}"))
    })?;
    let intent = ExecutionIntentV1::compile(
        closure.source_utf8().as_bytes(),
        &program,
        &plan,
        &graph,
        base_policy,
    )
    .map_err(|error| {
        TrustedInlineRealizerErrorV1::InvalidSource(format!(
            "execution-intent reconstruction failed: {error:#}"
        ))
    })?;

    let intent_source = sha256_from_hex("intent source", &intent.source_sha256)?;
    require_digest("source", &intent_source, closure.source_sha256())?;
    let intent_sha256 = sha256_from_hex("execution intent", &intent.execution_intent_sha256)?;
    require_digest("execution intent", &intent_sha256, closure.intent_sha256())?;
    // V1 admits exactly one root operation, so the whole-program OIR digest is
    // also the exact retained root-operation digest.
    let oir_sha256 = sha256_from_hex("operation OIR", &intent.oir_sha256)?;
    require_digest("operation OIR", &oir_sha256, closure.operation_oir_sha256())?;
    require_digest(
        "capsule operation OIR",
        &oir_sha256,
        capsule.region().expected_oir_sha256(),
    )?;
    let plan_sha256 = sha256_from_hex("execution plan", &intent.plan_sha256)?;
    require_digest(
        "execution plan",
        &plan_sha256,
        closure.execution_plan_sha256(),
    )?;
    require_digest(
        "capsule execution plan",
        &plan_sha256,
        capsule.region().expected_plan_sha256(),
    )?;

    // The M2 field binds exactly the plan-referenced catalog projection.  A
    // whole-catalog digest is intentionally not an interchangeable authority.
    let catalog_projection = sha256_from_hex(
        "backend catalog projection",
        &intent.backend_catalog_projection_sha256,
    )?;
    let whole_catalog = sha256_from_hex("whole backend catalog", &registry.catalog_sha256())?;
    if capsule.region().backend_catalog_sha256() == &whole_catalog {
        return Err(TrustedInlineRealizerErrorV1::BindingMismatch(
            "whole-catalog digest is forbidden; Fabric V1 requires the plan-referenced catalog projection"
                .to_string(),
        ));
    }
    require_digest(
        "backend catalog projection",
        &catalog_projection,
        capsule.region().backend_catalog_sha256(),
    )?;

    require_digest(
        "backend implementation",
        profile.implementation_sha256(),
        capsule.region().backend_implementation_sha256(),
    )?;
    let target_pipeline = submission
        .header()
        .lease()
        .lease()
        .target()
        .realization_pipeline_sha256();
    if profile.realization_pipeline_sha256() != target_pipeline {
        return Err(TrustedInlineRealizerErrorV1::BindingMismatch(format!(
            "realization pipeline mismatch: lease={}, recomputed={}",
            target_pipeline.as_sha256(),
            profile.realization_pipeline_sha256().as_sha256()
        )));
    }

    let reconstructed_region = SourceClosedRendererV1::new(
        renderer,
        parts.clone(),
        oir_sha256,
        plan_sha256,
        catalog_projection,
        *profile.implementation_sha256(),
    )
    .map_err(|error| {
        TrustedInlineRealizerErrorV1::InvalidSource(format!(
            "renderer-region reconstruction failed: {error}"
        ))
    })?;
    if &reconstructed_region != capsule.region() {
        return Err(TrustedInlineRealizerErrorV1::BindingMismatch(
            "reconstructed source-closed renderer does not equal the capsule region".to_string(),
        ));
    }

    Ok(VerifiedRendererV1 {
        renderer,
        splice_renderer,
        parts,
    })
}

fn realizer_profile_error(
    error: TrustedInlineFabricProfileErrorV1,
) -> TrustedInlineRealizerErrorV1 {
    match error {
        TrustedInlineFabricProfileErrorV1::UnsupportedProfile(message) => {
            TrustedInlineRealizerErrorV1::UnsupportedProfile(message)
        }
        TrustedInlineFabricProfileErrorV1::ImplementationIdentity(message) => {
            TrustedInlineRealizerErrorV1::ImplementationIdentity(message)
        }
    }
}

fn execute_renderer(
    capsule: &ExecutionCapsuleV1,
    verified: &VerifiedRendererV1,
) -> Result<OValue, TrustedInlineRealizerErrorV1> {
    let maximum_rendered_bytes =
        usize::min(capsule.output().max_bytes() as usize, MAX_OVALUE_TEXT_BYTES);
    let mut rendered = String::new();
    for part in &verified.parts {
        match part {
            RendererPartV1::Literal { utf8 } => {
                push_bounded(&mut rendered, utf8, maximum_rendered_bytes)?;
            }
            RendererPartV1::Input { slot } => {
                let binding = capsule.inputs().binding(slot).ok_or_else(|| {
                    TrustedInlineRealizerErrorV1::InvalidInput(format!(
                        "validated capsule omitted renderer input slot {slot}"
                    ))
                })?;
                let record = binding.value().decode().map_err(|error| {
                    TrustedInlineRealizerErrorV1::InvalidInput(error.to_string())
                })?;
                let PortableValueRecord::Core(portable) = record else {
                    return Err(TrustedInlineRealizerErrorV1::InvalidInput(
                        "portable extensions are not admitted by the renderer".to_string(),
                    ));
                };
                let value = lower_renderer_value(portable)?;
                let fragment = render_with(verified.splice_renderer, &value);
                push_bounded(&mut rendered, &fragment, maximum_rendered_bytes)?;
            }
        }
    }
    Ok(match verified.renderer {
        TrustedInlineRendererV1::Html => OValue::html(rendered),
        TrustedInlineRendererV1::Markdown
        | TrustedInlineRendererV1::Latex
        | TrustedInlineRendererV1::Text => OValue::str_(rendered),
    })
}

fn push_bounded(
    output: &mut String,
    fragment: &str,
    maximum: usize,
) -> Result<(), TrustedInlineRealizerErrorV1> {
    let next = output.len().checked_add(fragment.len()).ok_or_else(|| {
        TrustedInlineRealizerErrorV1::OutputContract(
            "rendered output byte count overflowed".to_string(),
        )
    })?;
    if next > maximum {
        return Err(TrustedInlineRealizerErrorV1::OutputContract(format!(
            "rendered UTF-8 exceeds the safe pre-encoding bound of {maximum} bytes"
        )));
    }
    output.push_str(fragment);
    Ok(())
}

// This is deliberately private and renderer-specific.  It cannot be used as
// a generic wire-to-live conversion or to resolve identities into authority.
fn lower_renderer_value(value: PortableOValue) -> Result<OValue, TrustedInlineRealizerErrorV1> {
    Ok(match value {
        PortableOValue::Null => OValue::Null,
        PortableOValue::Bool(v) => OValue::Bool { v },
        PortableOValue::Number(v) => OValue::Number { v },
        PortableOValue::Text(v) => OValue::Text { v },
        PortableOValue::Char(scalar) => OValue::Char { scalar },
        PortableOValue::List(values) => OValue::List {
            v: values
                .into_iter()
                .map(lower_renderer_value)
                .collect::<Result<Vec<_>, _>>()?,
        },
        PortableOValue::Record(fields) => {
            let mut output = BTreeMap::new();
            for (key, value) in fields {
                if output.insert(key, lower_renderer_value(value)?).is_some() {
                    return Err(TrustedInlineRealizerErrorV1::InvalidInput(
                        "portable record repeated a key after canonical decode".to_string(),
                    ));
                }
            }
            OValue::Object { fields: output }
        }
        PortableOValue::Map(entries) => OValue::EntriesMap {
            entries: entries
                .into_iter()
                .map(|(key, value)| Ok((lower_renderer_value(key)?, lower_renderer_value(value)?)))
                .collect::<Result<Vec<_>, TrustedInlineRealizerErrorV1>>()?,
        },
        PortableOValue::Bytes(_)
        | PortableOValue::Tagged(_)
        | PortableOValue::CodeRef(_)
        | PortableOValue::ObjectRef(_)
        | PortableOValue::Error(_) => {
            return Err(TrustedInlineRealizerErrorV1::InvalidInput(
                "portable value kind is outside the trusted renderer allowlist".to_string(),
            ))
        }
    })
}

fn sha256_from_hex(
    label: &str,
    value: &str,
) -> Result<Sha256DigestV1, TrustedInlineRealizerErrorV1> {
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(value, &mut digest).map_err(|error| {
        TrustedInlineRealizerErrorV1::BindingMismatch(format!(
            "{label} is not lowercase SHA-256: {error}"
        ))
    })?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TrustedInlineRealizerErrorV1::BindingMismatch(format!(
            "{label} is not canonical lowercase SHA-256"
        )));
    }
    Ok(digest)
}

fn require_digest(
    label: &str,
    recomputed: &Sha256DigestV1,
    retained: &Sha256DigestV1,
) -> Result<(), TrustedInlineRealizerErrorV1> {
    if recomputed != retained {
        return Err(TrustedInlineRealizerErrorV1::BindingMismatch(format!(
            "{label}: retained={}, recomputed={}",
            hex::encode(retained),
            hex::encode(recomputed)
        )));
    }
    Ok(())
}

fn unix_time_now() -> Result<Duration, TrustedInlineRealizerErrorV1> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| TrustedInlineRealizerErrorV1::Clock(error.to_string()))
}

fn unix_millis(time: Duration) -> Result<u64, TrustedInlineRealizerErrorV1> {
    u64::try_from(time.as_millis()).map_err(|_| {
        TrustedInlineRealizerErrorV1::Clock(
            "wall-clock milliseconds exceed the V1 u64 range".to_string(),
        )
    })
}
