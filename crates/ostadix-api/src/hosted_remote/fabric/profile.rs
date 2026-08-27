//! Immutable identity for the single Fabric V1 trusted-inline profile.
//!
//! Both coordinator-side capsule construction and provider-side realization
//! must obtain renderer semantics and implementation identity through this
//! sealed module. Keeping those facts together prevents either side from
//! independently describing a profile that merely appears equivalent.

use std::sync::OnceLock;

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::backend_catalog::{
    backend_executable_set_v2, BackendAdapterKind, BackendExecutableSetRowV2, BackendInterface,
    BackendRegistry, ExecutionMode, SpliceRenderer, BACKEND_EXECUTABLE_SET_DIGEST_DOMAIN_V2,
};
use crate::execution_fabric::{Sha256DigestV1, TrustedInlineRendererV1};
use crate::execution_fabric_authority::FABRIC_SOURCE_CLOSURE_DIALECT_V1;
use crate::placement_protocol::{
    BackendImplementationIdV1, BackendStateSupportV2, CanonicalPlacementRecordV1, SemanticDigestV1,
};
use crate::resource_identity::ArtifactId;

const TRUSTED_INLINE_PROTOCOL_ABI_V1: &str = "ostadix-trusted-inline-realizer/v1";
const TRUSTED_INLINE_REALIZATION_SCHEMA_V1: &str = "ostadix.trusted-inline-realization/v1";
const TRUSTED_INLINE_REALIZATION_DIGEST_DOMAIN_V1: &str =
    "ostadix/execution-fabric/trusted-inline-realization/v1";
const TRUSTED_INLINE_SOURCE_SET_SCHEMA_V1: &str = "ostadix.trusted-inline-adapter-source-set/v1";

// This manifest is intentionally crate-local. Generated/AOT runtimes copy
// these same source bytes, while workspace-parent Cargo files are not stable
// members of such a package. Paths and bytes are length-framed below, so a
// path substitution, concatenation ambiguity, or dependency edit changes the
// adapter artifact identity. Both files defining this extracted profile are
// explicit members of the identity.
const TRUSTED_INLINE_SOURCE_SET_V1: &[(&str, &[u8])] = &[
    (
        "src/backend_catalog.inc.rs",
        include_bytes!("../../backend_catalog.inc.rs") as &[u8],
    ),
    (
        "src/backend_catalog.rs",
        include_bytes!("../../backend_catalog.rs") as &[u8],
    ),
    (
        "src/backend_morphism.rs",
        include_bytes!("../../backend_morphism.rs") as &[u8],
    ),
    (
        "src/canonical_cbor.rs",
        include_bytes!("../../canonical_cbor.rs") as &[u8],
    ),
    (
        "src/dispatch_model.rs",
        include_bytes!("../../dispatch_model.rs") as &[u8],
    ),
    (
        "src/effects.rs",
        include_bytes!("../../effects.rs") as &[u8],
    ),
    (
        "src/environment.rs",
        include_bytes!("../../environment.rs") as &[u8],
    ),
    (
        "src/eval_core.rs",
        include_bytes!("../../eval_core.rs") as &[u8],
    ),
    (
        "src/evidence/analyze.rs",
        include_bytes!("../../evidence/analyze.rs") as &[u8],
    ),
    (
        "src/evidence/intent.rs",
        include_bytes!("../../evidence/intent.rs") as &[u8],
    ),
    (
        "src/execution_contract.rs",
        include_bytes!("../../execution_contract.rs") as &[u8],
    ),
    (
        "src/execution_fabric/codec.rs",
        include_bytes!("../../execution_fabric/codec.rs") as &[u8],
    ),
    (
        "src/execution_fabric/protocol.rs",
        include_bytes!("../../execution_fabric/protocol.rs") as &[u8],
    ),
    (
        "src/execution_fabric_authority/protocol.rs",
        include_bytes!("../../execution_fabric_authority/protocol.rs") as &[u8],
    ),
    (
        "src/hgraph/from_oir.rs",
        include_bytes!("../../hgraph/from_oir.rs") as &[u8],
    ),
    (
        "src/hgraph/graph.rs",
        include_bytes!("../../hgraph/graph.rs") as &[u8],
    ),
    (
        "src/hgraph/kinds.rs",
        include_bytes!("../../hgraph/kinds.rs") as &[u8],
    ),
    (
        "src/hgraph/solve.rs",
        include_bytes!("../../hgraph/solve.rs") as &[u8],
    ),
    ("src/ir.rs", include_bytes!("../../ir.rs") as &[u8]),
    ("src/parser.rs", include_bytes!("../../parser.rs") as &[u8]),
    (
        "src/placement/protocol/digest.rs",
        include_bytes!("../../placement/protocol/digest.rs") as &[u8],
    ),
    (
        "src/placement/protocol/error.rs",
        include_bytes!("../../placement/protocol/error.rs") as &[u8],
    ),
    (
        "src/placement/protocol/state.rs",
        include_bytes!("../../placement/protocol/state.rs") as &[u8],
    ),
    (
        "src/placement/protocol/target.rs",
        include_bytes!("../../placement/protocol/target.rs") as &[u8],
    ),
    (
        "src/syntax_dialect.rs",
        include_bytes!("../../syntax_dialect.rs") as &[u8],
    ),
    ("src/value.rs", include_bytes!("../../value.rs") as &[u8]),
    (
        "src/world/identity.rs",
        include_bytes!("../../world/identity.rs") as &[u8],
    ),
    (
        "src/world/identity_wire.rs",
        include_bytes!("../../world/identity_wire.rs") as &[u8],
    ),
    (
        "src/world/value.rs",
        include_bytes!("../../world/value.rs") as &[u8],
    ),
    (
        "src/world/value_codec.rs",
        include_bytes!("../../world/value_codec.rs") as &[u8],
    ),
    (
        "src/hosted_remote/fabric/profile.rs",
        include_bytes!("profile.rs") as &[u8],
    ),
    (
        "src/hosted_remote/fabric/realizer.rs",
        include_bytes!("realizer.rs") as &[u8],
    ),
];

#[derive(Debug, Error)]
pub(crate) enum TrustedInlineFabricProfileErrorV1 {
    #[error("unsupported trusted-inline realization profile: {0}")]
    UnsupportedProfile(String),
    #[error("trusted-inline implementation identity failed: {0}")]
    ImplementationIdentity(String),
}

/// One validated, immutable description of a current trusted-inline backend.
///
/// Fields remain private so consumers cannot mix a renderer from one catalog
/// entry with an implementation identity from another.
#[derive(Clone, Debug)]
pub(crate) struct TrustedInlineFabricProfileV1 {
    renderer: TrustedInlineRendererV1,
    splice_renderer: SpliceRenderer,
    implementation: BackendImplementationIdV1,
    implementation_sha256: Sha256DigestV1,
}

impl TrustedInlineFabricProfileV1 {
    pub(crate) const fn renderer(&self) -> TrustedInlineRendererV1 {
        self.renderer
    }

    pub(crate) const fn splice_renderer(&self) -> SpliceRenderer {
        self.splice_renderer
    }

    pub(crate) const fn implementation_sha256(&self) -> &Sha256DigestV1 {
        &self.implementation_sha256
    }

    pub(crate) fn realization_pipeline_sha256(&self) -> &SemanticDigestV1 {
        self.implementation.realization_pipeline()
    }
}

/// Resolve and validate exactly one of the four current deterministic,
/// stateless inline renderers, then derive its complete implementation
/// identity through the single shared source-set path.
pub(crate) fn trusted_inline_fabric_profile_v1(
    backend: &BackendInterface,
) -> Result<TrustedInlineFabricProfileV1, TrustedInlineFabricProfileErrorV1> {
    let registry = BackendRegistry::global();
    let (renderer, splice_renderer) = match backend.canonical.as_str() {
        "html" => (TrustedInlineRendererV1::Html, SpliceRenderer::Html),
        "markdown" => (TrustedInlineRendererV1::Markdown, SpliceRenderer::Markdown),
        "latex" => (TrustedInlineRendererV1::Latex, SpliceRenderer::Latex),
        "text" => (TrustedInlineRendererV1::Text, SpliceRenderer::Default),
        other => {
            return Err(TrustedInlineFabricProfileErrorV1::UnsupportedProfile(
                format!("backend {other} is outside the four trusted inline renderers"),
            ));
        }
    };
    let current = registry.interface_for(&backend.canonical);
    let runtime = registry.runtime_requirements_for(&backend.canonical);
    if backend != &current
        || registry.adapter_for(&backend.canonical) != BackendAdapterKind::Inline
        || !backend.pure
        || backend.execution != ExecutionMode::InlineValue
        || backend.renderer != splice_renderer
        || !backend.required_authorities.is_empty()
        || backend.state_support.as_ref() != Some(&BackendStateSupportV2::Stateless)
        || runtime.key != "builtin"
        || !runtime.builtin
        || !runtime.alternatives.is_empty()
    {
        return Err(TrustedInlineFabricProfileErrorV1::UnsupportedProfile(
            format!(
                "backend {} does not equal the current deterministic stateless inline profile",
                backend.canonical
            ),
        ));
    }

    let backend_specification = SemanticDigestV1::from_sha256(
        backend
            .specification_sha256
            .as_deref()
            .ok_or_else(|| {
                TrustedInlineFabricProfileErrorV1::ImplementationIdentity(
                    "trusted-inline backend has no current specification digest".to_string(),
                )
            })?
            .to_string(),
    )
    .map_err(|error| {
        TrustedInlineFabricProfileErrorV1::ImplementationIdentity(error.to_string())
    })?;
    let adapter_artifact = trusted_inline_adapter_artifact_v1()?;
    let executable_set = backend_executable_set_v2(Vec::<BackendExecutableSetRowV2>::new())
        .map_err(|error| {
            TrustedInlineFabricProfileErrorV1::ImplementationIdentity(error.to_string())
        })?;
    let realization_material = TrustedInlineRealizationMaterialV1 {
        schema: TRUSTED_INLINE_REALIZATION_SCHEMA_V1,
        backend_specification: backend_specification.as_sha256(),
        adapter_kind: BackendAdapterKind::Inline.name(),
        adapter_artifact: adapter_artifact.as_sha256(),
        executable_set_schema: BACKEND_EXECUTABLE_SET_DIGEST_DOMAIN_V2,
        executable_set: executable_set.as_sha256(),
        protocol: TRUSTED_INLINE_PROTOCOL_ABI_V1,
        source_dialect: FABRIC_SOURCE_CLOSURE_DIALECT_V1,
        source_profile: "single-fresh-exec-direct-literal-input/v1",
        renderer: trusted_renderer_name(renderer),
        splice_renderer: splice_renderer_name(splice_renderer),
        input_wire: "owvalue-v1-renderer-core",
        output_wire: "owvalue-v1-core-text",
        render_entrypoint: "eval-core-render-with/v1",
    };
    let realization_bytes = serde_json::to_vec(&realization_material).map_err(|error| {
        TrustedInlineFabricProfileErrorV1::ImplementationIdentity(error.to_string())
    })?;
    let realization_pipeline = SemanticDigestV1::hash_bytes(
        TRUSTED_INLINE_REALIZATION_DIGEST_DOMAIN_V1,
        &realization_bytes,
    );
    let implementation = BackendImplementationIdV1::new(
        backend_specification,
        adapter_artifact,
        executable_set,
        TRUSTED_INLINE_PROTOCOL_ABI_V1,
        realization_pipeline,
    )
    .map_err(|error| {
        TrustedInlineFabricProfileErrorV1::ImplementationIdentity(error.to_string())
    })?;
    let implementation_semantic = implementation.semantic_digest().map_err(|error| {
        TrustedInlineFabricProfileErrorV1::ImplementationIdentity(error.to_string())
    })?;
    let implementation_sha256 = decode_sha256(implementation_semantic.as_sha256())?;

    Ok(TrustedInlineFabricProfileV1 {
        renderer,
        splice_renderer,
        implementation,
        implementation_sha256,
    })
}

/// Return the canonical realization-pipeline identity for one current
/// deterministic trusted-inline backend.
///
/// This is a read-only configuration surface for explicit Fabric targets. It
/// does not admit work, construct a renderer command, grant execution
/// authority, or expose the sealed implementation profile.
pub fn trusted_inline_fabric_realization_pipeline_sha256_v1(
    backend: &str,
) -> anyhow::Result<SemanticDigestV1> {
    let interface = BackendRegistry::global().interface_for(backend);
    let profile = trusted_inline_fabric_profile_v1(&interface).map_err(anyhow::Error::new)?;
    Ok(profile.realization_pipeline_sha256().clone())
}

#[derive(Serialize)]
struct TrustedInlineRealizationMaterialV1<'a> {
    schema: &'static str,
    backend_specification: &'a str,
    adapter_kind: &'static str,
    adapter_artifact: &'a str,
    executable_set_schema: &'static str,
    executable_set: &'a str,
    protocol: &'static str,
    source_dialect: &'static str,
    source_profile: &'static str,
    renderer: &'static str,
    splice_renderer: &'static str,
    input_wire: &'static str,
    output_wire: &'static str,
    render_entrypoint: &'static str,
}

fn trusted_inline_adapter_artifact_v1() -> Result<ArtifactId, TrustedInlineFabricProfileErrorV1> {
    static SOURCE_SET_SHA256: OnceLock<String> = OnceLock::new();
    let sha256 = SOURCE_SET_SHA256.get_or_init(|| {
        let mut hash = Sha256::new();
        hash_source_field(&mut hash, TRUSTED_INLINE_SOURCE_SET_SCHEMA_V1.as_bytes());
        hash.update((TRUSTED_INLINE_SOURCE_SET_V1.len() as u64).to_be_bytes());
        for (path, source) in TRUSTED_INLINE_SOURCE_SET_V1 {
            hash_source_field(&mut hash, path.as_bytes());
            hash_source_field(&mut hash, source);
        }
        hex::encode(hash.finalize())
    });
    ArtifactId::from_sha256(sha256.clone()).map_err(|error| {
        TrustedInlineFabricProfileErrorV1::ImplementationIdentity(error.to_string())
    })
}

fn hash_source_field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

fn trusted_renderer_name(renderer: TrustedInlineRendererV1) -> &'static str {
    match renderer {
        TrustedInlineRendererV1::Html => "html",
        TrustedInlineRendererV1::Markdown => "markdown",
        TrustedInlineRendererV1::Latex => "latex",
        TrustedInlineRendererV1::Text => "text",
    }
}

fn splice_renderer_name(renderer: SpliceRenderer) -> &'static str {
    match renderer {
        SpliceRenderer::Python => "python",
        SpliceRenderer::Html => "html",
        SpliceRenderer::Latex => "latex",
        SpliceRenderer::Markdown => "markdown",
        SpliceRenderer::Nix => "nix",
        SpliceRenderer::Default => "default",
    }
}

fn decode_sha256(value: &str) -> Result<Sha256DigestV1, TrustedInlineFabricProfileErrorV1> {
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(value, &mut digest).map_err(|error| {
        TrustedInlineFabricProfileErrorV1::ImplementationIdentity(error.to_string())
    })?;
    Ok(digest)
}
