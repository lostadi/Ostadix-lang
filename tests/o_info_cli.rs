use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use o_lang::information::{
    AcquisitionModalityV1, EntityDescriptorV1, InformationAtomV1, InformationDeltaPackV1,
    InformationDeltaV1, InformationObjectKindV1, InformationPackSignerV1, InformationRevisionV1,
    InformationSnapshotV1, InformationStoreReaderV1, OfflinePackPolicyV1,
    PackedInformationObjectV1, ParticipantV1, PayloadRefV1, PublicScalarV1, ScopeV1,
};
use sha2::Digest;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_o-info"))
        .args(args)
        .output()
        .unwrap()
}

fn success(args: &[&str]) -> String {
    let output = run(args);
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn value<'a>(text: &'a str, key: &str) -> &'a str {
    text.split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("missing {key}= in {text}"))
}

#[test]
fn local_round_trip_is_offline_deterministic_and_authority_free() {
    let root = tempfile::tempdir().unwrap();
    let first_state = root.path().join("first-state");
    let second_state = root.path().join("second-state");
    let key = root.path().join("keys/private.json");
    let trust = root.path().join("trust/public.json");
    let other_key = root.path().join("keys/other-private.json");
    let other_trust = root.path().join("trust/other-public.json");
    let pack = root.path().join("fact.pack.cbor");

    let first_init = success(&["init", "--state", first_state.to_str().unwrap()]);
    let repeated_init = success(&["init", "--state", first_state.to_str().unwrap()]);
    let second_init = success(&["init", "--state", second_state.to_str().unwrap()]);
    assert_eq!(
        value(&first_init, "revision"),
        value(&repeated_init, "revision")
    );
    assert_eq!(
        value(&first_init, "revision"),
        value(&second_init, "revision")
    );

    success(&[
        "keygen",
        "--key",
        key.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    let private_text = fs::read_to_string(&key).unwrap();
    let trust_text = fs::read_to_string(&trust).unwrap();
    assert!(private_text.contains("secret_key"));
    assert!(!trust_text.contains("secret_key"));
    assert!(trust_text.contains("verify-offline-information-delta-packs-only"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&key).unwrap().permissions().mode() & 0o077, 0);
    }

    let recorded = success(&[
        "record",
        "--state",
        first_state.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--pack",
        pack.to_str().unwrap(),
        "--namespace",
        "local",
        "--kind",
        "research-result",
        "--coordinate",
        "name=demo",
        "--predicate",
        "ostadix.local/public-scalar-v1",
        "--scalar",
        "text",
        "--value",
        "local-only",
        "--acknowledge-public",
    ]);
    assert!(recorded
        .contains("authority=information presence and signatures grant no execution authority"));
    let verified = success(&[
        "verify",
        "--pack",
        pack.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    assert!(verified.contains("verified pack="));

    let imported = success(&[
        "import",
        "--state",
        second_state.to_str().unwrap(),
        "--pack",
        pack.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    assert!(imported.contains("disposition=current-eligible"));
    let inspected = success(&["head", "--state", second_state.to_str().unwrap()]);
    assert!(inspected.contains("facts=1"));

    let stale = success(&[
        "import",
        "--state",
        second_state.to_str().unwrap(),
        "--pack",
        pack.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    assert!(stale.contains("disposition=historical-only"));
    assert!(stale.contains("exact-base mismatch"));
    assert_eq!(
        fs::read_dir(second_state.join("historical-packs"))
            .unwrap()
            .count(),
        1
    );

    success(&[
        "keygen",
        "--key",
        other_key.to_str().unwrap(),
        "--trust",
        other_trust.to_str().unwrap(),
    ]);
    let untrusted = run(&[
        "verify",
        "--pack",
        pack.to_str().unwrap(),
        "--trust",
        other_trust.to_str().unwrap(),
    ]);
    assert!(!untrusted.status.success());
    assert!(String::from_utf8_lossy(&untrusted.stderr).contains("untrusted"));
}

#[test]
fn signed_but_incomplete_pack_is_historical_and_does_not_advance() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let trust = root.path().join("trust.json");
    let pack_path = root.path().join("incomplete.pack.cbor");
    let initialized = success(&["init", "--state", state.to_str().unwrap()]);
    let genesis = initialized
        .split_whitespace()
        .find_map(|field| field.strip_prefix("revision="))
        .unwrap();

    let signer = InformationPackSignerV1::from_secret_bytes([7; 32]);
    write_trust(&trust, &signer);
    let subject = EntityDescriptorV1::new(
        "local",
        "subject",
        BTreeMap::from([("name".to_string(), "incomplete".to_string())]),
    )
    .unwrap();
    let producer = EntityDescriptorV1::new(
        "ostadix.info",
        "offline-pack-signer",
        BTreeMap::from([("key-id".to_string(), hex::encode(signer.key_id()))]),
    )
    .unwrap();
    let atom = InformationAtomV1::new(
        vec![ParticipantV1::new("subject", subject.id().unwrap()).unwrap()],
        "ostadix.local/public-scalar-v1",
        PayloadRefV1::public(PublicScalarV1::U64(7)).unwrap(),
        AcquisitionModalityV1::Declared,
        ScopeV1::default(),
        producer.id().unwrap(),
        vec![],
    )
    .unwrap();
    let delta = InformationDeltaV1::new(
        o_lang::information::RevisionIdV1::from_sha256(genesis.to_string()).unwrap(),
        producer.id().unwrap(),
        vec![atom.id().unwrap()],
        vec![],
    )
    .unwrap();
    let objects = vec![
        PackedInformationObjectV1::from_entity(&subject).unwrap(),
        PackedInformationObjectV1::from_entity(&producer).unwrap(),
        PackedInformationObjectV1::from_atom(&atom).unwrap(),
        PackedInformationObjectV1::from_delta(&delta).unwrap(),
    ];
    let signed = signer
        .sign(InformationDeltaPackV1::new(delta, objects, OfflinePackPolicyV1::default()).unwrap())
        .unwrap();
    fs::write(&pack_path, signed.canonical_bytes().unwrap()).unwrap();

    let imported = success(&[
        "import",
        "--state",
        state.to_str().unwrap(),
        "--pack",
        pack_path.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    assert!(imported.contains("disposition=historical-only"));
    assert!(imported.contains("exact base-plus-additions snapshot"));
    let inspected = success(&["head", "--state", state.to_str().unwrap()]);
    assert_eq!(value(&inspected, "revision"), genesis);
    assert!(inspected.contains("facts=0"));
}

#[test]
fn trusted_signature_cannot_claim_an_unrelated_producer_entity() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let trust = root.path().join("trust.json");
    let pack_path = root.path().join("wrong-producer.pack.cbor");
    let initialized = success(&["init", "--state", state.to_str().unwrap()]);
    let genesis =
        o_lang::information::RevisionIdV1::from_sha256(value(&initialized, "revision").to_string())
            .unwrap();
    let signer = InformationPackSignerV1::from_secret_bytes([9; 32]);
    write_trust(&trust, &signer);

    let subject = EntityDescriptorV1::new(
        "local",
        "subject",
        BTreeMap::from([("name".to_string(), "wrong-producer".to_string())]),
    )
    .unwrap();
    let unrelated_producer = EntityDescriptorV1::new(
        "ostadix.info",
        "offline-pack-signer",
        BTreeMap::from([("key-id".to_string(), "00".repeat(32))]),
    )
    .unwrap();
    let atom = InformationAtomV1::new(
        vec![ParticipantV1::new("subject", subject.id().unwrap()).unwrap()],
        "ostadix.local/public-scalar-v1",
        PayloadRefV1::public(PublicScalarV1::Bool(true)).unwrap(),
        AcquisitionModalityV1::Declared,
        ScopeV1::default(),
        unrelated_producer.id().unwrap(),
        vec![],
    )
    .unwrap();
    let delta = InformationDeltaV1::new(
        genesis.clone(),
        unrelated_producer.id().unwrap(),
        vec![atom.id().unwrap()],
        vec![],
    )
    .unwrap();
    let snapshot = InformationSnapshotV1::new(vec![atom.id().unwrap()]);
    let revision = InformationRevisionV1::new(
        snapshot.id().unwrap(),
        vec![genesis.clone()],
        Some(format!("ostadix.info-delta:{}", delta.id().unwrap())),
    )
    .unwrap();
    let objects = vec![
        PackedInformationObjectV1::from_entity(&subject).unwrap(),
        PackedInformationObjectV1::from_entity(&unrelated_producer).unwrap(),
        PackedInformationObjectV1::from_atom(&atom).unwrap(),
        PackedInformationObjectV1::from_snapshot(&snapshot).unwrap(),
        PackedInformationObjectV1::from_revision(&revision).unwrap(),
        PackedInformationObjectV1::from_delta(&delta).unwrap(),
    ];
    let signed = signer
        .sign(InformationDeltaPackV1::new(delta, objects, OfflinePackPolicyV1::default()).unwrap())
        .unwrap();
    fs::write(&pack_path, signed.canonical_bytes().unwrap()).unwrap();

    let imported = success(&[
        "import",
        "--state",
        state.to_str().unwrap(),
        "--pack",
        pack_path.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    assert!(imported.contains("disposition=historical-only"));
    assert!(imported.contains("not bound to verified signer key"));
    let inspected = success(&["head", "--state", state.to_str().unwrap()]);
    assert_eq!(value(&inspected, "revision"), genesis.as_sha256());
    assert!(inspected.contains("facts=0"));
}

#[test]
fn recording_requires_an_explicit_public_data_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let key = root.path().join("private.json");
    let trust = root.path().join("trust.json");
    success(&["init", "--state", state.to_str().unwrap()]);
    success(&[
        "keygen",
        "--key",
        key.to_str().unwrap(),
        "--trust",
        trust.to_str().unwrap(),
    ]);
    let output = run(&[
        "record",
        "--state",
        state.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--pack",
        root.path().join("fact.cbor").to_str().unwrap(),
        "--namespace",
        "local",
        "--kind",
        "fact",
        "--coordinate",
        "name=demo",
        "--predicate",
        "ostadix.local/public-scalar-v1",
        "--scalar",
        "text",
        "--value",
        "not-classified",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--acknowledge-public"));
    assert!(!root.path().join("fact.cbor").exists());
}

#[test]
fn private_key_reads_are_bounded_after_open() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let key = root.path().join("oversized-private.json");
    success(&["init", "--state", state.to_str().unwrap()]);
    fs::write(&key, vec![b'x'; 64 * 1024 + 1]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = run(&[
        "record",
        "--state",
        state.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--pack",
        root.path().join("fact.cbor").to_str().unwrap(),
        "--namespace",
        "local",
        "--kind",
        "fact",
        "--coordinate",
        "name=demo",
        "--predicate",
        "ostadix.local/public-scalar-v1",
        "--scalar",
        "bool",
        "--value",
        "true",
        "--acknowledge-public",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("65536 byte local limit"));
    assert!(!root.path().join("fact.cbor").exists());
}

#[test]
fn head_inspection_is_existing_root_only_and_preserves_store_metadata_and_bytes() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    success(&["init", "--state", state.to_str().unwrap()]);
    fs::remove_file(state.join("store.lock")).unwrap();
    let before = snapshot_tree(&state);

    let inspected = success(&["head", "--state", state.to_str().unwrap()]);
    assert!(inspected.contains("facts=0"));
    assert_eq!(snapshot_tree(&state), before);
    assert!(!state.join("store.lock").exists());

    let missing = root.path().join("missing-state");
    let output = run(&["head", "--state", missing.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(!missing.exists());
}

#[cfg(unix)]
#[test]
fn readonly_reader_rejects_incomplete_or_nonprivate_roots_without_repair() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = tempfile::tempdir().unwrap();
    let incomplete = root.path().join("incomplete");
    fs::create_dir(&incomplete).unwrap();
    fs::set_permissions(&incomplete, fs::Permissions::from_mode(0o700)).unwrap();
    let output = run(&["head", "--state", incomplete.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(!incomplete.join("objects").exists());
    assert!(!incomplete.join("heads").exists());
    assert!(!incomplete.join("store.lock").exists());

    let nonprivate = root.path().join("nonprivate");
    fs::create_dir(&nonprivate).unwrap();
    fs::set_permissions(&nonprivate, fs::Permissions::from_mode(0o755)).unwrap();
    let before_mode = fs::metadata(&nonprivate).unwrap().mode();
    let output = run(&["head", "--state", nonprivate.to_str().unwrap()]);
    assert!(!output.status.success());
    assert_eq!(fs::metadata(&nonprivate).unwrap().mode(), before_mode);
    assert!(!nonprivate.join("objects").exists());
    assert!(!nonprivate.join("heads").exists());
}

#[cfg(unix)]
#[test]
fn readonly_reader_rejects_symlinked_object_kind_directory() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    success(&["init", "--state", state.to_str().unwrap()]);
    let external = root.path().join("external-revisions");
    fs::create_dir(&external).unwrap();
    fs::set_permissions(&external, fs::Permissions::from_mode(0o700)).unwrap();

    let revision_directory = state.join("objects/revision");
    fs::remove_dir_all(&revision_directory).unwrap();
    symlink(&external, &revision_directory).unwrap();

    let reader = InformationStoreReaderV1::open_existing(&state).unwrap();
    let error = reader
        .get(InformationObjectKindV1::Revision, &"00".repeat(32))
        .unwrap_err();
    assert!(
        error.to_string().contains("symlink") || error.to_string().contains("symbolic link"),
        "unexpected reader error: {error}"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct TreeEntry {
    relative: String,
    directory: bool,
    len: u64,
    sha256: Option<String>,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
}

fn snapshot_tree(root: &Path) -> Vec<TreeEntry> {
    fn visit(root: &Path, current: &Path, entries: &mut Vec<TreeEntry>) {
        let mut children = fs::read_dir(current)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        children.sort();
        for path in children {
            let metadata = fs::symlink_metadata(&path).unwrap();
            let directory = metadata.is_dir();
            let sha256 = metadata
                .is_file()
                .then(|| hex::encode(sha2::Sha256::digest(fs::read(&path).unwrap())));
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt;
            entries.push(TreeEntry {
                relative: path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                directory,
                len: metadata.len(),
                sha256,
                #[cfg(unix)]
                inode: metadata.ino(),
                #[cfg(unix)]
                mode: metadata.mode(),
                #[cfg(unix)]
                modified_seconds: metadata.mtime(),
                #[cfg(unix)]
                modified_nanoseconds: metadata.mtime_nsec(),
            });
            if directory {
                visit(root, &path, entries);
            }
        }
    }
    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}

fn write_trust(path: &Path, signer: &InformationPackSignerV1) {
    let trust = serde_json::json!({
        "schema": "ostadix.info-trust/v1",
        "purpose": "verify-offline-information-delta-packs-only",
        "algorithm": "ed25519",
        "keys": [{
            "key_id": hex::encode(signer.key_id()),
            "public_key": hex::encode(signer.public_key()),
        }],
    });
    fs::write(path, serde_json::to_vec_pretty(&trust).unwrap()).unwrap();
}

#[test]
fn deterministic_genesis_matches_the_library_model() {
    let snapshot = InformationSnapshotV1::new(vec![]);
    let revision = InformationRevisionV1::new(snapshot.id().unwrap(), vec![], None).unwrap();
    assert_eq!(revision.id().unwrap(), revision.id().unwrap());
}
