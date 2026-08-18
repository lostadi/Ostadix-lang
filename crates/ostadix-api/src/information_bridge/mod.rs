//! Explicit, authority-free projections from native Ostadix records into the
//! Information V1 substrate.
//!
//! This leaf module exposes only the typed metadata records declared here. It
//! does not serialize arbitrary native values, mint Information atoms, retain
//! credentials or capabilities, or provide mutation or dispatch handles.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::canonical_cbor::{decode_bounded, encode, DecodeLimits};
use crate::evidence::{EvidenceBundleV6, ANALYZER_ID_V6, EVIDENCE_SCHEMA_V6};
use crate::hgraph::HGraph;
use crate::hosted_remote::canonical_hosted_bytes;
use crate::hosted_remote::v2::SignedJournalEntryV2;
use crate::information::{InformationErrorV1, NativeRecordRefV1};
use crate::parser::ParsedDocumentV1;
use crate::project::LogicalHGraphV1;
use crate::registry::{VerifiedRegistryProfileV1, MAX_NAMESPACE_BYTES};
use crate::value::{FloatFormat, ONumber, OValue};
use crate::world::{
    project_receipt_semantic_sha256_v1, receipt_v1_sha256, VerifiedExecutionReceiptV1,
};

pub const INFORMATION_BRIDGE_SCHEMA_V1: &str = "ostadix.information-bridge/v1";
pub const INFORMATION_BRIDGE_MEDIA_TYPE_V1: &str = "application/cbor";
pub const MAX_INFORMATION_BRIDGE_RECORD_BYTES_V1: usize = 64 * 1024;
pub const MAX_INFORMATION_BRIDGE_DECODE_ITEMS_V1: usize = 256;
pub const MAX_INFORMATION_BRIDGE_DECODE_DEPTH_V1: usize = 8;
pub const MAX_PUBLIC_VALUE_CANONICAL_BYTES_V1: usize = 8 * 1024;
pub const MAX_PUBLIC_VALUE_TEXT_BYTES_V1: usize = 4 * 1024;
pub const MAX_PUBLIC_VALUE_IDENTIFIER_BYTES_V1: usize = 256;
pub const MAX_PUBLIC_VALUE_NUMBER_BYTES_V1: u64 = 256;
pub const MAX_PUBLIC_VALUE_NUMBER_DEPTH_V1: usize = 16;
pub const MAX_PUBLIC_VALUE_NUMBER_NODES_V1: usize = 64;
pub const HGRAPH_METADATA_PROJECTION_DIGEST_DOMAIN_V1: &str =
    "ostadix-information-bridge-hgraph-metadata-projection/v1";
pub const EVIDENCE_METADATA_PROJECTION_DIGEST_DOMAIN_V1: &str =
    "ostadix-information-bridge-evidence-metadata-projection/v1";
pub const REGISTRY_NODE_IDENTITY_DIGEST_DOMAIN_V1: &str =
    "ostadix-information-bridge-registry-node-identity/v1";
pub const HOSTED_SESSION_IDENTITY_DIGEST_DOMAIN_V1: &str =
    "ostadix-information-bridge-hosted-session-identity/v1";
pub const HOSTED_ENTRY_IDENTITY_DIGEST_DOMAIN_V1: &str =
    "ostadix-information-bridge-hosted-entry-identity/v1";

pub const PARSED_DOCUMENT_INFORMATION_SCHEMA_V1: &str = "ostadix.info-bridge-parsed-document/v1";
pub const PUBLIC_VALUE_INFORMATION_SCHEMA_V1: &str = "ostadix.info-bridge-public-value/v1";
pub const HGRAPH_INFORMATION_SCHEMA_V1: &str = "ostadix.info-bridge-hgraph/v1";
pub const EVIDENCE_INFORMATION_SCHEMA_V1: &str = "ostadix.info-bridge-evidence/v1";
pub const REGISTRY_PROFILE_INFORMATION_SCHEMA_V1: &str = "ostadix.info-bridge-registry-profile/v1";
pub const WORLD_RECEIPT_INFORMATION_SCHEMA_V1: &str = "ostadix.info-bridge-world-receipt/v1";
pub const PROJECT_GRAPH_INFORMATION_SCHEMA_V1: &str = "ostadix.info-bridge-project-graph/v1";
pub const HOSTED_JOURNAL_INFORMATION_SCHEMA_V1: &str = "ostadix.info-bridge-hosted-journal/v1";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InformationBridgeErrorV1 {
    #[error("information bridge canonical encoding failed: {0}")]
    Canonical(String),
    #[error("information bridge record uses schema `{actual}`; expected `{expected}`")]
    WrongSchema {
        expected: &'static str,
        actual: String,
    },
    #[error("information bridge input is invalid: {0}")]
    InvalidInput(String),
    #[error("OValue variant `{0}` is not an allowlisted public scalar projection")]
    UnsupportedValue(&'static str),
    #[error(transparent)]
    Information(#[from] InformationErrorV1),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedDocumentInformationV1 {
    pub schema: String,
    pub source_sha256: String,
    pub source_len: u64,
    pub syntax_node_count: u64,
    pub plan_origin_count: u64,
    pub plan_origins_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicValueInformationV1 {
    pub schema: String,
    pub value_kind: String,
    pub canonical_sha256: String,
    pub canonical_len: u64,
    pub caller_declared_public: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HGraphInformationV1 {
    pub schema: String,
    pub metadata_projection_sha256: String,
    pub node_count: u64,
    pub constraint_edge_count: u64,
    pub execution_operation_count: u64,
    pub root_count: u64,
    pub sequence_dependency_count: u64,
    pub admission_evidence_input_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceInformationV1 {
    pub schema: String,
    pub evidence_schema: String,
    pub analyzer: String,
    pub metadata_projection_sha256: String,
    pub backend_catalog_projection_sha256: String,
    pub node_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryProfileInformationV1 {
    pub schema: String,
    pub namespace: String,
    pub node_identity_sha256: String,
    pub profile_generation: u64,
    pub event_sha256: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub stale: bool,
}

/// This record deliberately has no generic Serde implementation. Its only
/// supported wire surface is [`Self::canonical_bytes`] and
/// [`Self::decode_canonical`].
///
/// ```compile_fail
/// use ostadix_api::api::WorldReceiptInformationV1;
///
/// fn require_serialize<T: serde::Serialize>() {}
/// require_serialize::<WorldReceiptInformationV1>();
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldReceiptInformationV1 {
    pub schema: String,
    pub receipt_sha256: String,
    pub semantic_sha256: String,
    pub signature_validated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectGraphInformationV1 {
    pub schema: String,
    pub logical_graph_sha256: String,
    pub source_bundle_sha256: String,
    pub operation_count: u64,
    pub root_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedJournalInformationV1 {
    pub schema: String,
    pub session_identity_sha256: String,
    pub sequence: u64,
    pub previous_entry_identity_sha256: Option<String>,
    pub entry_identity_sha256: String,
    pub recorded_unix_ms: u64,
    pub signature_self_consistent: bool,
    pub signer_trust_evaluated: bool,
}

macro_rules! private_projection_wire {
    ($wire:ident, $record:ident { $($field:ident: $type:ty),+ $(,)? }) => {
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct $wire {
            $($field: $type),+
        }

        impl From<&$record> for $wire {
            fn from(record: &$record) -> Self {
                Self {
                    $($field: record.$field.clone()),+
                }
            }
        }

        impl From<$wire> for $record {
            fn from(wire: $wire) -> Self {
                Self {
                    $($field: wire.$field),+
                }
            }
        }
    };
}

private_projection_wire!(
    ParsedDocumentInformationWireV1,
    ParsedDocumentInformationV1 {
        schema: String,
        source_sha256: String,
        source_len: u64,
        syntax_node_count: u64,
        plan_origin_count: u64,
        plan_origins_sha256: String,
    }
);
private_projection_wire!(
    PublicValueInformationWireV1,
    PublicValueInformationV1 {
        schema: String,
        value_kind: String,
        canonical_sha256: String,
        canonical_len: u64,
        caller_declared_public: bool,
    }
);
private_projection_wire!(
    HGraphInformationWireV1,
    HGraphInformationV1 {
        schema: String,
        metadata_projection_sha256: String,
        node_count: u64,
        constraint_edge_count: u64,
        execution_operation_count: u64,
        root_count: u64,
        sequence_dependency_count: u64,
        admission_evidence_input_count: u64,
    }
);
private_projection_wire!(
    EvidenceInformationWireV1,
    EvidenceInformationV1 {
        schema: String,
        evidence_schema: String,
        analyzer: String,
        metadata_projection_sha256: String,
        backend_catalog_projection_sha256: String,
        node_count: u64,
    }
);
private_projection_wire!(
    RegistryProfileInformationWireV1,
    RegistryProfileInformationV1 {
        schema: String,
        namespace: String,
        node_identity_sha256: String,
        profile_generation: u64,
        event_sha256: String,
        issued_at_ms: u64,
        expires_at_ms: u64,
        stale: bool,
    }
);
private_projection_wire!(
    WorldReceiptInformationWireV1,
    WorldReceiptInformationV1 {
        schema: String,
        receipt_sha256: String,
        semantic_sha256: String,
        signature_validated: bool,
    }
);
private_projection_wire!(
    ProjectGraphInformationWireV1,
    ProjectGraphInformationV1 {
        schema: String,
        logical_graph_sha256: String,
        source_bundle_sha256: String,
        operation_count: u64,
        root_count: u64,
    }
);
private_projection_wire!(
    HostedJournalInformationWireV1,
    HostedJournalInformationV1 {
        schema: String,
        session_identity_sha256: String,
        sequence: u64,
        previous_entry_identity_sha256: Option<String>,
        entry_identity_sha256: String,
        recorded_unix_ms: u64,
        signature_self_consistent: bool,
        signer_trust_evaluated: bool,
    }
);

fn decode_projection<T>(bytes: &[u8]) -> Result<T, InformationBridgeErrorV1>
where
    T: DeserializeOwned + Serialize,
{
    let record = decode_bounded(
        bytes,
        DecodeLimits {
            max_bytes: MAX_INFORMATION_BRIDGE_RECORD_BYTES_V1,
            max_items: MAX_INFORMATION_BRIDGE_DECODE_ITEMS_V1,
            max_depth: MAX_INFORMATION_BRIDGE_DECODE_DEPTH_V1,
        },
    )
    .map_err(|error| InformationBridgeErrorV1::Canonical(error.to_string()))?;
    let canonical =
        encode(&record).map_err(|error| InformationBridgeErrorV1::Canonical(error.to_string()))?;
    if canonical != bytes {
        return Err(InformationBridgeErrorV1::Canonical(
            "record is not in canonical CBOR form".to_string(),
        ));
    }
    Ok(record)
}

fn projection_bytes<T: Serialize>(record: &T) -> Result<Vec<u8>, InformationBridgeErrorV1> {
    let bytes =
        encode(record).map_err(|error| InformationBridgeErrorV1::Canonical(error.to_string()))?;
    if bytes.len() > MAX_INFORMATION_BRIDGE_RECORD_BYTES_V1 {
        return Err(InformationBridgeErrorV1::InvalidInput(format!(
            "projection record has {} bytes; maximum is {}",
            bytes.len(),
            MAX_INFORMATION_BRIDGE_RECORD_BYTES_V1
        )));
    }
    Ok(bytes)
}

fn record_ref(
    schema: &'static str,
    bytes: &[u8],
) -> Result<NativeRecordRefV1, InformationBridgeErrorV1> {
    let logical_len = u64::try_from(bytes.len()).map_err(|_| {
        InformationBridgeErrorV1::InvalidInput(
            "projection length does not fit the V1 record length".to_string(),
        )
    })?;
    Ok(NativeRecordRefV1::new(
        schema,
        INFORMATION_BRIDGE_MEDIA_TYPE_V1,
        hex::encode(Sha256::digest(bytes)),
        logical_len,
    )?)
}

fn check_schema(actual: &str, expected: &'static str) -> Result<(), InformationBridgeErrorV1> {
    if actual == expected {
        Ok(())
    } else {
        Err(InformationBridgeErrorV1::WrongSchema {
            expected,
            actual: actual.to_string(),
        })
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_record(message: impl Into<String>) -> InformationBridgeErrorV1 {
    InformationBridgeErrorV1::InvalidInput(message.into())
}

fn validate_parsed_document_information_v1(
    record: &ParsedDocumentInformationV1,
) -> Result<(), InformationBridgeErrorV1> {
    if !valid_sha256(&record.source_sha256) || !valid_sha256(&record.plan_origins_sha256) {
        return Err(invalid_record(
            "parsed-document projection digests must be lowercase sha256",
        ));
    }
    Ok(())
}

fn validate_public_value_information_v1(
    record: &PublicValueInformationV1,
) -> Result<(), InformationBridgeErrorV1> {
    if !matches!(
        record.value_kind.as_str(),
        "null" | "bool" | "number" | "text" | "char" | "symbol" | "keyword"
    ) || !record.caller_declared_public
        || record.canonical_len == 0
        || record.canonical_len > MAX_PUBLIC_VALUE_CANONICAL_BYTES_V1 as u64
        || !valid_sha256(&record.canonical_sha256)
    {
        return Err(invalid_record(
            "public-value projection violates its scalar/digest/length declaration",
        ));
    }
    Ok(())
}

fn validate_hgraph_information_v1(
    record: &HGraphInformationV1,
) -> Result<(), InformationBridgeErrorV1> {
    if record.admission_evidence_input_count != 0
        || record.root_count > record.node_count
        || record.execution_operation_count > record.node_count
    {
        return Err(invalid_record(
            "HGraph projection counts describe an admitted or impossible graph",
        ));
    }
    let projection = HGraphDigestProjectionV1 {
        schema: HGRAPH_INFORMATION_SCHEMA_V1,
        node_count: record.node_count,
        constraint_edge_count: record.constraint_edge_count,
        execution_operation_count: record.execution_operation_count,
        root_count: record.root_count,
        sequence_dependency_count: record.sequence_dependency_count,
        admission_evidence_input_count: record.admission_evidence_input_count,
    };
    if !valid_sha256(&record.metadata_projection_sha256)
        || metadata_projection_sha256(HGRAPH_METADATA_PROJECTION_DIGEST_DOMAIN_V1, &projection)?
            != record.metadata_projection_sha256
    {
        return Err(invalid_record("HGraph projection digest mismatch"));
    }
    Ok(())
}

fn validate_evidence_information_v1(
    record: &EvidenceInformationV1,
) -> Result<(), InformationBridgeErrorV1> {
    if record.evidence_schema != EVIDENCE_SCHEMA_V6
        || record.analyzer != ANALYZER_ID_V6
        || !valid_sha256(&record.backend_catalog_projection_sha256)
    {
        return Err(invalid_record(
            "Evidence projection is not the exact bounded V6/Catalog V5 coordinate",
        ));
    }
    let projection = EvidenceDigestProjectionV1 {
        schema: EVIDENCE_INFORMATION_SCHEMA_V1,
        evidence_schema: &record.evidence_schema,
        analyzer: &record.analyzer,
        backend_catalog_projection_sha256: &record.backend_catalog_projection_sha256,
        node_count: record.node_count,
    };
    if !valid_sha256(&record.metadata_projection_sha256)
        || metadata_projection_sha256(EVIDENCE_METADATA_PROJECTION_DIGEST_DOMAIN_V1, &projection)?
            != record.metadata_projection_sha256
    {
        return Err(invalid_record("Evidence projection digest mismatch"));
    }
    Ok(())
}

fn validate_registry_profile_information_v1(
    record: &RegistryProfileInformationV1,
) -> Result<(), InformationBridgeErrorV1> {
    let namespace_is_canonical = !record.namespace.is_empty()
        && record.namespace.len() <= MAX_NAMESPACE_BYTES
        && record.namespace.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
                && segment
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && segment
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        });
    if !namespace_is_canonical
        || !valid_sha256(&record.node_identity_sha256)
        || record.profile_generation == 0
        || record.issued_at_ms >= record.expires_at_ms
        || !valid_sha256(&record.event_sha256)
    {
        return Err(invalid_record(
            "registry-profile projection violates digest/identity/generation/time invariants",
        ));
    }
    Ok(())
}

fn validate_world_receipt_information_v1(
    record: &WorldReceiptInformationV1,
) -> Result<(), InformationBridgeErrorV1> {
    if !record.signature_validated
        || !valid_sha256(&record.receipt_sha256)
        || !valid_sha256(&record.semantic_sha256)
    {
        return Err(invalid_record(
            "World receipt projection requires verified lowercase receipt digests",
        ));
    }
    Ok(())
}

fn validate_project_graph_information_v1(
    record: &ProjectGraphInformationV1,
) -> Result<(), InformationBridgeErrorV1> {
    if record.root_count > record.operation_count
        || !valid_sha256(&record.logical_graph_sha256)
        || !valid_sha256(&record.source_bundle_sha256)
    {
        return Err(invalid_record(
            "project graph projection violates digest/count invariants",
        ));
    }
    Ok(())
}

fn validate_hosted_journal_information_v1(
    record: &HostedJournalInformationV1,
) -> Result<(), InformationBridgeErrorV1> {
    if !valid_sha256(&record.session_identity_sha256)
        || record.sequence == 0
        || !record.signature_self_consistent
        || record.signer_trust_evaluated
        || !valid_sha256(&record.entry_identity_sha256)
        || record
            .previous_entry_identity_sha256
            .as_deref()
            .is_some_and(|digest| !valid_sha256(digest))
    {
        return Err(invalid_record(
            "hosted journal projection violates self-signature/non-trust/sequence invariants",
        ));
    }
    Ok(())
}

macro_rules! impl_projection_record {
    ($record:ty, $wire:ty, $schema:expr, $validate:ident) => {
        impl $record {
            pub fn canonical_bytes(&self) -> Result<Vec<u8>, InformationBridgeErrorV1> {
                check_schema(&self.schema, $schema)?;
                $validate(self)?;
                projection_bytes(&<$wire>::from(self))
            }

            /// Decode canonical projection metadata. This checks its shape and
            /// encoding, not the trustworthiness of the party that supplied it.
            pub fn decode_canonical(bytes: &[u8]) -> Result<Self, InformationBridgeErrorV1> {
                let wire: $wire = decode_projection(bytes)?;
                let record: Self = wire.into();
                check_schema(&record.schema, $schema)?;
                $validate(&record)?;
                Ok(record)
            }

            pub fn native_record_ref(&self) -> Result<NativeRecordRefV1, InformationBridgeErrorV1> {
                let bytes = self.canonical_bytes()?;
                record_ref($schema, &bytes)
            }
        }
    };
}

impl_projection_record!(
    ParsedDocumentInformationV1,
    ParsedDocumentInformationWireV1,
    PARSED_DOCUMENT_INFORMATION_SCHEMA_V1,
    validate_parsed_document_information_v1
);
impl_projection_record!(
    PublicValueInformationV1,
    PublicValueInformationWireV1,
    PUBLIC_VALUE_INFORMATION_SCHEMA_V1,
    validate_public_value_information_v1
);
impl_projection_record!(
    HGraphInformationV1,
    HGraphInformationWireV1,
    HGRAPH_INFORMATION_SCHEMA_V1,
    validate_hgraph_information_v1
);
impl_projection_record!(
    EvidenceInformationV1,
    EvidenceInformationWireV1,
    EVIDENCE_INFORMATION_SCHEMA_V1,
    validate_evidence_information_v1
);
impl_projection_record!(
    RegistryProfileInformationV1,
    RegistryProfileInformationWireV1,
    REGISTRY_PROFILE_INFORMATION_SCHEMA_V1,
    validate_registry_profile_information_v1
);
impl_projection_record!(
    WorldReceiptInformationV1,
    WorldReceiptInformationWireV1,
    WORLD_RECEIPT_INFORMATION_SCHEMA_V1,
    validate_world_receipt_information_v1
);
impl_projection_record!(
    ProjectGraphInformationV1,
    ProjectGraphInformationWireV1,
    PROJECT_GRAPH_INFORMATION_SCHEMA_V1,
    validate_project_graph_information_v1
);
impl_projection_record!(
    HostedJournalInformationV1,
    HostedJournalInformationWireV1,
    HOSTED_JOURNAL_INFORMATION_SCHEMA_V1,
    validate_hosted_journal_information_v1
);

#[derive(Serialize)]
struct SpanProjectionV1 {
    start_byte: u64,
    end_byte: u64,
    start_line: u64,
    start_column: u64,
    end_line: u64,
    end_column: u64,
}

fn usize_v1(value: usize, field: &'static str) -> Result<u64, InformationBridgeErrorV1> {
    u64::try_from(value)
        .map_err(|_| InformationBridgeErrorV1::InvalidInput(format!("{field} does not fit u64")))
}

pub fn project_parsed_document_v1(
    source: &[u8],
    document: &ParsedDocumentV1,
) -> Result<ParsedDocumentInformationV1, InformationBridgeErrorV1> {
    let source_text = std::str::from_utf8(source).map_err(|error| {
        InformationBridgeErrorV1::InvalidInput(format!("source is not UTF-8: {error}"))
    })?;
    let source_sha256: [u8; 32] = Sha256::digest(source).into();
    if document.source_len() != source.len() || document.source_sha256() != &source_sha256 {
        return Err(InformationBridgeErrorV1::InvalidInput(
            "parsed document was not produced from the supplied source bytes".to_string(),
        ));
    }
    let mut spans = Vec::with_capacity(document.plan_origins().len());
    for span in document.plan_origins() {
        if span.start_byte > span.end_byte
            || span.end_byte > source.len()
            || !source_text.is_char_boundary(span.start_byte)
            || !source_text.is_char_boundary(span.end_byte)
        {
            return Err(InformationBridgeErrorV1::InvalidInput(
                "parsed-document source span is outside the exact source bytes".to_string(),
            ));
        }
        spans.push(SpanProjectionV1 {
            start_byte: usize_v1(span.start_byte, "source span start")?,
            end_byte: usize_v1(span.end_byte, "source span end")?,
            start_line: usize_v1(span.start_line, "source span start line")?,
            start_column: usize_v1(span.start_column, "source span start column")?,
            end_line: usize_v1(span.end_line, "source span end line")?,
            end_column: usize_v1(span.end_column, "source span end column")?,
        });
    }
    let span_bytes =
        encode(&spans).map_err(|error| InformationBridgeErrorV1::Canonical(error.to_string()))?;
    Ok(ParsedDocumentInformationV1 {
        schema: PARSED_DOCUMENT_INFORMATION_SCHEMA_V1.to_string(),
        source_sha256: hex::encode(source_sha256),
        source_len: usize_v1(source.len(), "source length")?,
        syntax_node_count: usize_v1(document.nodes().len(), "syntax node count")?,
        plan_origin_count: usize_v1(spans.len(), "plan origin count")?,
        plan_origins_sha256: hex::encode(Sha256::digest(&span_bytes)),
    })
}

/// Project only an explicitly allowlisted scalar. Calling this function is the
/// caller's declaration that the scalar is public; no confidentiality policy
/// is inferred from its runtime type.
pub fn project_public_value_v1(
    value: &OValue,
) -> Result<PublicValueInformationV1, InformationBridgeErrorV1> {
    let value_kind = match value {
        OValue::Null => "null",
        OValue::Bool { .. } => "bool",
        OValue::Number { v } => {
            validate_public_number_v1(v)?;
            "number"
        }
        OValue::Text { v } => {
            if v.utf8.len() > MAX_PUBLIC_VALUE_TEXT_BYTES_V1
                || v.encoding
                    .as_ref()
                    .is_some_and(|encoding| encoding.len() > MAX_PUBLIC_VALUE_IDENTIFIER_BYTES_V1)
            {
                return Err(InformationBridgeErrorV1::InvalidInput(
                    "public text exceeds bridge scalar bounds".to_string(),
                ));
            }
            "text"
        }
        OValue::Char { .. } => "char",
        OValue::Symbol { v } => {
            validate_public_identifier_v1(v.namespace.as_deref(), &v.name)?;
            "symbol"
        }
        OValue::Keyword { v } => {
            validate_public_identifier_v1(v.namespace.as_deref(), &v.name)?;
            "keyword"
        }
        OValue::Html { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("html")),
        OValue::StorePath { .. } => {
            return Err(InformationBridgeErrorV1::UnsupportedValue("store-path"))
        }
        OValue::Expr { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("expr")),
        OValue::List { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("list")),
        OValue::Map { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("map")),
        OValue::Seq { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("seq")),
        OValue::Object { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("object")),
        OValue::EntriesMap { .. } => {
            return Err(InformationBridgeErrorV1::UnsupportedValue("entries-map"))
        }
        OValue::Set { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("set")),
        OValue::Scope { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("scope")),
        OValue::Blob { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("blob")),
        OValue::Bytes { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("bytes")),
        OValue::Graph { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("graph")),
        OValue::Native { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("native")),
        OValue::NixExpr { .. } => {
            return Err(InformationBridgeErrorV1::UnsupportedValue("nix-expr"))
        }
        OValue::Derivation { .. } => {
            return Err(InformationBridgeErrorV1::UnsupportedValue("derivation"))
        }
        OValue::Request { .. } => {
            return Err(InformationBridgeErrorV1::UnsupportedValue("request"))
        }
        OValue::System { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("system")),
        OValue::Capability { .. } => {
            return Err(InformationBridgeErrorV1::UnsupportedValue("capability"))
        }
        OValue::Snapshot { .. } => {
            return Err(InformationBridgeErrorV1::UnsupportedValue("snapshot"))
        }
        OValue::Thunk { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("thunk")),
        OValue::Group { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("group")),
        OValue::Error { .. } => return Err(InformationBridgeErrorV1::UnsupportedValue("error")),
    };
    let canonical = value.canonical_bytes();
    if canonical.len() > MAX_PUBLIC_VALUE_CANONICAL_BYTES_V1 {
        return Err(InformationBridgeErrorV1::InvalidInput(format!(
            "public scalar has {} canonical bytes; maximum is {}",
            canonical.len(),
            MAX_PUBLIC_VALUE_CANONICAL_BYTES_V1
        )));
    }
    Ok(PublicValueInformationV1 {
        schema: PUBLIC_VALUE_INFORMATION_SCHEMA_V1.to_string(),
        value_kind: value_kind.to_string(),
        canonical_sha256: hex::encode(Sha256::digest(&canonical)),
        canonical_len: usize_v1(canonical.len(), "canonical value length")?,
        caller_declared_public: true,
    })
}

fn validate_public_identifier_v1(
    namespace: Option<&str>,
    name: &str,
) -> Result<(), InformationBridgeErrorV1> {
    if name.is_empty()
        || name.len() > MAX_PUBLIC_VALUE_IDENTIFIER_BYTES_V1
        || namespace.is_some_and(|value| value.len() > MAX_PUBLIC_VALUE_IDENTIFIER_BYTES_V1)
    {
        return Err(InformationBridgeErrorV1::InvalidInput(
            "public symbol/keyword identifier is empty or exceeds bridge bounds".to_string(),
        ));
    }
    Ok(())
}

fn validate_public_number_v1(number: &ONumber) -> Result<(), InformationBridgeErrorV1> {
    let mut stack = vec![(number, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((number, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_PUBLIC_VALUE_NUMBER_NODES_V1 || depth > MAX_PUBLIC_VALUE_NUMBER_DEPTH_V1 {
            return Err(InformationBridgeErrorV1::InvalidInput(
                "public number exceeds bridge depth/node bounds".to_string(),
            ));
        }
        let bounded_integer = |bits: u64| {
            if bits > MAX_PUBLIC_VALUE_NUMBER_BYTES_V1 * 8 {
                Err(InformationBridgeErrorV1::InvalidInput(
                    "public number integer exceeds bridge bounds".to_string(),
                ))
            } else {
                Ok(())
            }
        };
        match number {
            ONumber::Int { v } => bounded_integer(v.bits())?,
            ONumber::Rational { num, den } => {
                bounded_integer(num.bits())?;
                bounded_integer(den.bits())?;
                if *den == 0.into() {
                    return Err(InformationBridgeErrorV1::InvalidInput(
                        "public rational denominator is zero".to_string(),
                    ));
                }
            }
            ONumber::Decimal { coeff, .. } => bounded_integer(coeff.bits())?,
            ONumber::BinaryFloat { format, bits } => {
                let expected = match format {
                    FloatFormat::F32 => 4,
                    FloatFormat::F64 => 8,
                };
                if bits.len() != expected {
                    return Err(InformationBridgeErrorV1::InvalidInput(
                        "public binary float width does not match its format".to_string(),
                    ));
                }
            }
            ONumber::BigFloat {
                mantissa,
                precision,
                ..
            } => {
                bounded_integer(mantissa.bits())?;
                if precision.is_some_and(|value| value == 0) {
                    return Err(InformationBridgeErrorV1::InvalidInput(
                        "public big-float precision must be positive when present".to_string(),
                    ));
                }
            }
            ONumber::Complex { re, im } => {
                stack.push((re, depth + 1));
                stack.push((im, depth + 1));
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct HGraphDigestProjectionV1 {
    schema: &'static str,
    node_count: u64,
    constraint_edge_count: u64,
    execution_operation_count: u64,
    root_count: u64,
    sequence_dependency_count: u64,
    admission_evidence_input_count: u64,
}

#[derive(Serialize)]
struct EvidenceDigestProjectionV1<'a> {
    schema: &'static str,
    evidence_schema: &'a str,
    analyzer: &'a str,
    backend_catalog_projection_sha256: &'a str,
    node_count: u64,
}

fn metadata_projection_sha256<T: Serialize>(
    domain: &'static str,
    projection: &T,
) -> Result<String, InformationBridgeErrorV1> {
    let bytes = encode(projection)
        .map_err(|error| InformationBridgeErrorV1::Canonical(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn projected_identity_sha256(domain: &'static str, identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update((identity.len() as u64).to_be_bytes());
    hasher.update(identity.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn project_hgraph_v1(graph: &HGraph) -> Result<HGraphInformationV1, InformationBridgeErrorV1> {
    graph
        .validate_execution_graph()
        .map_err(InformationBridgeErrorV1::InvalidInput)?;
    if graph.admission_evidence_input_count() != 0 || graph.contains_admission_evidence_node() {
        return Err(InformationBridgeErrorV1::InvalidInput(
            "admitted HGraphs with materialized evidence inputs are outside the bridge".to_string(),
        ));
    }
    let projection = HGraphDigestProjectionV1 {
        schema: HGRAPH_INFORMATION_SCHEMA_V1,
        node_count: usize_v1(graph.node_count(), "HGraph node count")?,
        constraint_edge_count: usize_v1(
            graph.constraint_edge_count(),
            "HGraph constraint edge count",
        )?,
        execution_operation_count: usize_v1(
            graph.execution_operation_count(),
            "HGraph execution operation count",
        )?,
        root_count: usize_v1(graph.root_nodes().len(), "HGraph root count")?,
        sequence_dependency_count: usize_v1(
            graph.sequence_dependencies().len(),
            "HGraph sequence dependency count",
        )?,
        admission_evidence_input_count: usize_v1(
            graph.admission_evidence_input_count(),
            "HGraph admission evidence input count",
        )?,
    };
    Ok(HGraphInformationV1 {
        schema: projection.schema.to_string(),
        metadata_projection_sha256: metadata_projection_sha256(
            HGRAPH_METADATA_PROJECTION_DIGEST_DOMAIN_V1,
            &projection,
        )?,
        node_count: projection.node_count,
        constraint_edge_count: projection.constraint_edge_count,
        execution_operation_count: projection.execution_operation_count,
        root_count: projection.root_count,
        sequence_dependency_count: projection.sequence_dependency_count,
        admission_evidence_input_count: projection.admission_evidence_input_count,
    })
}

pub fn project_evidence_v6(
    bundle: &EvidenceBundleV6,
) -> Result<EvidenceInformationV1, InformationBridgeErrorV1> {
    if bundle.schema() != EVIDENCE_SCHEMA_V6 || bundle.analyzer() != ANALYZER_ID_V6 {
        return Err(InformationBridgeErrorV1::InvalidInput(
            "only the exact Evidence V6 schema/analyzer coordinate is accepted".to_string(),
        ));
    }
    let bindings = bundle.bindings();
    let projection = EvidenceDigestProjectionV1 {
        schema: EVIDENCE_INFORMATION_SCHEMA_V1,
        evidence_schema: bundle.schema(),
        analyzer: bundle.analyzer(),
        backend_catalog_projection_sha256: &bindings.backend_catalog_projection_sha256,
        node_count: usize_v1(bundle.node_count(), "Evidence V6 node count")?,
    };
    Ok(EvidenceInformationV1 {
        schema: projection.schema.to_string(),
        evidence_schema: projection.evidence_schema.to_string(),
        analyzer: projection.analyzer.to_string(),
        metadata_projection_sha256: metadata_projection_sha256(
            EVIDENCE_METADATA_PROJECTION_DIGEST_DOMAIN_V1,
            &projection,
        )?,
        backend_catalog_projection_sha256: bindings.backend_catalog_projection_sha256.clone(),
        node_count: projection.node_count,
    })
}

pub fn project_registry_profile_v1(
    profile: &VerifiedRegistryProfileV1,
) -> RegistryProfileInformationV1 {
    let publication = profile.publication();
    RegistryProfileInformationV1 {
        schema: REGISTRY_PROFILE_INFORMATION_SCHEMA_V1.to_string(),
        namespace: publication.namespace().to_string(),
        node_identity_sha256: projected_identity_sha256(
            REGISTRY_NODE_IDENTITY_DIGEST_DOMAIN_V1,
            publication.node_id(),
        ),
        profile_generation: publication.profile().profile_generation().get(),
        event_sha256: hex::encode(profile.event_sha256()),
        issued_at_ms: profile.issued_at_ms(),
        expires_at_ms: profile.expires_at_ms(),
        stale: profile.is_stale(),
    }
}

pub fn project_world_receipt_v1(
    receipt: &VerifiedExecutionReceiptV1,
) -> Result<WorldReceiptInformationV1, InformationBridgeErrorV1> {
    let bytes = receipt.signed().bytes();
    let receipt_sha256 = receipt_v1_sha256(bytes)
        .map_err(|error| InformationBridgeErrorV1::InvalidInput(error.to_string()))?;
    let semantic_sha256 = project_receipt_semantic_sha256_v1(bytes)
        .map_err(|error| InformationBridgeErrorV1::InvalidInput(error.to_string()))?;
    Ok(WorldReceiptInformationV1 {
        schema: WORLD_RECEIPT_INFORMATION_SCHEMA_V1.to_string(),
        receipt_sha256: hex::encode(receipt_sha256),
        semantic_sha256: hex::encode(semantic_sha256),
        signature_validated: true,
    })
}

pub fn project_logical_hgraph_v1(
    graph: &LogicalHGraphV1,
) -> Result<ProjectGraphInformationV1, InformationBridgeErrorV1> {
    graph
        .canonical_bytes()
        .map_err(|error| InformationBridgeErrorV1::InvalidInput(error.to_string()))?;
    let digest = graph
        .digest()
        .map_err(|error| InformationBridgeErrorV1::InvalidInput(error.to_string()))?;
    Ok(ProjectGraphInformationV1 {
        schema: PROJECT_GRAPH_INFORMATION_SCHEMA_V1.to_string(),
        logical_graph_sha256: digest.as_sha256().to_string(),
        source_bundle_sha256: graph.source.bundle.as_sha256().to_string(),
        operation_count: usize_v1(graph.operations.len(), "logical project operation count")?,
        root_count: usize_v1(graph.roots.len(), "logical project root count")?,
    })
}

pub fn project_hosted_journal_v2(
    entry: &SignedJournalEntryV2,
) -> Result<HostedJournalInformationV1, InformationBridgeErrorV1> {
    entry
        .verify()
        .map_err(|error| InformationBridgeErrorV1::InvalidInput(error.to_string()))?;
    if entry.entry.sequence == 0
        || entry
            .entry
            .previous_entry_sha256
            .as_deref()
            .is_some_and(|digest| !valid_sha256(digest))
    {
        return Err(InformationBridgeErrorV1::InvalidInput(
            "hosted journal sequence/previous-entry coordinate is invalid".to_string(),
        ));
    }
    canonical_hosted_bytes(entry)
        .map_err(|error| InformationBridgeErrorV1::InvalidInput(error.to_string()))?;
    let projected = HostedJournalInformationV1 {
        schema: HOSTED_JOURNAL_INFORMATION_SCHEMA_V1.to_string(),
        session_identity_sha256: projected_identity_sha256(
            HOSTED_SESSION_IDENTITY_DIGEST_DOMAIN_V1,
            &entry.entry.session_id,
        ),
        sequence: entry.entry.sequence,
        previous_entry_identity_sha256: entry.entry.previous_entry_sha256.as_deref().map(
            |digest| projected_identity_sha256(HOSTED_ENTRY_IDENTITY_DIGEST_DOMAIN_V1, digest),
        ),
        entry_identity_sha256: projected_identity_sha256(
            HOSTED_ENTRY_IDENTITY_DIGEST_DOMAIN_V1,
            &entry.entry_sha256,
        ),
        recorded_unix_ms: entry.entry.recorded_unix_ms,
        signature_self_consistent: true,
        signer_trust_evaluated: false,
    };
    validate_hosted_journal_information_v1(&projected)?;
    Ok(projected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_decoder_accepts_reachable_exact_item_and_depth_limits() {
        let mut exact_items = vec![0x98, 0xff];
        exact_items.extend(std::iter::repeat_n(
            0xf6,
            MAX_INFORMATION_BRIDGE_DECODE_ITEMS_V1 - 1,
        ));
        decode_projection::<serde_json::Value>(&exact_items).unwrap();

        let mut exact_depth = vec![0x81; MAX_INFORMATION_BRIDGE_DECODE_DEPTH_V1];
        exact_depth.push(0xf6);
        decode_projection::<serde_json::Value>(&exact_depth).unwrap();
    }
}
