use std::collections::BTreeMap;

use o_lang::ir::BackendRegistry;
use o_lang::placement::{
    BackendImplementationIdV1, CanonicalPlacementRecordV1, CapabilityAtomV1, CapabilityKeyV1,
    CapacityObservationV1, DischargedRequirementV1, EndiannessV1, GenerationV1, LeaseExpectationV1,
    NodeProfileV1, PlacementCandidateInputV1, PlacementLeaseV1, PlacementReservationV1,
    PlacementTrustPolicyV1, PlacementValidationError, PlacementWarrantV1, PlatformDescriptorV1,
    RecordAuthenticationV1, RecordAuthenticatorV1, RequirementAtomV1, RequirementFootprintV1,
    SemanticDigestV1, TargetCapabilityModelV1, TargetDescriptorV1, TaskAttemptIdV1, UnixMillisV1,
    WarrantAssertionV1, WarrantDischargeV1, WarrantScopeV1, WarrantTierV1,
};
use o_lang::world::ArtifactId;

struct AcceptAll;

impl RecordAuthenticatorV1 for AcceptAll {
    fn authenticate(&self, _record: &RecordAuthenticationV1) -> bool {
        true
    }
}

fn digest(byte: u8) -> SemanticDigestV1 {
    SemanticDigestV1::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
}

fn artifact(byte: u8) -> ArtifactId {
    ArtifactId::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
}

fn capability(namespace: &str, name: &str, level: u32) -> CapabilityAtomV1 {
    CapabilityAtomV1::new(CapabilityKeyV1::new(namespace, name).unwrap(), level).unwrap()
}

fn backend(seed: u8) -> BackendImplementationIdV1 {
    let specification = SemanticDigestV1::from_sha256(
        BackendRegistry::global()
            .specification_sha256("python")
            .expect("the canonical Python backend has a specification digest"),
    )
    .unwrap();
    backend_with_specification(seed, specification)
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
            && current_schema == "ostadix.backend-catalog/v3"
    ));
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
    };
    assert!(input.evaluate(now, &AcceptAll).is_eligible());
    assert!(!input
        .evaluate(UnixMillisV1::new(11_000), &AcceptAll)
        .is_eligible());
}
