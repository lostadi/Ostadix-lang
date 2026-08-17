use std::collections::BTreeMap;

use o_lang::ir::BackendRegistry;
use o_lang::placement::{
    BackendImplementationIdV1, BackendStateSupportV2, CanonicalPlacementRecordV1, CapabilityAtomV1,
    CapabilityKeyV1, CapacityObservationV1, CurrentBackendCatalogV1, DischargedRequirementV1,
    EndiannessV1, ExternalPinnedStateManifestV2, GenerationV1, LeaseExpectationV1,
    LeaseExpectationV2, LeaseStateBindingV2, NodeProfileV1, PlacementCandidateInputV1,
    PlacementLeaseV1, PlacementLeaseV2, PlacementReservationV1, PlacementTrustPolicyV1,
    PlacementValidationError, PlacementWarrantV1, PlatformDescriptorV1, RecordAuthenticationV1,
    RecordAuthenticatorV1, RequirementAtomV1, RequirementFootprintV1, SemanticDigestV1,
    SnapshotCompatibilityV2, StateCapacityObservationV2, StateCapacityRefusalV2,
    StateCheckpointPayloadV2, StateCheckpointV2, StateQuotaDimensionV2, StateQuotaLimitsV2,
    StateReservationV2, StateSessionIdV2, TargetCapabilityModelV1, TargetDescriptorV1,
    TaskAttemptIdV1, UnixMillisV1, WarrantAssertionV1, WarrantDischargeV1, WarrantScopeV1,
    WarrantTierV1,
};
use o_lang::registry::bundle::{
    LOCAL_BACKEND_PROTOCOL_ABI_V1, LOCAL_REALIZATION_DIGEST_DOMAIN_V1, LOCAL_REALIZATION_SCHEMA_V1,
};
use o_lang::world::ArtifactId;

struct AcceptAll;

impl RecordAuthenticatorV1 for AcceptAll {
    fn authenticate(&self, _record: &RecordAuthenticationV1) -> bool {
        true
    }
}

struct RejectAll;

impl RecordAuthenticatorV1 for RejectAll {
    fn authenticate(&self, _record: &RecordAuthenticationV1) -> bool {
        false
    }
}

struct ExactAuthentication {
    issuer: SemanticDigestV1,
    record: SemanticDigestV1,
}

impl RecordAuthenticatorV1 for ExactAuthentication {
    fn authenticate(&self, record: &RecordAuthenticationV1) -> bool {
        record.record_kind() == "placement lease v2"
            && record.issuer_key() == &self.issuer
            && record.record_digest() == &self.record
    }
}

fn digest(byte: u8) -> SemanticDigestV1 {
    SemanticDigestV1::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
}

fn artifact(byte: u8) -> ArtifactId {
    ArtifactId::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
}

#[test]
fn public_flat_and_nested_placement_paths_share_one_type_identity() {
    let nested = o_lang::placement::protocol::SemanticDigestV1::hash_bytes(
        "ostadix/placement/public-alias-test/v1",
        b"one-canonical-module",
    );
    let flat: o_lang::placement::SemanticDigestV1 = nested;
    let nested_again: o_lang::placement::protocol::SemanticDigestV1 = flat;

    assert_eq!(nested_again.as_sha256().len(), 64);
    assert_eq!(
        std::any::TypeId::of::<o_lang::placement::SemanticDigestV1>(),
        std::any::TypeId::of::<o_lang::placement::protocol::SemanticDigestV1>()
    );
}

fn lease_expectation_v2(state_binding: LeaseStateBindingV2) -> LeaseExpectationV2 {
    LeaseExpectationV2::new(
        "lease-v2-node",
        digest(90),
        GenerationV1::new(2).unwrap(),
        GenerationV1::new(3).unwrap(),
        digest(91),
        digest(92),
        artifact(93),
        digest(94),
        digest(95),
        digest(96),
        TaskAttemptIdV1::new(digest(97), GenerationV1::new(1).unwrap()),
        digest(98),
        digest(99),
        digest(100),
        PlacementReservationV1::new(2, 2_048, 512).unwrap(),
        digest(101),
        state_binding,
    )
    .unwrap()
}

fn capability(namespace: &str, name: &str, level: u32) -> CapabilityAtomV1 {
    CapabilityAtomV1::new(CapabilityKeyV1::new(namespace, name).unwrap(), level).unwrap()
}

fn backend(seed: u8) -> BackendImplementationIdV1 {
    let registry = BackendRegistry::global();
    let specification = SemanticDigestV1::from_sha256(
        registry
            .specification_sha256("python")
            .expect("the canonical Python backend has a specification digest"),
    )
    .unwrap();
    registry
        .backend_implementation_id_v1(
            "python",
            Some(&specification),
            artifact(seed.wrapping_add(1)),
            digest(seed.wrapping_add(2)),
            LOCAL_BACKEND_PROTOCOL_ABI_V1,
        )
        .unwrap()
}

fn backend_with_specification(
    seed: u8,
    specification: SemanticDigestV1,
) -> BackendImplementationIdV1 {
    BackendImplementationIdV1::new(
        specification,
        artifact(seed.wrapping_add(1)),
        digest(seed.wrapping_add(2)),
        "o-shim-v1",
        digest(seed.wrapping_add(3)),
    )
    .unwrap()
}

#[test]
fn placement_v1_canonical_bytes_and_digests_are_pinned() {
    let implementation = backend_with_specification(40, digest(200));
    let descriptor = target(
        "golden-node",
        "Golden node",
        "aarch64",
        vec![capability("cpu", "sve", 2)],
        implementation.clone(),
    );
    let profile = NodeProfileV1::new(
        digest(41),
        descriptor.clone(),
        GenerationV1::new(7).unwrap(),
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(2_000),
    )
    .unwrap();
    let fixtures = [
        (
            "implementation",
            implementation.canonical_bytes().unwrap(),
            implementation.semantic_digest().unwrap(),
            "{\"backend_specification\":\"c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8\",\"adapter_artifact\":\"2929292929292929292929292929292929292929292929292929292929292929\",\"executable_set\":\"2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a\",\"protocol_abi\":\"o-shim-v1\",\"realization_pipeline\":\"2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b\"}",
            "268d9955b677004c1e2fdfc3f8ae7e22954ebdf8830b4044395ad9dce03f2321",
        ),
        (
            "descriptor",
            descriptor.canonical_bytes().unwrap(),
            descriptor.semantic_digest().unwrap(),
            "{\"node_id\":\"golden-node\",\"display_name\":\"Golden node\",\"node_generation\":1,\"capability_model\":\"downward-closed-ideal\",\"platform\":{\"operating_system\":\"linux\",\"architecture\":\"aarch64\",\"abi\":\"gnu\",\"endianness\":\"little\",\"pointer_width\":64},\"capabilities\":[{\"key\":{\"namespace\":\"cpu\",\"name\":\"sve\"},\"level\":2}],\"raw_cpu_features\":[],\"backend_implementations\":[{\"backend_specification\":\"c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8\",\"adapter_artifact\":\"2929292929292929292929292929292929292929292929292929292929292929\",\"executable_set\":\"2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a\",\"protocol_abi\":\"o-shim-v1\",\"realization_pipeline\":\"2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b\"}]}",
            "96ae5fe8f4f25764afd9e956e3a0943c3dbeb65c384bf879e70d2bb9db024eb2",
        ),
        (
            "profile",
            profile.canonical_bytes().unwrap(),
            profile.semantic_digest().unwrap(),
            "{\"issuer_key\":\"2929292929292929292929292929292929292929292929292929292929292929\",\"descriptor\":{\"node_id\":\"golden-node\",\"display_name\":\"Golden node\",\"node_generation\":1,\"capability_model\":\"downward-closed-ideal\",\"platform\":{\"operating_system\":\"linux\",\"architecture\":\"aarch64\",\"abi\":\"gnu\",\"endianness\":\"little\",\"pointer_width\":64},\"capabilities\":[{\"key\":{\"namespace\":\"cpu\",\"name\":\"sve\"},\"level\":2}],\"raw_cpu_features\":[],\"backend_implementations\":[{\"backend_specification\":\"c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8\",\"adapter_artifact\":\"2929292929292929292929292929292929292929292929292929292929292929\",\"executable_set\":\"2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a\",\"protocol_abi\":\"o-shim-v1\",\"realization_pipeline\":\"2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b\"}]},\"profile_generation\":7,\"issued_at\":1000,\"expires_at\":2000}",
            "70840d3eecc5fe5e6cd39bb9194d4b96ff00462aa903760f4a5917d321ce9aec",
        ),
    ];
    for (name, bytes, digest, expected_bytes, expected_digest) in fixtures {
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            expected_bytes,
            "{name} bytes"
        );
        assert_eq!(digest.as_sha256(), expected_digest, "{name} digest");
    }
}

#[test]
fn noncurrent_catalog_profiles_remain_inspectable_but_cannot_authorize() {
    let obsolete_specification = digest(250);
    assert!(!BackendRegistry::global()
        .contains_specification_sha256(obsolete_specification.as_sha256()));
    let descriptor = target(
        "legacy-node",
        "legacy catalog node",
        "x86_64",
        vec![],
        backend_with_specification(240, obsolete_specification.clone()),
    );
    let encoded = serde_json::to_vec(&descriptor).unwrap();
    let decoded: TargetDescriptorV1 = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        decoded, descriptor,
        "archival decoding must remain lossless"
    );

    let profile = NodeProfileV1::new(
        digest(241),
        decoded,
        GenerationV1::new(1).unwrap(),
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(2_000),
    )
    .unwrap();
    assert!(matches!(
        profile.validate_at(UnixMillisV1::new(1_500), &AcceptAll),
        Err(PlacementValidationError::NonCurrentBackendCatalog {
            specification,
            current_schema,
        }) if specification == obsolete_specification.as_sha256()
            && current_schema == "ostadix.backend-catalog/v4"
    ));
}

#[test]
fn current_catalog_state_support_is_exact_and_alias_stable() {
    let registry = BackendRegistry::global();
    let python_digest = SemanticDigestV1::from_sha256(
        registry
            .specification_sha256("python")
            .expect("current Python catalog identity"),
    )
    .unwrap();
    assert_eq!(
        registry.state_support_for("py"),
        registry.state_support_for_current_specification(&python_digest)
    );
    assert!(matches!(
        registry.state_support_for_current_specification(&python_digest),
        Some(BackendStateSupportV2::SemanticSnapshot {
            compatibility: SnapshotCompatibilityV2::ExactImplementation,
            ..
        })
    ));

    let legacy_python = SemanticDigestV1::from_sha256(
        registry
            .specification_sha256_v3("python")
            .expect("archival Python V3 identity"),
    )
    .unwrap();
    assert!(!registry.contains_current_specification(&legacy_python));
    assert_eq!(
        registry.state_support_for_current_specification(&legacy_python),
        None
    );
}

#[test]
fn current_catalog_rejects_legacy_realization_with_a_current_specification() {
    let registry = BackendRegistry::global();
    let current = backend(50);
    let current_target = target(
        "current-realization",
        "current realization",
        "aarch64",
        Vec::new(),
        current.clone(),
    );
    current_target
        .validate_current_backend_catalog_with(registry)
        .unwrap();

    let legacy_material = serde_json::json!({
        "schema": LOCAL_REALIZATION_SCHEMA_V1,
        "backend_specification": current.backend_specification().as_sha256(),
        "adapter_kind": "legacy-python-shim",
        "adapter_artifact": current.adapter_artifact().as_sha256(),
        "executable_set": current.executable_set().as_sha256(),
        "protocol": current.protocol_abi(),
    });
    let legacy_pipeline = SemanticDigestV1::hash_bytes(
        LOCAL_REALIZATION_DIGEST_DOMAIN_V1,
        &serde_json::to_vec(&legacy_material).unwrap(),
    );
    let legacy = BackendImplementationIdV1::new(
        current.backend_specification().clone(),
        current.adapter_artifact().clone(),
        current.executable_set().clone(),
        current.protocol_abi(),
        legacy_pipeline.clone(),
    )
    .unwrap();
    let legacy_target = target(
        "legacy-realization",
        "legacy realization",
        "aarch64",
        Vec::new(),
        legacy,
    );
    assert!(matches!(
        legacy_target.validate_current_backend_catalog_with(registry),
        Err(PlacementValidationError::NonCurrentBackendImplementation {
            realization_pipeline,
            current_schema,
        }) if realization_pipeline == legacy_pipeline.as_sha256()
            && current_schema == "ostadix.backend-catalog/v4"
    ));
}

#[test]
fn state_quotas_are_hard_and_capacity_refusal_is_request_bound_evidence() {
    let limits = StateQuotaLimitsV2::new(2, 3, 100, 300, 500).unwrap();
    let reservation = StateReservationV2::new(2, 100, 200).unwrap();
    assert!(limits.permits(&reservation));
    reservation.validate_against(&limits).unwrap();

    let capacity = StateCapacityObservationV2::new(
        digest(60),
        "state-node",
        GenerationV1::new(4).unwrap(),
        GenerationV1::new(9).unwrap(),
        limits.clone(),
        1,
        250,
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(2_000),
    )
    .unwrap();
    assert_eq!(capacity.issuer_key(), &digest(60));
    assert_eq!(capacity.node_id(), "state-node");
    assert_eq!(capacity.node_generation().get(), 4);
    assert_eq!(capacity.capacity_generation().get(), 9);
    assert_eq!(capacity.open_sessions(), 1);
    assert_eq!(capacity.state_bytes_reserved(), 250);
    assert_eq!(capacity.issued_at(), UnixMillisV1::new(1_000));
    assert_eq!(capacity.expires_at(), UnixMillisV1::new(2_000));
    assert_eq!(capacity.available_sessions(), 1);
    assert_eq!(capacity.available_state_bytes(), 250);
    assert!(capacity.can_admit(&reservation));
    capacity
        .validate_at(UnixMillisV1::new(1_500), &AcceptAll)
        .unwrap();

    let saturated = StateCapacityObservationV2::new(
        digest(60),
        "state-node",
        GenerationV1::new(4).unwrap(),
        GenerationV1::new(10).unwrap(),
        limits,
        2,
        250,
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(2_000),
    )
    .unwrap();
    assert!(!saturated.can_admit(&reservation));

    let refusal = StateCapacityRefusalV2::new(
        digest(60),
        "state-node",
        GenerationV1::new(4).unwrap(),
        GenerationV1::new(10).unwrap(),
        reservation.semantic_digest().unwrap(),
        StateQuotaDimensionV2::OpenSessions,
        1,
        2,
        2,
        UnixMillisV1::new(1_500),
        UnixMillisV1::new(2_000),
    )
    .unwrap();
    assert_eq!(refusal.dimension(), StateQuotaDimensionV2::OpenSessions);
    refusal
        .validate_at(UnixMillisV1::new(1_750), &AcceptAll)
        .unwrap();
    assert!(StateCapacityRefusalV2::new(
        digest(60),
        "state-node",
        GenerationV1::new(4).unwrap(),
        GenerationV1::new(10).unwrap(),
        digest(61),
        StateQuotaDimensionV2::OpenSessions,
        1,
        1,
        2,
        UnixMillisV1::new(1_500),
        UnixMillisV1::new(2_000),
    )
    .is_err());
}

#[test]
fn state_record_deserialization_revalidates_quota_and_pinning_invariants() {
    let invalid_limits = br#"{
        "max_open_sessions":2,
        "max_actors_per_session":3,
        "max_snapshot_bytes_per_actor":301,
        "max_state_bytes_per_session":300,
        "max_state_bytes_total":500
    }"#;
    assert!(serde_json::from_slice::<StateQuotaLimitsV2>(invalid_limits).is_err());
    assert!(StateReservationV2::new(0, 0, 0).is_err());
    assert!(StateReservationV2::new(2, 100, 199).is_err());

    assert!(matches!(
        ExternalPinnedStateManifestV2::new(
            digest(70),
            "pinned-node",
            GenerationV1::new(2).unwrap(),
            digest(71),
            digest(72),
            digest(73),
            [],
            0,
            UnixMillisV1::new(10),
        ),
        Err(PlacementValidationError::EmptyPinnedStateResources)
    ));

    let left = ExternalPinnedStateManifestV2::new(
        digest(70),
        "pinned-node",
        GenerationV1::new(2).unwrap(),
        digest(71),
        digest(72),
        digest(73),
        [digest(75), digest(74)],
        512,
        UnixMillisV1::new(10),
    )
    .unwrap();
    let right = ExternalPinnedStateManifestV2::new(
        digest(70),
        "pinned-node",
        GenerationV1::new(2).unwrap(),
        digest(71),
        digest(72),
        digest(73),
        [digest(74), digest(75)],
        512,
        UnixMillisV1::new(10),
    )
    .unwrap();
    assert_eq!(
        left.semantic_digest().unwrap(),
        right.semantic_digest().unwrap()
    );
    left.validate_authentication(&AcceptAll).unwrap();
}

#[test]
fn stateless_checkpoint_is_an_explicit_empty_canonical_payload() {
    let session =
        StateSessionIdV2::new("checkpoint-node", GenerationV1::new(3).unwrap(), digest(80))
            .unwrap();
    let checkpoint = StateCheckpointV2::new(
        session,
        digest(81),
        digest(82),
        GenerationV1::new(1).unwrap(),
        StateCheckpointPayloadV2::Stateless,
        UnixMillisV1::new(1_000),
    );
    assert!(matches!(
        checkpoint.payload(),
        StateCheckpointPayloadV2::Stateless
    ));
    assert_eq!(
        String::from_utf8(checkpoint.canonical_bytes().unwrap()).unwrap(),
        concat!(
            "{\"session\":{\"node_id\":\"checkpoint-node\",\"node_generation\":3,",
            "\"session_nonce\":\"5050505050505050505050505050505050505050505050505050505050505050\"},",
            "\"actor_generation\":\"5151515151515151515151515151515151515151515151515151515151515151\",",
            "\"backend_implementation\":\"5252525252525252525252525252525252525252525252525252525252525252\",",
            "\"checkpoint_generation\":1,\"payload\":{\"kind\":\"stateless\"},\"captured_at\":1000}"
        )
    );
}

fn target(
    node: &str,
    display: &str,
    architecture: &str,
    capabilities: Vec<CapabilityAtomV1>,
    backend: BackendImplementationIdV1,
) -> TargetDescriptorV1 {
    TargetDescriptorV1::new(
        node,
        display,
        GenerationV1::new(1).unwrap(),
        TargetCapabilityModelV1::DownwardClosedIdeal,
        PlatformDescriptorV1::new("linux", architecture, "gnu", EndiannessV1::Little, 64).unwrap(),
        capabilities,
        Vec::<String>::new(),
        [backend],
    )
    .unwrap()
}

#[test]
fn requirement_join_is_aci_and_unknown_never_joins_away() {
    let a = RequirementFootprintV1::complete([RequirementAtomV1::Capability(capability(
        "semantic",
        "vector-reduce",
        1,
    ))]);
    let b = RequirementFootprintV1::complete([RequirementAtomV1::architecture("aarch64").unwrap()]);
    let unknown = RequirementFootprintV1::conservative_unknown(
        [RequirementAtomV1::portable_value_kind("integer").unwrap()],
        ["host callback footprint was not closed".to_owned()],
    )
    .unwrap();
    let impossible =
        RequirementFootprintV1::unsatisfiable([], ["conflicting exact backends".to_owned()])
            .unwrap();

    let samples = [&a, &b, &unknown, &impossible];
    for x in samples {
        assert_eq!(x.join(x), x.clone(), "idempotence");
        assert_eq!(
            x.join(&RequirementFootprintV1::empty()),
            x.clone(),
            "identity"
        );
        for y in samples {
            assert_eq!(x.join(y), y.join(x), "commutativity");
            for z in samples {
                assert_eq!(x.join(y).join(z), x.join(&y.join(z)), "associativity");
            }
        }
    }
    assert!(a.join(&unknown).is_conservative_unknown());
    assert!(unknown.join(&impossible).is_unsatisfiable());
    assert!(matches!(
        unknown.require_complete(),
        Err(PlacementValidationError::ConservativeUnknown(_))
    ));
}

#[test]
fn eligibility_does_not_factor_through_architecture_name() {
    let implementation = backend(10);
    let sve = capability("cpu", "sve", 1);
    let semantic_reduction = capability("semantic", "width-agnostic-vector-reduction", 1);
    let aarch64_sve = target(
        "arm-sve",
        "ARM with SVE",
        "aarch64",
        vec![sve.clone(), semantic_reduction.clone()],
        implementation.clone(),
    );
    let aarch64_scalar = target(
        "arm-scalar",
        "ARM scalar",
        "aarch64",
        vec![],
        implementation.clone(),
    );
    assert!(aarch64_sve.supports_capability(&sve));
    assert!(!aarch64_scalar.supports_capability(&sve));

    let x86_vector = target(
        "x86-vector",
        "x86 vector",
        "x86_64",
        vec![semantic_reduction.clone()],
        implementation,
    );
    assert!(aarch64_sve.supports_capability(&semantic_reduction));
    assert!(x86_vector.supports_capability(&semantic_reduction));
    assert_ne!(
        aarch64_sve.platform().architecture(),
        x86_vector.platform().architecture()
    );
}

#[test]
fn codegen_cache_projection_ignores_node_name_and_capacity_identity() {
    let implementation = backend(20);
    let operation_capability = capability("semantic", "integer-add", 1);
    let first = target(
        "node-a",
        "first display name",
        "aarch64",
        vec![operation_capability.clone()],
        implementation.clone(),
    );
    let second = target(
        "node-b",
        "second display name",
        "aarch64",
        vec![operation_capability.clone()],
        implementation.clone(),
    );
    let footprint =
        RequirementFootprintV1::complete([RequirementAtomV1::Capability(operation_capability)]);
    assert_eq!(
        first
            .codegen_projection_digest(&footprint, &implementation)
            .unwrap(),
        second
            .codegen_projection_digest(&footprint, &implementation)
            .unwrap()
    );
}

#[test]
fn exact_warrant_scope_and_discovered_negative_are_enforced() {
    let now = UnixMillisV1::new(1_000);
    let operation = artifact(31);
    let other_operation = artifact(32);
    let target_digest = digest(33);
    let implementation = digest(34);
    let pipeline = digest(35);
    let input_class = digest(36);
    let requirement = RequirementAtomV1::Capability(capability("semantic", "integer-add", 1));
    let footprint = RequirementFootprintV1::complete([requirement.clone()]);
    let exact_scope = WarrantScopeV1::exact(
        operation.clone(),
        target_digest.clone(),
        implementation,
        pipeline,
        input_class,
    );
    let static_warrant = PlacementWarrantV1::new(
        digest(40),
        WarrantTierV1::StaticFootprint,
        WarrantScopeV1::new(Some(operation), None, None, None, None),
        WarrantAssertionV1::OperationRequires(requirement.clone()),
        None,
        UnixMillisV1::new(100),
        None,
    )
    .unwrap();
    let positive = PlacementWarrantV1::new(
        digest(41),
        WarrantTierV1::ProviderDeclared,
        WarrantScopeV1::new(None, Some(target_digest.clone()), None, None, None),
        WarrantAssertionV1::TargetSupports(requirement.clone()),
        None,
        UnixMillisV1::new(900),
        Some(UnixMillisV1::new(1_100)),
    )
    .unwrap();
    let negative = PlacementWarrantV1::new(
        digest(42),
        WarrantTierV1::RuntimeDiscovered,
        WarrantScopeV1::new(None, Some(target_digest), None, None, None),
        WarrantAssertionV1::TargetRejects(requirement.clone()),
        None,
        UnixMillisV1::new(950),
        Some(UnixMillisV1::new(1_050)),
    )
    .unwrap();
    let entries = BTreeMap::from([(
        requirement.clone(),
        DischargedRequirementV1::new(static_warrant.id().unwrap(), positive.id().unwrap()),
    )]);
    let discharge = WarrantDischargeV1::new(exact_scope, entries).unwrap();

    assert!(matches!(
        discharge.validate(
            &footprint,
            &[static_warrant.clone(), positive.clone()],
            &PlacementTrustPolicyV1::strict(),
            now,
            &AcceptAll,
        ),
        Err(PlacementValidationError::WarrantTierNotAllowed(_))
    ));
    assert!(matches!(
        discharge.validate(
            &footprint,
            &[static_warrant.clone(), positive.clone(), negative],
            &PlacementTrustPolicyV1::declared(),
            now,
            &AcceptAll,
        ),
        Err(PlacementValidationError::NegativeVeto(_))
    ));

    let wrong_scope = WarrantScopeV1::exact(
        other_operation,
        digest(33),
        digest(34),
        digest(35),
        digest(36),
    );
    let wrong_discharge = WarrantDischargeV1::new(
        wrong_scope,
        BTreeMap::from([(
            requirement,
            DischargedRequirementV1::new(static_warrant.id().unwrap(), positive.id().unwrap()),
        )]),
    )
    .unwrap();
    assert!(matches!(
        wrong_discharge.validate(
            &footprint,
            &[static_warrant, positive],
            &PlacementTrustPolicyV1::declared(),
            now,
            &AcceptAll,
        ),
        Err(PlacementValidationError::ScopeMismatch { .. })
    ));
}

#[test]
fn lease_cannot_be_substituted_across_operation_or_attempt() {
    let reservation = PlacementReservationV1::new(1, 1024, 0).unwrap();
    let expectation = LeaseExpectationV1::new(
        "node-a",
        digest(50),
        GenerationV1::new(2).unwrap(),
        GenerationV1::new(3).unwrap(),
        artifact(51),
        digest(52),
        digest(53),
        TaskAttemptIdV1::new(digest(54), GenerationV1::new(1).unwrap()),
        digest(55),
        digest(56),
        reservation.clone(),
    )
    .unwrap();
    let lease = PlacementLeaseV1::new(
        digest(57),
        digest(58),
        expectation.clone(),
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(2_000),
    )
    .unwrap();
    assert!(lease
        .validate_for(&expectation, UnixMillisV1::new(1_500), &AcceptAll)
        .is_ok());

    let substituted = LeaseExpectationV1::new(
        "node-a",
        digest(50),
        GenerationV1::new(2).unwrap(),
        GenerationV1::new(3).unwrap(),
        artifact(99),
        digest(52),
        digest(53),
        TaskAttemptIdV1::new(digest(54), GenerationV1::new(2).unwrap()),
        digest(55),
        digest(56),
        reservation,
    )
    .unwrap();
    assert!(matches!(
        lease.validate_for(&substituted, UnixMillisV1::new(1_500), &AcceptAll),
        Err(PlacementValidationError::ScopeMismatch { .. })
    ));
}

#[test]
fn placement_lease_v2_authenticates_every_authority_layer() {
    let state = LeaseStateBindingV2::open(digest(102), StateReservationV2::new(1, 64, 64).unwrap());
    let expectation = lease_expectation_v2(state.clone());
    let issuer = digest(103);
    let lease = PlacementLeaseV2::new(
        issuer.clone(),
        digest(104),
        expectation.clone(),
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(2_000),
    )
    .unwrap();
    let authentication = ExactAuthentication {
        issuer: issuer.clone(),
        record: lease.semantic_digest().unwrap(),
    };

    lease
        .validate_for(&expectation, UnixMillisV1::new(1_500), &authentication)
        .unwrap();
    assert_eq!(lease.issuer_key(), &issuer);
    assert_eq!(lease.lease_nonce(), &digest(104));
    assert_eq!(lease.node_id(), "lease-v2-node");
    assert_eq!(lease.target_descriptor(), &digest(90));
    assert_eq!(lease.profile_generation().get(), 2);
    assert_eq!(lease.capacity_generation().get(), 3);
    assert_eq!(lease.capacity_observation(), &digest(91));
    assert_eq!(lease.candidate_eligibility(), &digest(92));
    assert_eq!(lease.operation_oir(), &artifact(93));
    assert_eq!(lease.requirement_footprint(), &digest(94));
    assert_eq!(lease.warrant_discharge(), &digest(95));
    assert_eq!(lease.admission(), &digest(96));
    assert_eq!(lease.task_attempt().task(), &digest(97));
    assert_eq!(lease.backend_implementation(), &digest(98));
    assert_eq!(lease.realization_pipeline(), &digest(99));
    assert_eq!(lease.trust_policy(), &digest(100));
    assert_eq!(lease.reservation().cpu_slots(), 2);
    assert_eq!(lease.hosted_command_binding(), &digest(101));
    assert_eq!(lease.state_binding(), &state);
    assert!(lease.one_use());
    assert_eq!(lease.issued_at(), UnixMillisV1::new(1_000));
    assert_eq!(lease.expires_at(), UnixMillisV1::new(2_000));

    assert!(matches!(
        lease.validate_for(&expectation, UnixMillisV1::new(1_500), &RejectAll),
        Err(PlacementValidationError::Unauthenticated {
            record: "placement lease v2"
        })
    ));
    assert!(matches!(
        lease.validate_for(&expectation, UnixMillisV1::new(2_000), &authentication),
        Err(PlacementValidationError::Expired {
            record: "placement lease v2"
        })
    ));
}

#[test]
fn placement_lease_v2_rejects_substitution_of_every_expected_binding() {
    let expectation = lease_expectation_v2(LeaseStateBindingV2::open(
        digest(102),
        StateReservationV2::new(1, 64, 64).unwrap(),
    ));
    let lease = PlacementLeaseV2::new(
        digest(103),
        digest(104),
        expectation.clone(),
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(2_000),
    )
    .unwrap();

    let baseline = serde_json::to_value(&expectation).unwrap();
    let replacement_digest = serde_json::Value::String(digest(200).to_string());
    let replacements = [
        ("node_id", serde_json::json!("other-node")),
        ("target_descriptor", replacement_digest.clone()),
        ("profile_generation", serde_json::json!(20)),
        ("capacity_generation", serde_json::json!(21)),
        ("capacity_observation", replacement_digest.clone()),
        ("candidate_eligibility", replacement_digest.clone()),
        (
            "operation_oir",
            serde_json::Value::String(artifact(200).as_sha256().to_owned()),
        ),
        ("requirement_footprint", replacement_digest.clone()),
        ("warrant_discharge", replacement_digest.clone()),
        ("admission", replacement_digest.clone()),
        (
            "task_attempt",
            serde_json::json!({
                "task": digest(200).as_sha256(),
                "attempt": 2,
            }),
        ),
        ("backend_implementation", replacement_digest.clone()),
        ("realization_pipeline", replacement_digest.clone()),
        ("trust_policy", replacement_digest.clone()),
        (
            "reservation",
            serde_json::json!({
                "cpu_slots": 3,
                "memory_bytes": 2048,
                "scratch_bytes": 512,
            }),
        ),
        ("hosted_command_binding", replacement_digest),
        ("state_binding", serde_json::json!({"kind": "none"})),
    ];

    for (field, replacement) in replacements {
        let mut substituted = baseline.clone();
        substituted
            .as_object_mut()
            .unwrap()
            .insert(field.to_owned(), replacement);
        let substituted: LeaseExpectationV2 = serde_json::from_value(substituted).unwrap();
        assert!(
            matches!(
                lease.validate_for(&substituted, UnixMillisV1::new(1_500), &AcceptAll),
                Err(PlacementValidationError::ScopeMismatch { .. })
            ),
            "field {field} was not compared exactly"
        );
    }
}

#[test]
fn placement_lease_v2_deserialization_rechecks_one_use_window_and_state_scope() {
    let existing = LeaseStateBindingV2::existing(
        StateSessionIdV2::new("lease-v2-node", GenerationV1::new(7).unwrap(), digest(105)).unwrap(),
        Some(digest(106)),
    );
    assert_eq!(existing.actor_generation(), Some(&digest(106)));
    assert_eq!(existing.session().unwrap().node_id(), "lease-v2-node");
    let expectation = lease_expectation_v2(existing);
    let lease = PlacementLeaseV2::new(
        digest(103),
        digest(104),
        expectation.clone(),
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(31_000),
    )
    .unwrap();
    let encoded = serde_json::to_vec(&lease).unwrap();
    let decoded: PlacementLeaseV2 = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, lease);
    assert_eq!(
        decoded.semantic_digest().unwrap(),
        lease.semantic_digest().unwrap()
    );

    let mut reusable = serde_json::to_value(&lease).unwrap();
    reusable["one_use"] = serde_json::json!(false);
    assert!(serde_json::from_value::<PlacementLeaseV2>(reusable).is_err());

    let mut too_long = serde_json::to_value(&lease).unwrap();
    too_long["expires_at"] = serde_json::json!(31_001);
    assert!(serde_json::from_value::<PlacementLeaseV2>(too_long).is_err());

    let mut wrong_node = serde_json::to_value(&expectation).unwrap();
    wrong_node["state_binding"]["session"]["node_id"] = serde_json::json!("other-node");
    assert!(serde_json::from_value::<LeaseExpectationV2>(wrong_node).is_err());

    let mut unknown_field = serde_json::to_value(&expectation).unwrap();
    unknown_field["unbound_future_field"] = serde_json::json!(true);
    assert!(serde_json::from_value::<LeaseExpectationV2>(unknown_field).is_err());

    let stateless_existing = LeaseStateBindingV2::existing(
        StateSessionIdV2::new("lease-v2-node", GenerationV1::new(8).unwrap(), digest(107)).unwrap(),
        None,
    );
    assert_eq!(stateless_existing.actor_generation(), None);
    lease_expectation_v2(stateless_existing);
}

#[test]
fn placement_lease_v2_nonce_is_covered_by_the_authenticated_digest() {
    let expectation = lease_expectation_v2(LeaseStateBindingV2::None);
    let lease = PlacementLeaseV2::new(
        digest(103),
        digest(104),
        expectation.clone(),
        UnixMillisV1::new(1_000),
        UnixMillisV1::new(2_000),
    )
    .unwrap();
    let authentication = ExactAuthentication {
        issuer: digest(103),
        record: lease.semantic_digest().unwrap(),
    };
    let mut substituted = serde_json::to_value(&lease).unwrap();
    substituted["lease_nonce"] = serde_json::json!(digest(105).as_sha256());
    let substituted: PlacementLeaseV2 = serde_json::from_value(substituted).unwrap();
    assert!(matches!(
        substituted.validate_for(&expectation, UnixMillisV1::new(1_500), &authentication),
        Err(PlacementValidationError::Unauthenticated {
            record: "placement lease v2"
        })
    ));
}

#[test]
fn complete_candidate_requires_fresh_profile_capacity_and_exact_discharge() {
    let now = UnixMillisV1::new(10_000);
    let operation = artifact(70);
    let implementation = backend(71);
    let implementation_digest = implementation.semantic_digest().unwrap();
    let pipeline = implementation.realization_pipeline().clone();
    let semantic_capability = capability("semantic", "integer-add", 1);
    let target = target(
        "node-live",
        "live target",
        "aarch64",
        vec![semantic_capability.clone()],
        implementation,
    );
    let target_digest = target.semantic_digest().unwrap();
    let profile = NodeProfileV1::new(
        digest(80),
        target,
        GenerationV1::new(2).unwrap(),
        UnixMillisV1::new(9_000),
        UnixMillisV1::new(11_000),
    )
    .unwrap();
    let capacity = CapacityObservationV1::new(
        digest(81),
        "node-live",
        target_digest.clone(),
        GenerationV1::new(2).unwrap(),
        GenerationV1::new(4).unwrap(),
        8,
        4,
        16_384,
        8_192,
        4_096,
        4_096,
        UnixMillisV1::new(9_900),
        UnixMillisV1::new(10_500),
    )
    .unwrap();
    let requirement = RequirementAtomV1::Capability(semantic_capability);
    let footprint = RequirementFootprintV1::complete([requirement.clone()]);
    let scope = WarrantScopeV1::exact(
        operation.clone(),
        target_digest.clone(),
        implementation_digest,
        pipeline,
        digest(82),
    );
    let static_warrant = PlacementWarrantV1::new(
        digest(83),
        WarrantTierV1::StaticFootprint,
        WarrantScopeV1::new(Some(operation), None, None, None, None),
        WarrantAssertionV1::OperationRequires(requirement.clone()),
        None,
        UnixMillisV1::new(8_000),
        None,
    )
    .unwrap();
    let target_warrant = PlacementWarrantV1::new(
        digest(84),
        WarrantTierV1::RuntimeDiscovered,
        WarrantScopeV1::new(None, Some(target_digest), None, None, None),
        WarrantAssertionV1::TargetSupports(requirement.clone()),
        None,
        UnixMillisV1::new(9_900),
        Some(UnixMillisV1::new(10_500)),
    )
    .unwrap();
    let discharge = WarrantDischargeV1::new(
        scope,
        BTreeMap::from([(
            requirement,
            DischargedRequirementV1::new(
                static_warrant.id().unwrap(),
                target_warrant.id().unwrap(),
            ),
        )]),
    )
    .unwrap();
    let reservation = PlacementReservationV1::new(1, 1_024, 0).unwrap();
    let warrants = [static_warrant, target_warrant];
    let strict = PlacementTrustPolicyV1::strict();
    let input = PlacementCandidateInputV1 {
        profile: &profile,
        capacity: &capacity,
        footprint: &footprint,
        discharge: &discharge,
        warrants: &warrants,
        trust_policy: &strict,
        reservation: &reservation,
        actor_generation: None,
        prospective_logical_environment: None,
    };
    assert!(input.evaluate(now, &AcceptAll).is_eligible());
    assert!(!input
        .evaluate(UnixMillisV1::new(11_000), &AcceptAll)
        .is_eligible());
}
