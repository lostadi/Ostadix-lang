//! Acceptance boundaries for read-only semantic operation record inspection.

use std::ffi::{CString, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use o_lang::computation_core::{
    artifact_id_for_bytes, ComputationTokenV1, OperationContractV1, OperationIdV1,
    OperationInterfaceV1, OperationPortV1, RealizationDescriptorV1, RealizationIdV1,
    RealizationPortRepresentationsV1, RealizationSetV1, SemanticArtifactRefV1,
};
use serde_json::Value;

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn make_sparse_file(path: &Path, bytes: u64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::File::create(path).unwrap().set_len(bytes).unwrap();
}

fn make_fifo(path: &Path) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

fn token(value: &str) -> ComputationTokenV1 {
    ComputationTokenV1::new(value).unwrap()
}

fn semantic_ref(schema: &str, seed: &str) -> SemanticArtifactRefV1 {
    SemanticArtifactRefV1::new(token(schema), artifact_id_for_bytes(seed.as_bytes())).unwrap()
}

#[derive(Clone)]
struct Fixture {
    contract: OperationContractV1,
    interface: OperationInterfaceV1,
    descriptors: Vec<RealizationDescriptorV1>,
    set: RealizationSetV1,
}

fn fixture() -> Fixture {
    let operation = OperationIdV1::new("tensor/normalize").unwrap();
    let contract = OperationContractV1::new(
        operation.clone(),
        1,
        semantic_ref("ostadix.semantic/true/v1", "preconditions"),
        semantic_ref("ostadix.semantic/unit-norm/v1", "postconditions"),
        semantic_ref("ostadix.semantic/stateless/v1", "state-model"),
        semantic_ref("ostadix.semantic/pure/v1", "effect-model"),
        semantic_ref("ostadix.semantic/pointwise/v1", "ordering"),
        semantic_ref("ostadix.semantic/deterministic/v1", "determinism"),
        semantic_ref("ostadix.fidelity/exact-f32/v1", "required-fidelity"),
    )
    .unwrap();
    let contract_id = contract.id().unwrap();
    let value_type = semantic_ref("ostadix.value/tensor-f32/v1", "tensor-f32-type");
    let interface = OperationInterfaceV1::new(
        operation,
        1,
        contract_id.clone(),
        Vec::new(),
        vec![OperationPortV1::new(token("input"), value_type.clone()).unwrap()],
        vec![OperationPortV1::new(token("output"), value_type).unwrap()],
    )
    .unwrap();
    let interface_id = interface.id().unwrap();

    let descriptor = |name: &str, implementation: &str| {
        RealizationDescriptorV1::new(
            RealizationIdV1::new(name).unwrap(),
            interface_id.clone(),
            contract_id.clone(),
            artifact_id_for_bytes(implementation.as_bytes()),
            semantic_ref(
                "ostadix.pipeline/native-call/v1",
                &format!("{name}-pipeline"),
            ),
            vec![RealizationPortRepresentationsV1::new(
                token("input"),
                vec![semantic_ref(
                    "ostadix.representation/dense-f32/v1",
                    "dense-f32-input",
                )],
            )
            .unwrap()],
            vec![RealizationPortRepresentationsV1::new(
                token("output"),
                vec![semantic_ref(
                    "ostadix.representation/dense-f32/v1",
                    "dense-f32-output",
                )],
            )
            .unwrap()],
            semantic_ref("ostadix.requirements/host-cpu/v1", "target-requirements"),
            semantic_ref("ostadix.requirements/no-state/v1", "state-requirements"),
            semantic_ref("ostadix.requirements/no-actor/v1", "actor-requirements"),
            semantic_ref("ostadix.fidelity/exact-f32/v1", "supplied-fidelity"),
            None,
            Vec::new(),
        )
        .unwrap()
    };
    let descriptors = vec![
        descriptor("native/reference", "reference-implementation"),
        descriptor("native/vectorized", "vectorized-implementation"),
    ];
    let set = RealizationSetV1::new(
        interface_id,
        contract_id,
        vec![descriptors[1].id().unwrap(), descriptors[0].id().unwrap()],
    )
    .unwrap();
    Fixture {
        contract,
        interface,
        descriptors,
        set,
    }
}

fn o_cli(home: &Path, state: &Path, marker: &Path) -> Command {
    let temporary = home.join("tmp");
    let poison_bin = home.join("poison-bin");
    fs::create_dir_all(&temporary).unwrap();
    fs::create_dir_all(&poison_bin).unwrap();
    for name in ["O", "olangc", "o-node"] {
        let path = poison_bin.join(name);
        write(
            &path,
            b"#!/bin/sh\nprintf invoked > \"$OPERATION_MARKER\"\nexit 99\n",
        );
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
    let path = std::env::join_paths([poison_bin, PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
        .unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_STATE_HOME", state)
        .env("TMPDIR", temporary)
        .env("PATH", path)
        .env("OPERATION_MARKER", marker)
        .env("LANG", "C")
        .env("LC_ALL", "C");
    command
}

fn run(command: &mut Command) -> Output {
    command.output().expect("launch compiled o-cli")
}

fn run_with_timeout(command: &mut Command, timeout: Duration) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch compiled o-cli");
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().expect("poll compiled o-cli").is_some() {
            return child.wait_with_output().expect("collect compiled o-cli");
        }
        if Instant::now() >= deadline {
            child.kill().expect("terminate stalled compiled o-cli");
            let output = child
                .wait_with_output()
                .expect("collect stalled compiled o-cli");
            panic!(
                "compiled o-cli exceeded {timeout:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn single_json(output: &Output) -> Value {
    let stdout = std::str::from_utf8(&output.stdout).expect("operation stdout must be UTF-8");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count(),
        1,
        "expected exactly one compact JSON envelope\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "operation stdout was not one JSON object: {error}\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn as_arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn verification_arguments(
    contract: &Path,
    interface: &Path,
    descriptors: &[&Path],
    set: &Path,
    json: bool,
) -> Vec<OsString> {
    let mut arguments = vec![
        OsString::from("operation"),
        OsString::from("verify"),
        OsString::from("--contract"),
        contract.as_os_str().to_owned(),
        OsString::from("--interface"),
        interface.as_os_str().to_owned(),
    ];
    for descriptor in descriptors {
        arguments.push(OsString::from("--descriptor"));
        arguments.push(descriptor.as_os_str().to_owned());
    }
    arguments.push(OsString::from("--set"));
    arguments.push(set.as_os_str().to_owned());
    if json {
        arguments.push(OsString::from("--json"));
    }
    arguments
}

#[test]
fn every_kind_inspects_json_and_canonical_cbor_without_resolving_references() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let marker = root.join("must-not-execute");
    let fixture = fixture();
    let cases = vec![
        (
            "contract",
            fixture.contract.canonical_bytes().unwrap(),
            fixture.contract.canonical_json().unwrap(),
        ),
        (
            "interface",
            fixture.interface.canonical_bytes().unwrap(),
            fixture.interface.canonical_json().unwrap(),
        ),
        (
            "descriptor",
            fixture.descriptors[0].canonical_bytes().unwrap(),
            fixture.descriptors[0].canonical_json().unwrap(),
        ),
        (
            "set",
            fixture.set.canonical_bytes().unwrap(),
            fixture.set.canonical_json().unwrap(),
        ),
    ];

    for (kind, cbor, json) in cases {
        let cbor_path = root.join(format!("{kind}.cbor"));
        let json_path = root.join(format!("{kind}.json"));
        write(&cbor_path, &cbor);
        write(&json_path, &json);
        let cbor_output = run(o_cli(&home, &state, &marker).args(as_arguments(&[
            "operation",
            "inspect",
            kind,
            cbor_path.to_str().unwrap(),
            "--json",
        ])));
        let json_output = run(o_cli(&home, &state, &marker).args(as_arguments(&[
            "operation",
            "inspect",
            kind,
            json_path.to_str().unwrap(),
            "--json",
        ])));
        assert!(
            cbor_output.status.success(),
            "{kind} CBOR inspection failed: {}",
            String::from_utf8_lossy(&cbor_output.stderr)
        );
        assert!(
            json_output.status.success(),
            "{kind} JSON inspection failed: {}",
            String::from_utf8_lossy(&json_output.stderr)
        );
        assert!(cbor_output.stderr.is_empty());
        assert!(json_output.stderr.is_empty());
        let cbor_report = single_json(&cbor_output);
        let json_report = single_json(&json_output);
        assert_eq!(cbor_report["schema"], "ostadix.operation-inspection/v1");
        assert_eq!(cbor_report["status"], "valid_record");
        assert_eq!(cbor_report["kind"], kind);
        assert_eq!(cbor_report["input_encoding"], "canonical_cbor");
        assert_eq!(json_report["input_encoding"], "validated_json");
        assert_eq!(cbor_report["id"], json_report["id"]);
        assert_eq!(cbor_report["record"], json_report["record"]);
        match kind {
            "contract" => {
                assert!(cbor_report["declared_interface_id"].is_null());
                assert!(cbor_report["declared_contract_id"].is_null());
                assert!(cbor_report["declared_descriptor_ids"].is_null());
            }
            "interface" => {
                assert!(cbor_report["declared_interface_id"].is_null());
                assert_eq!(
                    cbor_report["declared_contract_id"],
                    serde_json::to_value(&fixture.interface.contract).unwrap()
                );
                assert!(cbor_report["declared_descriptor_ids"].is_null());
            }
            "descriptor" => {
                assert_eq!(
                    cbor_report["declared_interface_id"],
                    serde_json::to_value(&fixture.descriptors[0].interface).unwrap()
                );
                assert_eq!(
                    cbor_report["declared_contract_id"],
                    serde_json::to_value(&fixture.descriptors[0].contract).unwrap()
                );
                assert!(cbor_report["declared_descriptor_ids"].is_null());
            }
            "set" => {
                assert_eq!(
                    cbor_report["declared_interface_id"],
                    serde_json::to_value(&fixture.set.interface).unwrap()
                );
                assert_eq!(
                    cbor_report["declared_contract_id"],
                    serde_json::to_value(&fixture.set.contract).unwrap()
                );
                assert_eq!(
                    cbor_report["declared_descriptor_ids"],
                    serde_json::to_value(&fixture.set.realizations).unwrap()
                );
            }
            _ => unreachable!(),
        }
        assert_eq!(cbor_report["referential_consistency"], "not_checked");
        assert_eq!(
            cbor_report["nonclaims"]["referenced_artifacts"],
            "not_resolved"
        );
        assert_eq!(cbor_report["nonclaims"]["selection"], "not_performed");
        assert_eq!(cbor_report["nonclaims"]["dispatch"], "not_run");
        assert_eq!(cbor_report["nonclaims"]["authority"], "none");
        assert_eq!(fs::read(cbor_path).unwrap(), cbor);
        assert_eq!(fs::read(json_path).unwrap(), json);
    }

    let set_path = root.join("set.cbor");
    let set_output = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "inspect",
        "set",
        set_path.to_str().unwrap(),
        "--json",
    ])));
    let set_report = single_json(&set_output);
    assert_eq!(
        set_report["declared_descriptor_ids"],
        serde_json::to_value(&fixture.set.realizations).unwrap()
    );
    assert_eq!(
        set_report["declared_interface_id"],
        serde_json::to_value(&fixture.set.interface).unwrap()
    );
    assert_eq!(
        set_report["declared_contract_id"],
        serde_json::to_value(&fixture.set.contract).unwrap()
    );
    assert!(!marker.exists());
    assert!(!state.exists());
}

#[test]
fn verify_checks_exact_closure_but_does_not_plan_select_or_execute() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let marker = root.join("must-not-execute");
    let fixture = fixture();
    let contract = root.join("contract.json");
    let interface = root.join("interface.cbor");
    let descriptor_left = root.join("descriptor-left.json");
    let descriptor_right = root.join("descriptor-right.cbor");
    let set = root.join("set.json");
    write(&contract, fixture.contract.canonical_json().unwrap());
    write(&interface, fixture.interface.canonical_bytes().unwrap());
    write(
        &descriptor_left,
        fixture.descriptors[0].canonical_json().unwrap(),
    );
    write(
        &descriptor_right,
        fixture.descriptors[1].canonical_bytes().unwrap(),
    );
    write(&set, fixture.set.canonical_json().unwrap());

    let output = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "verify",
        "--contract",
        contract.to_str().unwrap(),
        "--interface",
        interface.to_str().unwrap(),
        "--descriptor",
        descriptor_right.to_str().unwrap(),
        "--descriptor",
        descriptor_left.to_str().unwrap(),
        "--set",
        set.to_str().unwrap(),
        "--json",
    ])));
    assert!(
        output.status.success(),
        "operation verification failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report = single_json(&output);
    assert_eq!(report["schema"], "ostadix.operation-verification/v1");
    assert_eq!(report["status"], "referentially_consistent");
    assert_eq!(report["record_validation"], "pass");
    assert_eq!(report["referential_consistency"], "pass");
    assert_eq!(report["exact_descriptor_closure"], "pass");
    assert_eq!(report["input_encodings"]["contract"], "validated_json");
    assert_eq!(report["input_encodings"]["interface"], "canonical_cbor");
    assert_eq!(report["input_encodings"]["set"], "validated_json");
    let descriptors = report["descriptors"].as_array().unwrap();
    assert_eq!(
        descriptors
            .iter()
            .map(|descriptor| descriptor["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        fixture
            .set
            .realizations
            .iter()
            .map(|descriptor| descriptor.as_sha256())
            .collect::<Vec<_>>()
    );
    let left_id = fixture.descriptors[0].id().unwrap();
    let right_id = fixture.descriptors[1].id().unwrap();
    for descriptor in descriptors {
        let id = descriptor["id"].as_str().unwrap();
        let expected_encoding = if id == left_id.as_sha256() {
            "validated_json"
        } else if id == right_id.as_sha256() {
            "canonical_cbor"
        } else {
            panic!("verification projected an unknown descriptor id {id}");
        };
        assert_eq!(descriptor["input_encoding"], expected_encoding);
    }
    for (field, value) in [
        ("behavioral_equivalence", "not_proven"),
        ("target_eligibility", "not_evaluated"),
        ("cost_evaluation", "not_evaluated"),
        ("planning", "not_performed"),
        ("selection", "not_performed"),
        ("placement", "not_performed"),
        ("dispatch", "not_run"),
        ("recovery", "not_performed"),
        ("world_state", "not_observed"),
        ("authority", "none"),
    ] {
        assert_eq!(report["nonclaims"][field], value);
    }

    let human_output = run(o_cli(&home, &state, &marker).args(verification_arguments(
        &contract,
        &interface,
        &[&descriptor_right, &descriptor_left],
        &set,
        false,
    )));
    assert!(
        human_output.status.success(),
        "human operation verification failed: {}",
        String::from_utf8_lossy(&human_output.stderr)
    );
    assert!(human_output.stderr.is_empty());
    let human = String::from_utf8(human_output.stdout).unwrap();
    for required in [
        "Referential consistency: PASS",
        "behavioral_equivalence=not_proven",
        "planning=not_performed",
        "selection=not_performed",
        "placement=not_performed",
        "dispatch=not_run",
        "authority=none",
    ] {
        assert!(
            human.contains(required),
            "human output omitted {required:?}"
        );
    }
    assert!(!marker.exists());
    assert!(!state.exists());
}

#[test]
fn verify_rejects_missing_extra_duplicate_descriptors_and_duplicate_stable_names() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let marker = root.join("must-not-execute");
    let fixture = fixture();
    let contract = root.join("contract.json");
    let interface = root.join("interface.json");
    let descriptor_left = root.join("descriptor-left.json");
    let descriptor_right = root.join("descriptor-right.json");
    let set = root.join("set.json");
    write(&contract, fixture.contract.canonical_json().unwrap());
    write(&interface, fixture.interface.canonical_json().unwrap());
    write(
        &descriptor_left,
        fixture.descriptors[0].canonical_json().unwrap(),
    );
    write(
        &descriptor_right,
        fixture.descriptors[1].canonical_json().unwrap(),
    );
    write(&set, fixture.set.canonical_json().unwrap());

    let mut extra = fixture.descriptors[0].clone();
    extra.realization = RealizationIdV1::new("native/extra").unwrap();
    extra.implementation = artifact_id_for_bytes(b"extra-implementation");
    let extra = extra.verify().unwrap();
    let extra_path = root.join("descriptor-extra.json");
    write(&extra_path, extra.canonical_json().unwrap());

    let mut duplicate_name = fixture.descriptors[1].clone();
    duplicate_name.realization = fixture.descriptors[0].realization.clone();
    let duplicate_name = duplicate_name.verify().unwrap();
    let duplicate_name_path = root.join("descriptor-duplicate-name.json");
    write(
        &duplicate_name_path,
        duplicate_name.canonical_json().unwrap(),
    );
    let duplicate_name_set = RealizationSetV1::new(
        fixture.interface.id().unwrap(),
        fixture.contract.id().unwrap(),
        vec![
            fixture.descriptors[0].id().unwrap(),
            duplicate_name.id().unwrap(),
        ],
    )
    .unwrap();
    let duplicate_name_set_path = root.join("set-duplicate-name.json");
    write(
        &duplicate_name_set_path,
        duplicate_name_set.canonical_json().unwrap(),
    );

    let cases = vec![
        (
            "missing descriptor",
            vec![descriptor_left.as_path()],
            set.as_path(),
            "descriptor argument count",
        ),
        (
            "duplicate descriptor argument",
            vec![descriptor_left.as_path(), descriptor_left.as_path()],
            set.as_path(),
            "duplicate stable realization name",
        ),
        (
            "extra descriptor",
            vec![
                descriptor_left.as_path(),
                descriptor_right.as_path(),
                extra_path.as_path(),
            ],
            set.as_path(),
            "descriptor argument count",
        ),
        (
            "equal-count descriptor substitution",
            vec![descriptor_left.as_path(), extra_path.as_path()],
            set.as_path(),
            "membership",
        ),
        (
            "duplicate stable realization name",
            vec![descriptor_left.as_path(), duplicate_name_path.as_path()],
            duplicate_name_set_path.as_path(),
            "duplicate stable realization name",
        ),
    ];
    for (label, descriptors, set_path, expected_error) in cases {
        let output = run(o_cli(&home, &state, &marker).args(verification_arguments(
            &contract,
            &interface,
            &descriptors,
            set_path,
            true,
        )));
        assert!(!output.status.success(), "{label} unexpectedly passed");
        assert!(
            output.stdout.is_empty(),
            "{label} emitted a success envelope"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_error),
            "{label} omitted {expected_error:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(!marker.exists());
    assert!(!state.exists());
}

#[test]
fn verify_rejects_aggregate_raw_input_over_sixty_four_mib_before_decoding() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let marker = root.join("must-not-execute");
    let large = root.join("four-mib-sparse.record");
    make_sparse_file(&large, 4 * 1024 * 1024);
    let descriptors = vec![large.as_path(); 14];

    let output = run(o_cli(&home, &state, &marker).args(verification_arguments(
        &large,
        &large,
        &descriptors,
        &large,
        true,
    )));
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("aggregate raw-input budget"), "{stderr}");
    assert!(stderr.contains("67108864"), "{stderr}");
    assert!(!marker.exists());
    assert!(!state.exists());
}

#[test]
fn hostile_operation_parse_errors_are_escaped_but_help_and_version_remain_native() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let marker = root.join("must-not-execute");
    let contract = root.join("contract.json");
    write(&contract, fixture().contract.canonical_json().unwrap());

    let hostile_kind = "bad\n\u{1b}[31m\u{202e}";
    let output = run(o_cli(&home, &state, &marker).args([
        OsString::from("operation"),
        OsString::from("inspect"),
        OsString::from(hostile_kind),
        contract.as_os_str().to_owned(),
    ]));
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "diagnostic injected a new line");
    assert!(!stderr.contains('\u{1b}'));
    assert!(!stderr.contains('\u{202e}'));
    assert!(stderr.contains("\\n"));
    // Clap strips the ANSI escape sequence before rendering its diagnostic;
    // the remaining bidi control still passes through our explicit escaping.
    assert!(stderr.contains("\\u{202e}"));

    let help = run(o_cli(&home, &state, &marker).args(["operation", "--help"]));
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let help_stdout = String::from_utf8(help.stdout).unwrap();
    assert!(help_stdout.contains("Usage:"));
    assert!(help_stdout.contains("operation"));
    assert!(help_stdout.lines().count() > 2);

    let version = run(o_cli(&home, &state, &marker).arg("--version"));
    assert!(version.status.success());
    assert!(version.stderr.is_empty());
    assert!(String::from_utf8(version.stdout).unwrap().starts_with("o "));
    assert!(!marker.exists());
    assert!(!state.exists());
}

#[test]
fn wrong_kind_substitution_unknown_fields_and_oversize_inputs_fail_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let marker = root.join("must-not-execute");
    let fixture = fixture();
    let contract = root.join("contract.json");
    let interface = root.join("interface.json");
    let descriptor_left = root.join("descriptor-left.json");
    let descriptor_right = root.join("descriptor-right.json");
    let set = root.join("set.json");
    write(&contract, fixture.contract.canonical_json().unwrap());
    write(&interface, fixture.interface.canonical_json().unwrap());
    write(
        &descriptor_left,
        fixture.descriptors[0].canonical_json().unwrap(),
    );
    write(
        &descriptor_right,
        fixture.descriptors[1].canonical_json().unwrap(),
    );

    let wrong_kind = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "inspect",
        "interface",
        contract.to_str().unwrap(),
        "--json",
    ])));
    assert!(!wrong_kind.status.success());
    assert!(wrong_kind.stdout.is_empty());

    let mut substituted_set = serde_json::to_value(&fixture.set).unwrap();
    substituted_set["contract"] = Value::String("11".repeat(32));
    write(&set, serde_json::to_vec(&substituted_set).unwrap());
    let substituted = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "verify",
        "--contract",
        contract.to_str().unwrap(),
        "--interface",
        interface.to_str().unwrap(),
        "--descriptor",
        descriptor_left.to_str().unwrap(),
        "--descriptor",
        descriptor_right.to_str().unwrap(),
        "--set",
        set.to_str().unwrap(),
        "--json",
    ])));
    assert!(!substituted.status.success());
    assert!(substituted.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&substituted.stderr).contains("referentially_consistent"));

    let mut unknown_field = serde_json::to_value(&fixture.contract).unwrap();
    unknown_field["authority"] = Value::String("must-not-be-accepted".to_string());
    write(&contract, serde_json::to_vec(&unknown_field).unwrap());
    let unknown = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "inspect",
        "contract",
        contract.to_str().unwrap(),
        "--json",
    ])));
    assert!(!unknown.status.success());
    assert!(unknown.stdout.is_empty());

    let mut unsafe_schema = serde_json::to_value(&fixture.contract).unwrap();
    unsafe_schema["schema"] = Value::String("bad\n\u{1b}[31m\u{202e}".to_string());
    write(&contract, serde_json::to_vec(&unsafe_schema).unwrap());
    let unsafe_output = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "inspect",
        "contract",
        contract.to_str().unwrap(),
    ])));
    assert!(!unsafe_output.status.success());
    let stderr = String::from_utf8_lossy(&unsafe_output.stderr);
    assert!(!stderr.contains('\u{1b}'));
    assert!(!stderr.contains('\u{202e}'));
    assert!(stderr.contains("\\n"));
    assert!(stderr.contains("\\u{1b}"));
    assert!(stderr.contains("\\u{202e}"));

    let trailing_cbor = root.join("contract-trailing.cbor");
    let mut trailing_bytes = fixture.contract.canonical_bytes().unwrap();
    trailing_bytes.push(0);
    write(&trailing_cbor, trailing_bytes);
    let trailing_output = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "inspect",
        "contract",
        trailing_cbor.to_str().unwrap(),
        "--json",
    ])));
    assert!(!trailing_output.status.success());
    assert!(trailing_output.stdout.is_empty());

    let oversized = root.join("oversized.record");
    write(&oversized, vec![b'x'; 4 * 1024 * 1024 + 1]);
    let oversized_output = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "inspect",
        "contract",
        oversized.to_str().unwrap(),
    ])));
    assert!(!oversized_output.status.success());
    assert!(String::from_utf8_lossy(&oversized_output.stderr).contains("exceeds 4194304 bytes"));

    let fifo = root.join("contract.fifo");
    make_fifo(&fifo);
    let fifo_output = run_with_timeout(
        o_cli(&home, &state, &marker).args(as_arguments(&[
            "operation",
            "inspect",
            "contract",
            fifo.to_str().unwrap(),
        ])),
        Duration::from_secs(2),
    );
    assert!(!fifo_output.status.success());
    assert!(fifo_output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&fifo_output.stderr).contains("regular non-symlink file"));

    let symlink_target = root.join("contract-symlink-target.json");
    let symlink_path = root.join("contract-symlink.json");
    write(&symlink_target, fixture.contract.canonical_json().unwrap());
    symlink(&symlink_target, &symlink_path).unwrap();
    let symlink_output = run(o_cli(&home, &state, &marker).args(as_arguments(&[
        "operation",
        "inspect",
        "contract",
        symlink_path.to_str().unwrap(),
    ])));
    assert!(!symlink_output.status.success());
    assert!(symlink_output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&symlink_output.stderr).contains("regular non-symlink file"));
    assert!(!marker.exists());
    assert!(!state.exists());
}
