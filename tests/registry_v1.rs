use o_lang::placement::{
    EndiannessV1, GenerationV1, NodeProfileV1, PlatformDescriptorV1, SemanticDigestV1,
    TargetCapabilityModelV1, TargetDescriptorV1,
};
use o_lang::registry::{
    append_namespace_delegation, append_profile_publication, canonical_registry_bytes,
    create_registry_root, merge_registry_store, registry_public_key_id, verify_registry_store,
    write_new_registry_state, NamespaceDelegationV1, ProfilePublicationV1,
    ProfileStalenessPolicyV1, RegistryError, RegistryRootPinV1, RegistrySignerV1,
    RegistryStatePathsV1, RegistryStoreV1, RegistryTrustV1,
};

fn signer(seed: u8) -> RegistrySignerV1 {
    RegistrySignerV1::from_secret_bytes([seed; 32])
}

fn trust(namespace: &str, signer: &RegistrySignerV1) -> RegistryTrustV1 {
    RegistryTrustV1::new([RegistryRootPinV1::new(namespace, signer.public_key()).unwrap()]).unwrap()
}

fn profile(
    signer: &RegistrySignerV1,
    node_id: &str,
    generation: u64,
    issued_at_ms: u64,
    expires_at_ms: u64,
) -> NodeProfileV1 {
    let issuer =
        SemanticDigestV1::from_sha256(hex::encode(registry_public_key_id(&signer.public_key())))
            .unwrap();
    let descriptor = TargetDescriptorV1::new(
        node_id,
        format!("test node {node_id}"),
        GenerationV1::new(1).unwrap(),
        TargetCapabilityModelV1::DownwardClosedIdeal,
        PlatformDescriptorV1::new("linux", "aarch64", "gnu", EndiannessV1::Little, 64).unwrap(),
        [],
        Vec::<String>::new(),
        [],
    )
    .unwrap();
    NodeProfileV1::new(
        issuer,
        descriptor,
        GenerationV1::new(generation).unwrap(),
        o_lang::placement::UnixMillisV1::new(issued_at_ms),
        o_lang::placement::UnixMillisV1::new(expires_at_ms),
    )
    .unwrap()
}

#[test]
fn canonical_signed_chain_is_deterministic_and_fresh_by_default() {
    let root_signer = signer(1);
    let mut first = create_registry_root("org.ostadix", 1_000, 10_000, &root_signer).unwrap();
    let mut second = create_registry_root("org.ostadix", 1_000, 10_000, &root_signer).unwrap();
    let publication = ProfilePublicationV1::new(
        "org.ostadix",
        "node-a",
        profile(&root_signer, "node-a", 1, 1_400, 2_000),
    )
    .unwrap();
    append_profile_publication(&mut first, publication.clone(), 1_500, &root_signer).unwrap();
    append_profile_publication(&mut second, publication, 1_500, &root_signer).unwrap();
    assert_eq!(
        canonical_registry_bytes(&first).unwrap(),
        canonical_registry_bytes(&second).unwrap()
    );

    let store = RegistryStoreV1::new(first);
    let trusted = trust("org.ostadix", &root_signer);
    let verified =
        verify_registry_store(&store, &trusted, 1_600, ProfileStalenessPolicyV1::Reject).unwrap();
    assert_eq!(verified.profiles().len(), 1);
    assert!(matches!(
        verify_registry_store(&store, &trusted, 2_000, ProfileStalenessPolicyV1::Reject,),
        Err(RegistryError::StaleProfile { .. })
    ));
    let stale = verify_registry_store(
        &store,
        &trusted,
        2_000,
        ProfileStalenessPolicyV1::AllowExpired,
    )
    .unwrap();
    assert!(stale.profiles().values().all(|entry| entry.is_stale()));
}

#[test]
fn signatures_and_previous_event_chain_fail_closed() {
    let root_signer = signer(2);
    let mut snapshot = create_registry_root("org.ostadix", 1_000, 10_000, &root_signer).unwrap();
    append_namespace_delegation(
        &mut snapshot,
        NamespaceDelegationV1::new(
            "org.ostadix",
            "org.ostadix/team",
            signer(3).public_key(),
            1_500,
            9_000,
        )
        .unwrap(),
        1_400,
        &root_signer,
    )
    .unwrap();
    let trusted = trust("org.ostadix", &root_signer);

    let mut broken_chain = serde_json::to_value(RegistryStoreV1::new(snapshot.clone())).unwrap();
    broken_chain["snapshots"][0]["events"][1]["event"]["previous_event_sha256"][0] =
        serde_json::json!(255);
    let broken_chain: RegistryStoreV1 = serde_json::from_value(broken_chain).unwrap();
    assert!(matches!(
        verify_registry_store(
            &broken_chain,
            &trusted,
            2_000,
            ProfileStalenessPolicyV1::Reject,
        ),
        Err(RegistryError::PreviousEventMismatch { sequence: 2 })
    ));

    let mut broken_signature = serde_json::to_value(RegistryStoreV1::new(snapshot)).unwrap();
    broken_signature["snapshots"][0]["events"][1]["signature"][0] = serde_json::json!(255);
    let broken_signature: RegistryStoreV1 = serde_json::from_value(broken_signature).unwrap();
    assert!(matches!(
        verify_registry_store(
            &broken_signature,
            &trusted,
            2_000,
            ProfileStalenessPolicyV1::Reject,
        ),
        Err(RegistryError::InvalidSignature { sequence: 2 })
    ));
}

#[test]
fn federated_snapshot_requires_live_parent_delegation() {
    let parent_signer = signer(4);
    let child_signer = signer(5);
    let parent_without_delegation = RegistryStoreV1::new(
        create_registry_root("org.ostadix", 1_000, 10_000, &parent_signer).unwrap(),
    );
    let child = RegistryStoreV1::new(
        create_registry_root("org.ostadix/team", 1_500, 8_000, &child_signer).unwrap(),
    );
    let trusted = trust("org.ostadix", &parent_signer);
    let untrusted_merge = merge_registry_store(&parent_without_delegation, &child).unwrap();
    assert!(matches!(
        verify_registry_store(
            &untrusted_merge,
            &trusted,
            2_000,
            ProfileStalenessPolicyV1::Reject,
        ),
        Err(RegistryError::MissingDelegation { .. })
    ));

    let mut parent = create_registry_root("org.ostadix", 1_000, 10_000, &parent_signer).unwrap();
    append_namespace_delegation(
        &mut parent,
        NamespaceDelegationV1::new(
            "org.ostadix",
            "org.ostadix/team",
            child_signer.public_key(),
            1_500,
            8_000,
        )
        .unwrap(),
        1_400,
        &parent_signer,
    )
    .unwrap();
    let merged = merge_registry_store(&RegistryStoreV1::new(parent), &child).unwrap();
    let verified =
        verify_registry_store(&merged, &trusted, 2_000, ProfileStalenessPolicyV1::Reject).unwrap();
    assert_eq!(verified.verified_snapshots(), 2);
}

#[test]
fn merge_detects_snapshot_rollback_and_equivocation() {
    let root_signer = signer(6);
    let base = create_registry_root("org.ostadix", 1_000, 10_000, &root_signer).unwrap();
    let mut left = base.clone();
    append_namespace_delegation(
        &mut left,
        NamespaceDelegationV1::new(
            "org.ostadix",
            "org.ostadix/left",
            signer(7).public_key(),
            1_500,
            9_000,
        )
        .unwrap(),
        1_400,
        &root_signer,
    )
    .unwrap();
    let current = RegistryStoreV1::new(left.clone());
    assert!(matches!(
        merge_registry_store(&current, &RegistryStoreV1::new(base.clone())),
        Err(RegistryError::SnapshotRollback { .. })
    ));

    let mut right = base;
    append_namespace_delegation(
        &mut right,
        NamespaceDelegationV1::new(
            "org.ostadix",
            "org.ostadix/right",
            signer(8).public_key(),
            1_500,
            9_000,
        )
        .unwrap(),
        1_400,
        &root_signer,
    )
    .unwrap();
    assert!(matches!(
        merge_registry_store(&current, &RegistryStoreV1::new(right)),
        Err(RegistryError::Equivocation { sequence: 2, .. })
    ));
}

#[test]
fn issuer_generation_and_namespace_authority_invariants_are_enforced() {
    let root_signer = signer(10);
    let trusted = trust("org.ostadix", &root_signer);

    let mut wrong_issuer =
        create_registry_root("org.ostadix", 1_000, 10_000, &root_signer).unwrap();
    append_profile_publication(
        &mut wrong_issuer,
        ProfilePublicationV1::new(
            "org.ostadix",
            "node-a",
            profile(&signer(11), "node-a", 1, 1_300, 3_000),
        )
        .unwrap(),
        1_400,
        &root_signer,
    )
    .unwrap();
    assert!(matches!(
        verify_registry_store(
            &RegistryStoreV1::new(wrong_issuer),
            &trusted,
            1_500,
            ProfileStalenessPolicyV1::Reject,
        ),
        Err(RegistryError::ProfileIssuerMismatch)
    ));

    let mut rollback = create_registry_root("org.ostadix", 1_000, 10_000, &root_signer).unwrap();
    for (generation, event_time) in [(2, 1_400), (1, 1_600)] {
        append_profile_publication(
            &mut rollback,
            ProfilePublicationV1::new(
                "org.ostadix",
                "node-a",
                profile(&root_signer, "node-a", generation, event_time - 100, 3_000),
            )
            .unwrap(),
            event_time,
            &root_signer,
        )
        .unwrap();
    }
    assert!(matches!(
        verify_registry_store(
            &RegistryStoreV1::new(rollback),
            &trusted,
            1_700,
            ProfileStalenessPolicyV1::Reject,
        ),
        Err(RegistryError::ProfileRollback {
            current: 2,
            incoming: 1,
            ..
        })
    ));

    let mut equivocation =
        create_registry_root("org.ostadix", 1_000, 10_000, &root_signer).unwrap();
    for event_time in [1_400, 1_600] {
        append_profile_publication(
            &mut equivocation,
            ProfilePublicationV1::new(
                "org.ostadix",
                "node-a",
                profile(&root_signer, "node-a", 2, event_time - 100, 3_000),
            )
            .unwrap(),
            event_time,
            &root_signer,
        )
        .unwrap();
    }
    assert!(matches!(
        verify_registry_store(
            &RegistryStoreV1::new(equivocation),
            &trusted,
            1_700,
            ProfileStalenessPolicyV1::Reject,
        ),
        Err(RegistryError::ProfileEquivocation { generation: 2, .. })
    ));

    let team_signer = signer(12);
    let mut out_of_scope =
        create_registry_root("org.ostadix", 1_000, 10_000, &root_signer).unwrap();
    append_namespace_delegation(
        &mut out_of_scope,
        NamespaceDelegationV1::new(
            "org.ostadix",
            "org.ostadix/team-a",
            team_signer.public_key(),
            1_300,
            9_000,
        )
        .unwrap(),
        1_200,
        &root_signer,
    )
    .unwrap();
    append_profile_publication(
        &mut out_of_scope,
        ProfilePublicationV1::new(
            "org.ostadix/team-b",
            "node-b",
            profile(&team_signer, "node-b", 1, 1_300, 3_000),
        )
        .unwrap(),
        1_400,
        &team_signer,
    )
    .unwrap();
    assert!(matches!(
        verify_registry_store(
            &RegistryStoreV1::new(out_of_scope),
            &trusted,
            1_500,
            ProfileStalenessPolicyV1::Reject,
        ),
        Err(RegistryError::UnauthorizedSigner {
            sequence: 3,
            namespace,
        }) if namespace == "org.ostadix/team-b"
    ));
}

#[cfg(unix)]
#[test]
fn initialized_secret_key_is_mode_0600() {
    use std::os::unix::fs::MetadataExt;

    let directory = tempfile::tempdir().unwrap();
    let paths = RegistryStatePathsV1::new(
        directory.path().join("registry.cbor"),
        directory.path().join("registry.key"),
        directory.path().join("trust.cbor"),
    );
    write_new_registry_state(&paths, "org.ostadix", 1_000, 10_000, &signer(9)).unwrap();
    assert_eq!(
        std::fs::metadata(paths.signing_key()).unwrap().mode() & 0o777,
        0o600
    );
}

#[test]
fn local_cli_init_profile_publish_verify_list_and_export_import_workflow() {
    use std::process::Command;

    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("registry.cbor");
    let key = directory.path().join("registry.key");
    let trust = directory.path().join("trust.cbor");
    let profile = directory.path().join("profile.json");
    let exported = directory.path().join("export.cbor");
    let binary = env!("CARGO_BIN_EXE_o-registry");

    let run = |arguments: &[&str]| {
        let output = Command::new(binary).args(arguments).output().unwrap();
        assert!(
            output.status.success(),
            "o-registry {:?} failed:\nstdout:\n{}\nstderr:\n{}",
            arguments,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    run(&[
        "init",
        "--state",
        state.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
        "--namespace",
        "org.ostadix",
    ]);
    run(&[
        "profile-local",
        "--key",
        key.to_str().unwrap(),
        "--output",
        profile.to_str().unwrap(),
        "--node-id",
        "node-local",
        "--capability",
        "semantic/integer-add@1",
    ]);
    run(&[
        "publish-profile",
        "--state",
        state.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
        "--namespace",
        "org.ostadix",
        "--profile",
        profile.to_str().unwrap(),
    ]);
    run(&[
        "verify",
        "--state",
        state.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    let listed = run(&[
        "list",
        "--state",
        state.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    assert!(String::from_utf8_lossy(&listed.stdout).contains("org.ostadix\tnode-local"));
    run(&[
        "export",
        "--state",
        state.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
        "--output",
        exported.to_str().unwrap(),
    ]);
    run(&[
        "import",
        "--state",
        state.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
        "--input",
        exported.to_str().unwrap(),
    ]);
}
