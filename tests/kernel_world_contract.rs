use o_lang::kernel_world::{
    ExecutionMechanism, ExportPlane, FirmwareContract, InspectedNativeKernelWorldRecord,
    InstanceState, IntegrationMode, KernelImage, KernelWorldError, KernelWorldInstance,
    KernelWorldManifest, LicenseContract, LifecycleContract, MachineContract, RedistributionPolicy,
    RequestTerminal, ResourceQuotas, RestartPolicy, VerifiedKernelWorld, WorldCapabilityRequest,
    WorldExport, KERNEL_WORLD_CONTROL_PROTOCOL_V1, KERNEL_WORLD_RUNTIME_KIND,
    KERNEL_WORLD_SCHEMA_V1, MAX_NATIVE_KERNEL_WORLD_EXPORTS, NATIVE_KERNEL_WORLD_RECORD_V1,
    NATIVE_KERNEL_WORLD_RECORD_V2,
};
use o_lang::live_system::manifest::{
    payload_sha256, BuildManifest, CapabilityRequestManifest, HealthManifest, PackageDigest,
    PackageManifest, RuntimeManifest, ServiceManifest, VerifiedPackage, PACKAGE_SCHEMA_V1,
};
use sha2::{Digest, Sha256};
use std::fs;

use o_lang::world::{
    DomainGeneration, DomainId, DomainIdentity, NodeGeneration, NodeId, NodeIdentity, WorldId,
    WorldIdentityError,
};

fn binary_manifest() -> KernelWorldManifest {
    KernelWorldManifest {
        schema: KERNEL_WORLD_SCHEMA_V1.into(),
        name: "kernel/linux-driver".into(),
        version: "1.0.0".into(),
        integration: IntegrationMode::BinaryContained,
        image: KernelImage::UserSupplied {
            expected_sha256: "a".repeat(64),
        },
        machine: MachineContract {
            guest_architecture: "x86_64".into(),
            profile: "o-machine-pc/v1".into(),
            execution: ExecutionMechanism::HardwareVirtualized,
            firmware: FirmwareContract::Uefi,
            min_vcpus: 1,
            max_vcpus: 4,
            min_memory_mib: 512,
            max_memory_mib: 4096,
            requirements: vec!["vmx".into(), "iommu".into()],
        },
        lifecycle: LifecycleContract {
            health_protocol: "ocore.kernel-world-health/v1".into(),
            health_timeout_ms: 5_000,
            restart: RestartPolicy::OnFailure,
        },
        quotas: ResourceQuotas {
            max_outstanding_requests: 2,
            max_requests_per_generation: 4,
            max_shared_memory_bytes: 64 * 1024 * 1024,
            max_devices: 1,
        },
        exports: vec![
            WorldExport {
                name: "linux.exec".into(),
                plane: ExportPlane::Abi,
                protocol: "linux.exec/v1".into(),
                authority_request: None,
            },
            WorldExport {
                name: "network.default".into(),
                plane: ExportPlane::Device,
                protocol: "o.net-port/v1".into(),
                authority_request: Some("device.net".into()),
            },
        ],
        capability_requests: vec![
            WorldCapabilityRequest {
                kind: "device.net".into(),
                rights: vec!["reset".into(), "dma".into()],
                purpose: "exclusive network provider".into(),
            },
            WorldCapabilityRequest {
                kind: "vm.machine".into(),
                rights: vec!["stop".into(), "run".into()],
                purpose: "contained guest execution".into(),
            },
        ],
        license: LicenseContract {
            redistribution: RedistributionPolicy::UserSuppliedOnly,
            external_acceptance_required: true,
        },
    }
}

fn package_digest() -> PackageDigest {
    PackageDigest::from_hex(&"b".repeat(64)).unwrap()
}

fn native_string(value: &str) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(2 + value.len());
    encoded.extend_from_slice(&(value.len() as u16).to_le_bytes());
    encoded.extend_from_slice(value.as_bytes());
    encoded
}

fn native_export(name: &str, plane: u8, protocol: &str, authority_request: &str) -> Vec<u8> {
    let mut encoded = native_string(name);
    encoded.push(plane);
    encoded.push(0);
    encoded.extend_from_slice(&native_string(protocol));
    encoded.extend_from_slice(&native_string(authority_request));
    encoded
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap_or_else(|| panic!("missing byte sequence {needle:?}"))
}

fn source_manifest(image_sha256: String) -> KernelWorldManifest {
    let mut manifest = binary_manifest();
    manifest.integration = IntegrationMode::SourceIntegrated;
    manifest.image = KernelImage::PackagePayload {
        path: "/boot/kernel.elf".into(),
        sha256: image_sha256,
    };
    manifest.machine.execution = ExecutionMechanism::Paravirtual;
    manifest.machine.firmware = FirmwareContract::Direct;
    manifest.license = LicenseContract {
        redistribution: RedistributionPolicy::Redistributable,
        external_acceptance_required: false,
    };
    manifest
}

fn package_for(
    world: &KernelWorldManifest,
    mutate: impl FnOnce(&mut PackageManifest),
) -> VerifiedPackage {
    let root = tempfile::tempdir().unwrap();
    let payload = root.path().join("payload");
    fs::create_dir(&payload).unwrap();
    fs::write(
        payload.join("kernel-world.toml"),
        world.canonical_toml().unwrap(),
    )
    .unwrap();
    if let KernelImage::PackagePayload { path, .. } = &world.image {
        let destination = payload.join(&path[1..]);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, b"pinned source-integrated kernel image").unwrap();
    }

    let mut package = PackageManifest {
        schema: PACKAGE_SCHEMA_V1.into(),
        name: world.name.clone(),
        version: world.version.clone(),
        architecture: world.machine.guest_architecture.clone(),
        payload_sha256: payload_sha256(&payload).unwrap(),
        runtime: RuntimeManifest {
            kind: KERNEL_WORLD_RUNTIME_KIND.into(),
            entry: "/kernel-world.toml".into(),
            abi: KERNEL_WORLD_CONTROL_PROTOCOL_V1.into(),
        },
        services: world
            .exports
            .iter()
            .map(|export| ServiceManifest {
                name: export.name.clone(),
                protocol: export.protocol.clone(),
            })
            .collect(),
        capability_requests: world
            .capability_requests
            .iter()
            .map(|request| CapabilityRequestManifest {
                kind: request.kind.clone(),
                rights: request.rights.clone(),
                purpose: request.purpose.clone(),
            })
            .collect(),
        health: HealthManifest {
            protocol: world.lifecycle.health_protocol.clone(),
            timeout_ms: world.lifecycle.health_timeout_ms,
        },
        build: BuildManifest {
            source_sha256: "d".repeat(64),
            builder: "kernel-world-contract-test/v1".into(),
        },
    };
    mutate(&mut package);
    VerifiedPackage::load(&package.canonical_toml().unwrap(), &payload).unwrap()
}

#[test]
fn strict_manifest_round_trips_canonically() {
    let manifest = binary_manifest();
    let canonical = manifest.canonical_toml().unwrap();
    let reparsed = KernelWorldManifest::parse_toml(&canonical).unwrap();

    assert_eq!(reparsed.canonical_toml().unwrap(), canonical);
    assert_eq!(reparsed.integration, IntegrationMode::BinaryContained);
    assert!(canonical.find("device.net").unwrap() < canonical.find("vm.machine").unwrap());
    assert!(canonical.contains("authority_request = \"device.net\""));
    assert_eq!(
        canonical.matches("authority_request =").count(),
        1,
        "non-device exports must omit the optional binding"
    );

    let with_unknown = canonical.replacen(
        "schema = \"ocore.kernel-world/v1\"",
        "schema = \"ocore.kernel-world/v1\"\nambient_host_access = true",
        1,
    );
    assert!(KernelWorldManifest::parse_toml(&with_unknown)
        .unwrap_err()
        .to_string()
        .contains("unknown field"));
}

#[test]
fn integration_and_authority_rules_fail_closed() {
    let mut source = binary_manifest();
    source.integration = IntegrationMode::SourceIntegrated;
    source.image = KernelImage::PackagePayload {
        path: "/boot/kernel.elf".into(),
        sha256: "c".repeat(64),
    };
    source.license = LicenseContract {
        redistribution: RedistributionPolicy::Redistributable,
        external_acceptance_required: false,
    };
    let error = source.validate().unwrap_err().to_string();
    assert!(error.contains("machine.execution"), "{error}");

    let mut missing_vm = binary_manifest();
    missing_vm
        .capability_requests
        .retain(|request| request.kind != "vm.machine");
    let error = missing_vm.validate().unwrap_err().to_string();
    assert!(error.contains("`vm.machine` `run` authority"), "{error}");

    let mut unusable_vm = binary_manifest();
    let vm = unusable_vm
        .capability_requests
        .iter_mut()
        .find(|request| request.kind == "vm.machine")
        .unwrap();
    vm.rights = vec!["stop".into()];
    let error = unusable_vm.validate().unwrap_err().to_string();
    assert!(error.contains("`vm.machine` `run` authority"), "{error}");

    let mut missing_binding = binary_manifest();
    missing_binding.exports[1].authority_request = None;
    let error = missing_binding.validate().unwrap_err().to_string();
    assert!(error.contains("must name an exact `device.*`"), "{error}");

    let mut unknown_binding = binary_manifest();
    unknown_binding.exports[1].authority_request = Some("device.unknown".into());
    let error = unknown_binding.validate().unwrap_err().to_string();
    assert!(error.contains("exact existing `device.*`"), "{error}");

    let mut bare_device_kind = binary_manifest();
    bare_device_kind.capability_requests[0].kind = "device.".into();
    let error = bare_device_kind.validate().unwrap_err().to_string();
    assert!(error.contains("non-empty suffix"), "{error}");

    let mut bare_device_binding = binary_manifest();
    bare_device_binding.exports[1].authority_request = Some("device.".into());
    let error = bare_device_binding.validate().unwrap_err().to_string();
    assert!(error.contains("exact existing `device.*`"), "{error}");

    let mut non_device_binding = binary_manifest();
    non_device_binding.exports[0].authority_request = Some("device.net".into());
    let error = non_device_binding.validate().unwrap_err().to_string();
    assert!(error.contains("non-device export"), "{error}");
    assert!(error.contains("must omit"), "{error}");

    let mut zero_device_quota = binary_manifest();
    zero_device_quota.quotas.max_devices = 0;
    let error = zero_device_quota.validate().unwrap_err().to_string();
    assert!(
        error.contains("distinct device authority requests"),
        "{error}"
    );
    assert!(error.contains("limit of 0"), "{error}");

    let mut unpinned = binary_manifest();
    unpinned.image = KernelImage::UserSupplied {
        expected_sha256: "latest".into(),
    };
    let error = unpinned.validate().unwrap_err().to_string();
    assert!(error.contains("64 lowercase hexadecimal"), "{error}");
}

#[test]
fn capability_request_kinds_and_reserved_rights_are_unambiguous() {
    let mut duplicate_kind = binary_manifest();
    duplicate_kind
        .capability_requests
        .push(WorldCapabilityRequest {
            kind: "device.net".into(),
            rights: vec!["reset".into()],
            purpose: "a different purpose cannot create a second key".into(),
        });
    let error = duplicate_kind.validate().unwrap_err().to_string();
    assert!(error.contains("capability_requests.kind"), "{error}");
    assert!(error.contains("device.net"), "{error}");

    let mut vm_with_device_right = binary_manifest();
    vm_with_device_right
        .capability_requests
        .iter_mut()
        .find(|request| request.kind == "vm.machine")
        .unwrap()
        .rights
        .push("dma".into());
    let error = vm_with_device_right.validate().unwrap_err().to_string();
    assert!(error.contains("vm.machine"), "{error}");
    assert!(error.contains("dma"), "{error}");

    let mut device_with_vm_right = binary_manifest();
    device_with_vm_right
        .capability_requests
        .iter_mut()
        .find(|request| request.kind == "device.net")
        .unwrap()
        .rights
        .push("run".into());
    let error = device_with_vm_right.validate().unwrap_err().to_string();
    assert!(error.contains("device.net"), "{error}");
    assert!(error.contains("run"), "{error}");

    let mut other_with_reserved_right = binary_manifest();
    other_with_reserved_right
        .capability_requests
        .push(WorldCapabilityRequest {
            kind: "service.audit".into(),
            rights: vec!["stop".into()],
            purpose: "reserved rights remain kind-specific".into(),
        });
    let error = other_with_reserved_right
        .validate()
        .unwrap_err()
        .to_string();
    assert!(error.contains("service.audit"), "{error}");
    assert!(error.contains("stop"), "{error}");
}

#[test]
fn device_quota_counts_distinct_bound_authority_requests() {
    let mut shared_authority = binary_manifest();
    shared_authority.exports.push(WorldExport {
        name: "network.control".into(),
        plane: ExportPlane::Device,
        protocol: "o.net-control/v1".into(),
        authority_request: Some("device.net".into()),
    });
    shared_authority.validate().unwrap();

    let mut distinct_authority = shared_authority;
    distinct_authority
        .capability_requests
        .push(WorldCapabilityRequest {
            kind: "device.storage".into(),
            rights: vec!["reset".into()],
            purpose: "exclusive storage provider".into(),
        });
    distinct_authority.exports.push(WorldExport {
        name: "storage.default".into(),
        plane: ExportPlane::Device,
        protocol: "o.block/v1".into(),
        authority_request: Some("device.storage".into()),
    });
    let error = distinct_authority.validate().unwrap_err().to_string();
    assert!(
        error.contains("distinct device authority requests"),
        "{error}"
    );
    assert!(error.contains("limit of 1"), "{error}");
    assert!(error.contains("got 2"), "{error}");
}

#[test]
fn health_failure_and_replacement_preserve_one_terminal_result() {
    let mut world = KernelWorldInstance::new(binary_manifest(), package_digest()).unwrap();
    assert_eq!(world.state(), InstanceState::Installed);

    let generation_one = world.start_generation().unwrap();
    assert_eq!(generation_one.generation(), 1);
    assert!(matches!(
        world.resolve_export(1, "network.default"),
        Err(KernelWorldError::InvalidState { .. })
    ));
    world.mark_healthy(1).unwrap();

    let provenance = world.resolve_export(1, "network.default").unwrap();
    assert_eq!(provenance.world.name(), "kernel/linux-driver");
    assert_eq!(provenance.world.package_digest().as_hex(), "b".repeat(64));
    assert_eq!(provenance.world.generation(), 1);
    assert_eq!(provenance.integration, IntegrationMode::BinaryContained);
    assert_eq!(provenance.export.plane, ExportPlane::Device);

    let cancelled = world.begin_request(1, "network.default").unwrap();
    let service_death = world.begin_request(1, "linux.exec").unwrap();
    assert!(matches!(
        world.begin_request(1, "linux.exec"),
        Err(KernelWorldError::OutstandingRequestLimit)
    ));

    assert_eq!(
        world.cancel(cancelled).unwrap().terminal,
        RequestTerminal::Cancelled
    );
    assert!(matches!(
        world.reply(cancelled),
        Err(KernelWorldError::RequestAlreadyTerminal {
            terminal: RequestTerminal::Cancelled,
            ..
        })
    ));

    let failed = world.fail_generation(1).unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].request, service_death);
    assert_eq!(failed[0].terminal, RequestTerminal::WorldFailed);
    assert_eq!(world.state(), InstanceState::Failed);

    let generation_two = world.start_generation().unwrap();
    assert_eq!(generation_two.generation(), 2);
    world.mark_healthy(2).unwrap();
    assert!(matches!(
        world.reply(service_death),
        Err(KernelWorldError::StaleGeneration {
            expected: 2,
            got: 1
        })
    ));

    let fresh = world.begin_request(2, "network.default").unwrap();
    assert_eq!(fresh.generation(), 2);
    assert_eq!(fresh.sequence(), 1);
    assert_eq!(
        world.reply(fresh).unwrap().terminal,
        RequestTerminal::Replied
    );
}

#[test]
fn kernel_world_binding_requires_explicit_placement_and_preserves_provenance() {
    let node = NodeIdentity::new(
        WorldId::new("desk").unwrap(),
        NodeId::new("node-a").unwrap(),
        NodeGeneration::new(2).unwrap(),
    );
    let domain_id = DomainId::new("foreign-kernel-provider").unwrap();
    let allocated_domain = DomainIdentity::new(
        node.clone(),
        domain_id.clone(),
        DomainGeneration::new(41).unwrap(),
    );

    let mut instance = KernelWorldInstance::new(binary_manifest(), package_digest()).unwrap();
    let generation_one = instance.start_generation().unwrap();
    let binding_one = generation_one
        .bind_execution_domain(allocated_domain.clone())
        .unwrap();
    assert_eq!(binding_one.domain().node(), &node);
    assert_eq!(binding_one.domain().domain(), &domain_id);
    assert_eq!(binding_one.domain().generation().get(), 41);
    assert_eq!(binding_one.provider_generation(), 1);
    assert_eq!(binding_one.kernel_world_name(), "kernel/linux-driver");
    assert_eq!(binding_one.package().as_sha256(), "b".repeat(64));

    instance.fail_generation(1).unwrap();
    let generation_two = instance.start_generation().unwrap();
    let binding_two = generation_two
        .bind_execution_domain(allocated_domain)
        .unwrap();
    assert_eq!(binding_two.domain().generation().get(), 41);
    assert_eq!(binding_two.provider_generation(), 2);
    assert_eq!(binding_two.domain(), binding_one.domain());

    let replacement_domain =
        DomainIdentity::new(node, domain_id, DomainGeneration::new(42).unwrap());
    let replacement_binding = generation_two
        .bind_execution_domain(replacement_domain)
        .unwrap();
    assert!(matches!(
        replacement_binding
            .domain()
            .require_current(binding_one.domain()),
        Err(WorldIdentityError::StaleGeneration {
            kind: "domain generation",
            expected: 42,
            got: 41
        })
    ));
}

#[test]
fn request_history_is_bounded_per_generation() {
    let mut manifest = binary_manifest();
    manifest.lifecycle.restart = RestartPolicy::Always;
    manifest.quotas.max_outstanding_requests = 1;
    manifest.quotas.max_requests_per_generation = 2;
    let mut world = KernelWorldInstance::new(manifest, package_digest()).unwrap();
    world.start_generation().unwrap();
    world.mark_healthy(1).unwrap();

    let first = world.begin_request(1, "linux.exec").unwrap();
    world.reply(first).unwrap();
    let second = world.begin_request(1, "linux.exec").unwrap();
    world.timeout(second).unwrap();
    assert!(matches!(
        world.begin_request(1, "linux.exec"),
        Err(KernelWorldError::GenerationRequestLimit)
    ));

    world.stop_generation(1).unwrap();
    world.start_generation().unwrap();
    world.mark_healthy(2).unwrap();
    assert_eq!(world.begin_request(2, "linux.exec").unwrap().sequence(), 1);
}

#[test]
fn replacement_obeys_declared_restart_policy() {
    let mut never = binary_manifest();
    never.lifecycle.restart = RestartPolicy::Never;
    let mut world = KernelWorldInstance::new(never, package_digest()).unwrap();
    world.start_generation().unwrap();
    world.mark_healthy(1).unwrap();
    world.fail_generation(1).unwrap();
    assert!(matches!(
        world.start_generation(),
        Err(KernelWorldError::InvalidState {
            state: InstanceState::Failed,
            ..
        })
    ));

    let mut on_failure = binary_manifest();
    on_failure.lifecycle.restart = RestartPolicy::OnFailure;
    let mut world = KernelWorldInstance::new(on_failure, package_digest()).unwrap();
    world.start_generation().unwrap();
    world.mark_healthy(1).unwrap();
    world.stop_generation(1).unwrap();
    assert!(matches!(
        world.start_generation(),
        Err(KernelWorldError::InvalidState {
            state: InstanceState::Stopped,
            ..
        })
    ));
}

#[test]
fn verified_package_binding_is_exact_and_content_addressed() {
    let world = binary_manifest();
    let package = package_for(&world, |_| {});
    let bound = VerifiedKernelWorld::from_package(&package).unwrap();

    assert_eq!(
        bound.manifest().canonical_toml().unwrap(),
        world.canonical_toml().unwrap()
    );
    assert_eq!(bound.package_digest(), package.digest());
    let mut instance = bound.into_instance().unwrap();
    let identity = instance.start_generation().unwrap();
    assert_eq!(identity.package_digest(), package.digest());

    let missing_export = package_for(&world, |package| {
        package.services.pop();
    });
    let error = VerifiedKernelWorld::from_package(&missing_export)
        .unwrap_err()
        .to_string();
    assert!(error.contains("services must exactly match"), "{error}");

    let weaker_authority = package_for(&world, |package| {
        package.capability_requests[0].rights.pop();
    });
    let error = VerifiedKernelWorld::from_package(&weaker_authority)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("capability requests must exactly match"),
        "{error}"
    );
}

#[test]
fn package_payload_kernel_image_is_hash_verified() {
    let image_sha256 = hex::encode(Sha256::digest(b"pinned source-integrated kernel image"));
    let world = source_manifest(image_sha256);
    let package = package_for(&world, |_| {});
    VerifiedKernelWorld::from_package(&package).unwrap();

    let wrong_digest = source_manifest("e".repeat(64));
    let package = package_for(&wrong_digest, |_| {});
    let error = VerifiedKernelWorld::from_package(&package)
        .unwrap_err()
        .to_string();
    assert!(error.contains("SHA-256 mismatch"), "{error}");
}

#[test]
fn verified_world_encodes_a_canonical_native_admission_record() {
    let manifest = binary_manifest();
    let package = package_for(&manifest, |_| {});
    let verified = VerifiedKernelWorld::from_package(&package).unwrap();

    let first = verified.encode_native_record().unwrap();
    let second = verified.encode_native_record().unwrap();
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(&first.bytes()[0..8], b"OKWORLD1");
    assert_eq!(
        u16::from_le_bytes(first.bytes()[8..10].try_into().unwrap()),
        NATIVE_KERNEL_WORLD_RECORD_V2
    );
    assert_eq!(
        u32::from_le_bytes(first.bytes()[12..16].try_into().unwrap()) as usize,
        first.bytes().len()
    );
    assert_eq!(first.sha256_hex().len(), 64);

    let decoded: InspectedNativeKernelWorldRecord =
        InspectedNativeKernelWorldRecord::from_bytes(first.bytes()).unwrap();
    assert_eq!(decoded.package_digest(), package.digest());
    assert_eq!(
        decoded.manifest().canonical_toml().unwrap(),
        manifest.canonical_toml().unwrap()
    );
    assert_eq!(decoded.sha256_hex(), first.sha256_hex());
}

#[test]
fn native_admission_record_rejects_tamper_and_noncanonical_headers() {
    let manifest = binary_manifest();
    let package = package_for(&manifest, |_| {});
    let record = VerifiedKernelWorld::from_package(&package)
        .unwrap()
        .encode_native_record()
        .unwrap();

    let mut tampered = record.bytes().to_vec();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(matches!(
        InspectedNativeKernelWorldRecord::from_bytes(&tampered),
        Err(KernelWorldError::InvalidNativeRecord { .. })
            | Err(KernelWorldError::InvalidField { .. })
            | Err(KernelWorldError::Duplicate { .. })
    ));

    let mut wrong_version = record.bytes().to_vec();
    wrong_version[8..10].copy_from_slice(&NATIVE_KERNEL_WORLD_RECORD_V1.to_le_bytes());
    assert!(matches!(
        InspectedNativeKernelWorldRecord::from_bytes(&wrong_version),
        Err(KernelWorldError::InvalidNativeRecord { .. })
    ));

    let device_export = native_export("network.default", 1, "o.net-port/v1", "device.net");
    let device_export_offset = find_bytes(record.bytes(), &device_export);
    let authority_offset = device_export_offset + device_export.len() - "device.net".len();
    let mut forged_authority = record.bytes().to_vec();
    forged_authority[authority_offset..authority_offset + "device.net".len()]
        .copy_from_slice(b"device.bad");
    let error = InspectedNativeKernelWorldRecord::from_bytes(&forged_authority)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("exact existing `device.*`")
            || error.contains("canonical manifest digest mismatch"),
        "{error}"
    );

    let mut wrong_length = record.bytes().to_vec();
    wrong_length[12..16].copy_from_slice(&1u32.to_le_bytes());
    assert!(matches!(
        InspectedNativeKernelWorldRecord::from_bytes(&wrong_length),
        Err(KernelWorldError::InvalidNativeRecord { .. })
    ));
}

#[test]
fn native_inspection_rejects_zero_digest_and_reordered_canonical_tuples() {
    let manifest = binary_manifest();
    let package = package_for(&manifest, |_| {});
    let record = VerifiedKernelWorld::from_package(&package)
        .unwrap()
        .encode_native_record()
        .unwrap();

    let mut zero_digest = record.bytes().to_vec();
    zero_digest[16..48].fill(0);
    let error = InspectedNativeKernelWorldRecord::from_bytes(&zero_digest)
        .unwrap_err()
        .to_string();
    assert!(error.contains("all-zero sentinel"), "{error}");

    let first = native_export("linux.exec", 2, "linux.exec/v1", "");
    let second = native_export("network.default", 1, "o.net-port/v1", "device.net");
    let first_offset = find_bytes(record.bytes(), &first);
    let second_offset = find_bytes(record.bytes(), &second);
    assert_eq!(first_offset + first.len(), second_offset);

    let mut reordered = Vec::with_capacity(record.bytes().len());
    reordered.extend_from_slice(&record.bytes()[..first_offset]);
    reordered.extend_from_slice(&second);
    reordered.extend_from_slice(&first);
    reordered.extend_from_slice(&record.bytes()[second_offset + second.len()..]);
    assert_eq!(reordered.len(), record.bytes().len());

    let error = InspectedNativeKernelWorldRecord::from_bytes(&reordered)
        .unwrap_err()
        .to_string();
    assert!(error.contains("record is not canonical"), "{error}");
}

#[test]
fn native_inspection_rejects_reserved_unknown_trailing_and_duplicate_fields() {
    let manifest = binary_manifest();
    let package = package_for(&manifest, |_| {});
    let record = VerifiedKernelWorld::from_package(&package)
        .unwrap()
        .encode_native_record()
        .unwrap();

    let mut reserved = record.bytes().to_vec();
    reserved[10] = 1;
    let error = InspectedNativeKernelWorldRecord::from_bytes(&reserved)
        .unwrap_err()
        .to_string();
    assert!(error.contains("reserved header bits"), "{error}");

    let mut unknown_enum = record.bytes().to_vec();
    unknown_enum[80] = u8::MAX;
    let error = InspectedNativeKernelWorldRecord::from_bytes(&unknown_enum)
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid integration enum"), "{error}");

    let mut trailing = record.bytes().to_vec();
    trailing.push(0);
    let trailing_length = trailing.len() as u32;
    trailing[12..16].copy_from_slice(&trailing_length.to_le_bytes());
    let error = InspectedNativeKernelWorldRecord::from_bytes(&trailing)
        .unwrap_err()
        .to_string();
    assert!(error.contains("trailing bytes"), "{error}");

    let mut duplicate_manifest = binary_manifest();
    duplicate_manifest.machine.requirements = vec!["alpha".into(), "bravo".into()];
    let package = package_for(&duplicate_manifest, |_| {});
    let duplicate_record = VerifiedKernelWorld::from_package(&package)
        .unwrap()
        .encode_native_record()
        .unwrap();
    let second_requirement = native_string("bravo");
    let offset = find_bytes(duplicate_record.bytes(), &second_requirement);
    let mut duplicate = duplicate_record.bytes().to_vec();
    duplicate[offset + 2..offset + 7].copy_from_slice(b"alpha");
    let error = InspectedNativeKernelWorldRecord::from_bytes(&duplicate)
        .unwrap_err()
        .to_string();
    assert!(error.contains("duplicate value `alpha`"), "{error}");
}

#[test]
fn native_admission_pilot_bounds_reject_without_truncation() {
    let mut manifest = binary_manifest();
    while manifest.exports.len() <= MAX_NATIVE_KERNEL_WORLD_EXPORTS {
        let ordinal = manifest.exports.len();
        manifest.exports.push(WorldExport {
            name: format!("semantic.extra{ordinal}"),
            plane: ExportPlane::Semantic,
            protocol: format!("o.extra{ordinal}/v1"),
            authority_request: None,
        });
    }
    manifest.validate().unwrap();
    let package = package_for(&manifest, |_| {});
    let verified = VerifiedKernelWorld::from_package(&package).unwrap();
    assert!(matches!(
        verified.encode_native_record(),
        Err(KernelWorldError::LimitExceeded {
            resource: "native kernel-world exports",
            ..
        })
    ));
}

#[test]
fn native_admission_rejects_values_outside_the_native_parser_domain() {
    let mut unsupported_right = binary_manifest();
    unsupported_right
        .capability_requests
        .push(WorldCapabilityRequest {
            kind: "service.audit".into(),
            rights: vec!["audit".into()],
            purpose: "auditable non-native service".into(),
        });
    let package = package_for(&unsupported_right, |_| {});
    let error = VerifiedKernelWorld::from_package(&package)
        .unwrap()
        .encode_native_record()
        .unwrap_err();
    assert!(matches!(
        error,
        KernelWorldError::InvalidNativeRecord { .. }
    ));

    let mut non_ascii_purpose = binary_manifest();
    non_ascii_purpose.capability_requests[0].purpose = "réseau provider".into();
    let package = package_for(&non_ascii_purpose, |_| {});
    let error = VerifiedKernelWorld::from_package(&package)
        .unwrap()
        .encode_native_record()
        .unwrap_err();
    assert!(matches!(
        error,
        KernelWorldError::InvalidNativeRecord { .. }
    ));

    let mut direct_binary_entry = binary_manifest();
    direct_binary_entry.machine.firmware = FirmwareContract::Direct;
    let package = package_for(&direct_binary_entry, |_| {});
    let record = VerifiedKernelWorld::from_package(&package)
        .unwrap()
        .encode_native_record()
        .unwrap();
    assert_eq!(
        InspectedNativeKernelWorldRecord::from_bytes(record.bytes())
            .unwrap()
            .manifest()
            .machine
            .firmware,
        FirmwareContract::Direct
    );
}

#[test]
fn native_ocore_fixture_has_a_pinned_cross_language_identity() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ocore/kernel/kernel-world-fixture");
    let manifest = fs::read_to_string(fixture.join("package.toml")).unwrap();
    let package = VerifiedPackage::load(&manifest, &fixture.join("payload")).unwrap();
    let record = VerifiedKernelWorld::from_package(&package)
        .unwrap()
        .encode_native_record()
        .unwrap();

    assert_eq!(
        (
            record.bytes().len(),
            record.package_digest().as_hex(),
            record.sha256_hex(),
        ),
        (
            459,
            "be912b0cbd26ac76fb57500399907a1af214e1f7784eee59e5758bc481815a78",
            "0ece5f7f37ebe203d03cc7e5213dc8f9257a9a225a73e52d37d1f718424b9232".into(),
        ),
        "refresh the native fixture length, package digest, and record digest together"
    );
    InspectedNativeKernelWorldRecord::from_bytes(record.bytes()).unwrap();
}
