//! Black-box lifecycle coverage for the public hosted live-system CLI.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitStatus};

use o_lang::live_system::manifest::{
    payload_sha256, BuildManifest, CapabilityRequestManifest, HealthManifest, PackageManifest,
    RuntimeManifest, ServiceManifest, VerifiedPackage, PACKAGE_SCHEMA_V1,
};
use o_lang::live_system::protocol::{RUNTIME_PROGRAM_SCHEMA, RUNTIME_PROTOCOL};
use o_lang::live_system::store::PackageStore;
use o_lang::live_system::supervisor::HEALTH_PROTOCOL;
use o_lang::value::OValue;
use serde_json::Value;
use sha2::{Digest, Sha256};

const PACKAGE_NAME: &str = "runtime.cli";
const PACKAGE_ALIAS: &str = "runtime/cli";
const SERVICE_NAME: &str = "world.cli";
const REQUEST_KIND: &str = "endpoint";
const REQUEST_PURPOSE: &str = "CLI lifecycle channel";
const OPERATION: &str = "echo";

struct WritableTempDir(tempfile::TempDir);

impl WritableTempDir {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("create hosted-live CLI temporary root"))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for WritableTempDir {
    fn drop(&mut self) {
        restore_owner_permissions(self.0.path());
    }
}

struct CliOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

#[test]
fn public_cli_lifecycle_is_policy_gated_and_reconstructable() {
    let root = WritableTempDir::new();
    let state = root.path().join("state");
    let source = root.path().join("source");
    let payload = source.join("payload");
    let runtime_path = payload.join("bin/live.toml");
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();

    let runtime = format!(
        "schema = \"{RUNTIME_PROGRAM_SCHEMA}\"\nworld = \"cli.world\"\n\n\
         [health]\nstatus = \"healthy\"\n\n\
         [operations.{OPERATION}]\nkind = \"identity\"\n"
    );
    fs::write(&runtime_path, runtime.as_bytes()).unwrap();

    let manifest = PackageManifest {
        schema: PACKAGE_SCHEMA_V1.to_owned(),
        name: PACKAGE_NAME.to_owned(),
        version: "1.0.0".to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        payload_sha256: payload_sha256(&payload).unwrap(),
        runtime: RuntimeManifest {
            kind: "native_test_runtime".to_owned(),
            entry: "/bin/live.toml".to_owned(),
            abi: RUNTIME_PROTOCOL.to_owned(),
        },
        services: vec![ServiceManifest {
            name: SERVICE_NAME.to_owned(),
            protocol: RUNTIME_PROTOCOL.to_owned(),
        }],
        capability_requests: vec![CapabilityRequestManifest {
            kind: REQUEST_KIND.to_owned(),
            rights: vec!["send".to_owned(), "receive".to_owned()],
            purpose: REQUEST_PURPOSE.to_owned(),
        }],
        health: HealthManifest {
            protocol: HEALTH_PROTOCOL.to_owned(),
            timeout_ms: 1_000,
        },
        build: BuildManifest {
            source_sha256: hex::encode(Sha256::digest(runtime.as_bytes())),
            builder: "hosted-cli-test/v1".to_owned(),
        },
    };
    let manifest_text = manifest.canonical_toml().unwrap();
    let manifest_path = source.join("manifest.toml");
    fs::write(&manifest_path, &manifest_text).unwrap();
    let expected_digest = VerifiedPackage::load(&manifest_text, &payload)
        .unwrap()
        .digest()
        .to_string();

    let packed = run_cli(&[
        OsStr::new("pack"),
        OsStr::new("--manifest"),
        manifest_path.as_os_str(),
        OsStr::new("--payload"),
        payload.as_os_str(),
    ]);
    packed.assert_success("pack");
    assert_eq!(packed.stdout.trim(), expected_digest);

    let installed = run_cli(&[
        OsStr::new("install"),
        OsStr::new("--state"),
        state.as_os_str(),
        OsStr::new("--manifest"),
        manifest_path.as_os_str(),
        OsStr::new("--payload"),
        payload.as_os_str(),
        OsStr::new("--alias"),
        OsStr::new(PACKAGE_ALIAS),
    ]);
    installed.assert_success("install --alias");
    assert_eq!(installed.stdout.trim(), expected_digest);
    let resolved = PackageStore::open(state.join("store"))
        .unwrap()
        .resolve_alias(PACKAGE_ALIAS)
        .unwrap()
        .expect("installed alias resolves");
    assert_eq!(resolved.to_string(), expected_digest);

    let denied = run_cli(&[
        OsStr::new("activate"),
        OsStr::new("--state"),
        state.as_os_str(),
        OsStr::new(&expected_digest),
    ]);
    denied.assert_failure("default-deny activate");
    assert!(
        denied.stderr.contains("activation policy denies"),
        "activation failed for the wrong reason:\n{}",
        denied.stderr
    );

    let policy_path = root.path().join("policy.toml");
    fs::write(
        &policy_path,
        format!(
            r#"schema = "ocore.hosted-activation-policy/v1"

[[grants]]
package = "{PACKAGE_NAME}"
kind = "{REQUEST_KIND}"
purpose = "{REQUEST_PURPOSE}"
rights = ["send", "receive"]
"#
        ),
    )
    .unwrap();

    let activated = run_cli(&[
        OsStr::new("--policy"),
        policy_path.as_os_str(),
        OsStr::new("activate"),
        OsStr::new("--state"),
        state.as_os_str(),
        OsStr::new(&expected_digest),
    ]);
    activated.assert_success("policy-authorized activate");
    let activation_prefix = format!("activated {expected_digest}\n");
    let activation_json = activated
        .stdout
        .strip_prefix(&activation_prefix)
        .expect("activation prints its exact digest before status JSON");
    assert_single_service(activation_json, &expected_digest);

    let denied_status = run_cli(&[
        OsStr::new("status"),
        OsStr::new("--state"),
        state.as_os_str(),
    ]);
    denied_status.assert_failure("default-deny reconstruction");
    assert!(
        denied_status.stderr.contains("activation policy denies"),
        "reconstruction failed for the wrong reason:\n{}",
        denied_status.stderr
    );

    let status = run_cli(&[
        OsStr::new("--policy"),
        policy_path.as_os_str(),
        OsStr::new("status"),
        OsStr::new("--state"),
        state.as_os_str(),
    ]);
    status.assert_success("policy-authorized status reconstruction");
    assert_single_service(&status.stdout, &expected_digest);

    let input = OValue::Object {
        fields: BTreeMap::from([
            ("message".to_owned(), OValue::str_("bounded CLI input")),
            ("count".to_owned(), OValue::int(7)),
        ]),
    };
    let input_path = root.path().join("input.json");
    let input_bytes = serde_json::to_vec_pretty(&input).unwrap();
    assert!(input_bytes.len() < 512 * 1024);
    fs::write(&input_path, input_bytes).unwrap();

    let invoked = run_cli(&[
        OsStr::new("--policy"),
        policy_path.as_os_str(),
        OsStr::new("invoke"),
        OsStr::new("--state"),
        state.as_os_str(),
        OsStr::new(SERVICE_NAME),
        OsStr::new(RUNTIME_PROTOCOL),
        OsStr::new(OPERATION),
        input_path.as_os_str(),
    ]);
    invoked.assert_success("invoke");
    let returned: OValue = serde_json::from_str(&invoked.stdout).unwrap();
    assert_eq!(returned, input);
}

fn assert_single_service(json: &str, expected_digest: &str) {
    let value: Value = serde_json::from_str(json).expect("service status is JSON");
    let services = value.as_array().expect("service status is an array");
    assert_eq!(services.len(), 1, "expected one active service: {json}");
    let service = services[0].as_object().unwrap();
    assert_eq!(service["package"], PACKAGE_NAME);
    assert_eq!(service["digest"], expected_digest);
    assert_eq!(service["service"], SERVICE_NAME);
    assert_eq!(service["protocol"], RUNTIME_PROTOCOL);
    assert_eq!(service["world"], "cli.world");
    assert!(service["generation"]
        .as_u64()
        .is_some_and(|value| value > 0));
}

fn run_cli(args: &[&OsStr]) -> CliOutput {
    let output = Command::new(env!("CARGO_BIN_EXE_o-live-host"))
        .args(args)
        .output()
        .expect("run o-live-host");
    CliOutput {
        status: output.status,
        stdout: String::from_utf8(output.stdout).expect("o-live-host stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("o-live-host stderr is UTF-8"),
    }
}

impl CliOutput {
    fn assert_success(&self, action: &str) {
        assert!(
            self.status.success(),
            "{action} failed with {}\nstdout:\n{}\nstderr:\n{}",
            self.status,
            self.stdout,
            self.stderr
        );
    }

    fn assert_failure(&self, action: &str) {
        assert!(
            !self.status.success(),
            "{action} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
    }
}

fn restore_owner_permissions(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    make_owner_writable(path, &metadata);
    if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            restore_owner_permissions(&entry.path());
        }
    }
}

#[cfg(unix)]
fn make_owner_writable(path: &Path, metadata: &fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;

    let required = if metadata.is_dir() { 0o700 } else { 0o600 };
    let mode = metadata.permissions().mode() | required;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn make_owner_writable(path: &Path, metadata: &fs::Metadata) {
    let mut permissions = metadata.permissions();
    permissions.set_readonly(false);
    let _ = fs::set_permissions(path, permissions);
}
