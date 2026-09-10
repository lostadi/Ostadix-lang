//! Acceptance boundaries for the read-only `o routes` catalog.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn deterministic_path() -> OsString {
    std::env::join_paths([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]).unwrap()
}

fn o_cli(home: &Path, state: &Path) -> Command {
    let temporary = home.join("tmp");
    fs::create_dir_all(home).unwrap();
    fs::create_dir_all(&temporary).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_o-cli"));
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_STATE_HOME", state)
        .env(
            "O_BACKENDS_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
        )
        .env("PATH", deterministic_path())
        .env("TMPDIR", temporary)
        .env("LANG", "C")
        .env("LC_ALL", "C");
    command
}

fn run(command: &mut Command) -> Output {
    command.output().expect("launch compiled o-cli")
}

fn single_json(output: &Output) -> Value {
    let stdout = std::str::from_utf8(&output.stdout).expect("catalog stdout must be UTF-8");
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
            "catalog stdout was not one JSON object: {error}\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn assert_no_secret(output: &Output, secrets: &[&str]) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for secret in secrets {
        assert!(
            !stdout.contains(secret),
            "stdout leaked {secret:?}:\n{stdout}"
        );
        assert!(
            !stderr.contains(secret),
            "stderr leaked {secret:?}:\n{stderr}"
        );
    }
}

fn manifest(marker: &Path) -> String {
    format!(
        r#"[project]
name = "catalog-project"

[[routes]]
id = "reference"
label = "LABEL_SECRET_do_not_print"
kind = "shell"
command = ["sh", "-c", "printf COMMAND_SECRET_do_not_print > \"$MARKER\"; printf '{{\"ok\":true}}'"]
env = {{ MARKER = "{}", TOKEN = "ENV_SECRET_do_not_print" }}
guards = {{ requires_command = "GUARD_SECRET_do_not_print" }}
result_codec = "json"
pure = true

[[routes]]
id = "candidate"
kind = "interpreter"
command = ["sh", "-c", "printf candidate > \"$MARKER\"; printf '{{\"ok\":true}}'"]
env = {{ MARKER = "{}" }}
result_codec = "json"
pure = true

[[route_sets]]
provides = "main"
alternatives = ["reference", "candidate"]
policy = "verify_equivalent"

[[route_sets]]
provides = "solo"
alternatives = ["reference"]
policy = "default"
"#,
        marker.display(),
        marker.display(),
    )
}

#[test]
fn catalog_is_ordered_safe_read_only_and_reports_structural_readiness() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    let marker = root.join("must-not-execute");
    let manifest_path = project.join("olang.project.toml");
    let manifest = manifest(&marker);
    let source = b"SOURCE_SECRET_do_not_print\n";
    write(&manifest_path, &manifest);
    write(&project.join("notes.txt"), source);

    let json_output =
        run(o_cli(&home, &state).args(["routes", project.to_str().unwrap(), "--json"]));
    assert!(
        json_output.status.success(),
        "route catalog failed: {}",
        String::from_utf8_lossy(&json_output.stderr),
    );
    let catalog = single_json(&json_output);
    assert_eq!(catalog["schema"], "ostadix.route-catalog/v1");
    assert_eq!(catalog["input"]["kind"], "project_directory");
    assert_eq!(
        catalog["input"]["path"],
        project.canonicalize().unwrap().to_str().unwrap(),
    );
    assert_eq!(
        catalog["input"]["bundle_sha256"].as_str().unwrap().len(),
        64
    );
    assert_eq!(catalog["project_name"], "catalog-project");
    assert_eq!(
        catalog["routes"],
        serde_json::json!([
            {"id": "reference", "kind": "shell_task", "result_codec": "json"},
            {"id": "candidate", "kind": "interpreter_command", "result_codec": "json"}
        ]),
    );
    assert_eq!(catalog["route_sets"][0]["name"], "main");
    assert_eq!(
        catalog["route_sets"][0]["declared_policy"],
        "verify_equivalent",
    );
    assert_eq!(catalog["route_sets"][0]["reference_route"], "reference");
    assert_eq!(
        catalog["route_sets"][0]["alternatives"],
        serde_json::json!(["reference", "candidate"]),
    );
    assert_eq!(catalog["route_sets"][0]["optimize_ready"], true);
    assert_eq!(catalog["route_sets"][0]["optimize_rejection"], Value::Null);
    assert_eq!(catalog["route_sets"][0]["reuse_ready"], true);
    assert_eq!(catalog["route_sets"][0]["reuse_rejection"], Value::Null);
    assert_eq!(catalog["route_sets"][1]["name"], "solo");
    assert_eq!(catalog["route_sets"][1]["optimize_ready"], false);
    assert!(catalog["route_sets"][1]["optimize_rejection"]
        .as_str()
        .unwrap()
        .contains("at least one candidate"));
    assert_eq!(catalog["failure"], Value::Null);

    let secrets = [
        "COMMAND_SECRET_do_not_print",
        "ENV_SECRET_do_not_print",
        "GUARD_SECRET_do_not_print",
        "LABEL_SECRET_do_not_print",
        "SOURCE_SECRET_do_not_print",
    ];
    assert_no_secret(&json_output, &secrets);

    let human_output = run(o_cli(&home, &state).args(["routes", project.to_str().unwrap()]));
    assert!(human_output.status.success());
    let human = String::from_utf8(human_output.stdout.clone()).unwrap();
    assert!(human.contains("Ostadix route catalog"));
    assert!(human.contains("optimize: ready"));
    assert!(human.contains("later winner reuse: ready after successful optimization"));
    assert!(human.contains("o optimize TARGET --route \"main\""));
    assert!(human.contains("at least one candidate"));
    assert_no_secret(&human_output, &secrets);

    assert!(!marker.exists(), "catalog unexpectedly executed a route");
    assert!(!state.exists(), "catalog unexpectedly created run state");
    assert_eq!(fs::read_to_string(manifest_path).unwrap(), manifest);
    assert_eq!(fs::read(project.join("notes.txt")).unwrap(), source);
}

#[test]
fn catalog_never_infers_a_route_set_from_shared_provides() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    let marker = root.join("must-not-execute");
    write(
        &project.join("olang.project.toml"),
        format!(
            r#"[project]
name = "shared-provides"

[[routes]]
id = "left"
command = ["sh", "-c", "printf left > \"$MARKER\""]
env = {{ MARKER = "{}" }}
provides = ["shared"]

[[routes]]
id = "right"
command = ["sh", "-c", "printf right > \"$MARKER\""]
env = {{ MARKER = "{}" }}
provides = ["shared"]
"#,
            marker.display(),
            marker.display(),
        ),
    );

    let output = run(o_cli(&home, &state).args([
        "routes",
        project.to_str().unwrap(),
        "--route-decl",
        "id=override;cmd=ROUTE_DECL_SECRET_do_not_print;env=TOKEN=OVERRIDE_ENV_SECRET;provides=shared",
        "--json",
    ]));
    assert!(output.status.success());
    let catalog = single_json(&output);
    assert_eq!(catalog["routes"].as_array().unwrap().len(), 3);
    assert_eq!(catalog["route_sets"], serde_json::json!([]));
    assert_eq!(catalog["failure"], Value::Null);
    assert_no_secret(
        &output,
        &["ROUTE_DECL_SECRET_do_not_print", "OVERRIDE_ENV_SECRET"],
    );
    assert!(!marker.exists(), "catalog unexpectedly executed a route");
    assert!(!state.exists(), "catalog unexpectedly created run state");
}

#[test]
fn directory_and_lifted_project_catalogs_preserve_the_same_safe_structure() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let project = root.join("project");
    let marker = root.join("must-not-execute");
    write(&project.join("olang.project.toml"), manifest(&marker));
    write(&project.join("notes.txt"), b"lifted source bytes\n");

    let bundle = o_lang::project::assemble(&project, "ignored-name", &[]).unwrap();
    let lifted = root.join("project.O");
    write(&lifted, o_lang::project::lower::lower_to_o(&bundle));

    let directory_output =
        run(o_cli(&home, &state).args(["routes", project.to_str().unwrap(), "--json"]));
    let lifted_output =
        run(o_cli(&home, &state).args(["routes", lifted.to_str().unwrap(), "--json"]));
    assert!(directory_output.status.success());
    assert!(lifted_output.status.success());
    let directory = single_json(&directory_output);
    let lifted = single_json(&lifted_output);

    assert_eq!(directory["input"]["kind"], "project_directory");
    assert_eq!(lifted["input"]["kind"], "lifted_project_bundle");
    assert_eq!(
        directory["input"]["bundle_sha256"],
        lifted["input"]["bundle_sha256"]
    );
    assert_eq!(directory["project_name"], lifted["project_name"]);
    assert_eq!(directory["routes"], lifted["routes"]);
    assert_eq!(directory["route_sets"], lifted["route_sets"]);
    assert_eq!(directory["failure"], Value::Null);
    assert_eq!(lifted["failure"], Value::Null);
    assert!(!marker.exists(), "catalog unexpectedly executed a route");
    assert!(!state.exists(), "catalog unexpectedly created run state");
}

#[test]
fn json_failures_are_single_sanitized_catalog_envelopes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let state = root.join("state");
    let ordinary = root.join("ordinary.O");
    write(&ordinary, b"SOURCE_FAILURE_SECRET_do_not_print\n");

    let unsupported =
        run(o_cli(&home, &state).args(["routes", ordinary.to_str().unwrap(), "--json"]));
    assert!(!unsupported.status.success());
    let failure = single_json(&unsupported);
    assert_eq!(failure["schema"], "ostadix.route-catalog/v1");
    assert_eq!(failure["input"], Value::Null);
    assert_eq!(failure["project_name"], Value::Null);
    assert_eq!(failure["routes"], serde_json::json!([]));
    assert_eq!(failure["route_sets"], serde_json::json!([]));
    assert_eq!(failure["failure"]["code"], "unsupported_input");
    assert_no_secret(&unsupported, &["SOURCE_FAILURE_SECRET_do_not_print"]);

    let invalid_arguments = run(o_cli(&home, &state).args(["routes", "--json"]));
    assert!(!invalid_arguments.status.success());
    let failure = single_json(&invalid_arguments);
    assert_eq!(failure["schema"], "ostadix.route-catalog/v1");
    assert_eq!(failure["failure"]["code"], "invalid_arguments");
    assert_eq!(failure["routes"], serde_json::json!([]));
    assert_eq!(failure["route_sets"], serde_json::json!([]));
    assert!(
        !state.exists(),
        "catalog failure unexpectedly created run state"
    );
}
