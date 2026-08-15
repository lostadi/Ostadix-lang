use thiserror::Error;

/// Structural, temporal, and scope failures in the hosted placement protocol.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PlacementValidationError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds the {limit}-byte limit")]
    TooLong { field: &'static str, limit: usize },
    #[error("{field} contains an unsupported value `{value}`")]
    InvalidToken { field: &'static str, value: String },
    #[error("{field} must be exactly 64 lowercase hexadecimal SHA-256 characters")]
    InvalidDigest { field: &'static str },
    #[error("{field} must be nonzero")]
    Zero { field: &'static str },
    #[error("{record} has an invalid validity interval")]
    InvalidValidity { record: &'static str },
    #[error("{record} exceeds its maximum lifetime of {maximum_ms}ms")]
    LifetimeExceeded {
        record: &'static str,
        maximum_ms: u64,
    },
    #[error("{record} is not yet valid")]
    NotYetValid { record: &'static str },
    #[error("{record} has expired")]
    Expired { record: &'static str },
    #[error("{record} authentication was not established")]
    Unauthenticated { record: &'static str },
    #[error("scope mismatch for {field}: expected {expected}, got {got}")]
    ScopeMismatch {
        field: &'static str,
        expected: String,
        got: String,
    },
    #[error("unsupported target capability model")]
    UnsupportedCapabilityModel,
    #[error("requirement footprint is conservatively unknown: {0:?}")]
    ConservativeUnknown(Vec<String>),
    #[error("requirement footprint is unsatisfiable: {0:?}")]
    Unsatisfiable(Vec<String>),
    #[error("target does not support requirement `{0}`")]
    UnsupportedRequirement(String),
    #[error(
        "backend specification `{specification}` is not authorized by current catalog `{current_schema}`"
    )]
    NonCurrentBackendCatalog {
        specification: String,
        current_schema: String,
    },
    #[error("node capacity cannot satisfy the requested reservation")]
    InsufficientCapacity,
    #[error("invalid state quota relationship: {0}")]
    InvalidStateQuota(String),
    #[error("state quota `{dimension}` cannot admit {requested} units against limit {limit}")]
    StateQuotaExceeded {
        dimension: String,
        requested: u64,
        limit: u64,
    },
    #[error("external-pinned state requires at least one governed resource identity")]
    EmptyPinnedStateResources,
    #[error("warrant `{0}` was not supplied")]
    MissingWarrant(String),
    #[error("requirement `{0}` has no exact discharge")]
    MissingDischarge(String),
    #[error("discharge contains an atom that is not required: `{0}`")]
    ExtraneousDischarge(String),
    #[error("warrant assertion does not discharge requirement `{0}`")]
    WarrantAssertionMismatch(String),
    #[error("warrant tier `{0}` is not authorized by the placement trust policy")]
    WarrantTierNotAllowed(String),
    #[error("a fresh discovered negative warrant vetoes requirement `{0}`")]
    NegativeVeto(String),
    #[error("historical warrant has {observed} observations; at least {minimum} are required")]
    InsufficientHistoricalObservations { observed: u32, minimum: u32 },
    #[error("canonical serialization failed: {0}")]
    CanonicalSerialization(String),
    #[error("duplicate {kind} `{value}`")]
    Duplicate { kind: &'static str, value: String },
    #[error("plan node P{node} is out of bounds for a plan with {len} nodes")]
    PlanNodeOutOfBounds { node: usize, len: usize },
}
