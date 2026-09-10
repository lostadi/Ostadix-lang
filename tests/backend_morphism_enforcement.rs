//! Executable crossing contracts constrain adapter conversions, not program
//! transformations or the side effects preceding a rejected output.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use o_lang::value::OValue;

mod support;

struct Fixture {
    directory: tempfile::TempDir,
    binary: PathBuf,
    session_limit: Option<usize>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("O-under-test");
        fs::copy(env!("CARGO_BIN_EXE_O"), &binary).unwrap();
        Self {
            directory,
            binary,
            session_limit: None,
        }
    }

    fn run(&self, executor: &str, source: &str, enforced: bool) -> Output {
        self.run_with_shims(
            executor,
            source,
            enforced,
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
        )
    }

    fn run_with_shims(&self, executor: &str, source: &str, enforced: bool, shims: &Path) -> Output {
        let mut command = Command::new(&self.binary);
        command.args(["--executor", executor, "--workers", "2", "--json"]);
        if enforced {
            command.args(["--morphism-contract", "python-plain-data-lossless"]);
        }
        command
            .args(["--eval", source])
            .arg(shims)
            .current_dir(self.directory.path())
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("O_BACKEND_OPERATION_TIMEOUT_MS", "5000")
            .env("O_BACKEND_SHUTDOWN_TIMEOUT_MS", "1000");
        if let Some(limit) = self.session_limit {
            command
                .env("O_BACKEND_MAX_OPEN_SESSIONS", limit.to_string())
                .env("O_BACKEND_MAX_OPEN_SESSIONS_PER_BACKEND", limit.to_string());
        }
        support::output_private_executable(&mut command).unwrap()
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
}

fn value(output: &Output) -> OValue {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    serde_json::from_value(envelope["value"].clone()).unwrap()
}

fn failure(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn serial_and_graph_enforce_exact_plain_values_and_allow_program_transformation() {
    if !support::require_runtime("python3") {
        return;
    }
    let fixture = Fixture::new();
    for executor in ["serial", "graph"] {
        let output = fixture.run(
            executor,
            r#"let payload = python^([40, {"delta": 2}])_python
python^(payload[0] + payload[1]["delta"])_python"#,
            true,
        );
        assert_eq!(value(&output), OValue::int(42));
        let output = fixture.run(executor, "python^(-0.0)_python", true);
        assert_eq!(value(&output), OValue::float(-0.0));
    }
}

#[test]
fn incompatible_input_and_splice_fail_before_backend_effects() {
    if !support::require_runtime("python3") {
        return;
    }
    let fixture = Fixture::new();
    for executor in ["serial", "graph"] {
        for splice in [false, true] {
            let source = format!("let payload = html^(opaque)_html\npython^(\nopen('input-effect', 'w').write('ran')\n{}\n)_python", if splice { "$payload" } else { "42" });
            let error = failure(&fixture.run(executor, &source, true));
            assert!(error.contains("morphism.input-rejected"), "{error}");
            assert!(!fixture.marker("input-effect").exists());
        }
    }
}

#[test]
fn native_output_rejection_prevents_publication_but_does_not_claim_effect_rollback() {
    if !support::require_runtime("python3") {
        return;
    }
    let fixture = Fixture::new();
    let output = fixture.run(
        "graph",
        r#"let result = python^(
open('before-output', 'w').write('ran')
(1, 2)
)_python
python^(open('after-output', 'w').write('ran'))_python"#,
        true,
    );
    assert!(failure(&output).contains("morphism.unsupported-native"));
    assert_eq!(
        fs::read_to_string(fixture.marker("before-output")).unwrap(),
        "ran"
    );
    assert!(!fixture.marker("after-output").exists());
    let legacy = fixture.run("graph", "python^((1, 2))_python", false);
    assert!(legacy.status.success(), "legacy tuple support changed");
}

#[test]
fn deferred_requests_and_recursive_callbacks_keep_the_contract() {
    if !support::require_runtime("python3") {
        return;
    }
    let fixture = Fixture::new();
    for executor in ["serial", "graph"] {
        let output = fixture.run(executor, "now(python{defer}^((1, 2))_python{defer})", true);
        assert!(failure(&output).contains("morphism.unsupported-native"));
        let output = fixture.run(
            executor,
            r#"python^(
O.eval("python" + "^(40 + 2)" + "_python")
)_python"#,
            true,
        );
        assert_eq!(value(&output), OValue::int(42));
        let output = fixture.run(
            executor,
            r#"python^(
O.eval("javascript" + "^(console.log(42))" + "_javascript")
)_python"#,
            true,
        );
        assert!(failure(&output).contains("morphism.unsupported-backend"));
        let output = fixture.run(
            executor,
            r#"python^(
shared = []
O.eval("python" + "^(42)" + "_python", O.scope({"a": shared, "b": shared}))
)_python"#,
            true,
        );
        assert!(failure(&output).contains("morphism.identity-required"));
        let output = fixture.run(
            executor,
            r#"python^(
class Sub(int): pass
O.eval("python" + "^(x)" + "_python", O.scope({"x": Sub(42)}))
)_python"#,
            true,
        );
        assert!(failure(&output).contains("morphism.unsupported-native"));
    }
}

#[test]
fn enforced_autonomous_workers_preserve_overlap_and_check_native_outputs() {
    if !support::require_runtime("python3") {
        return;
    }
    let fixture = Fixture::new();
    let operation = |name: &str| {
        format!(
            r#"python^(
import time
start = time.monotonic_ns()
time.sleep(0.25)
end = time.monotonic_ns()
open('{name}.interval', 'w').write(f'{{start}} {{end}}')
42
)_python"#
        )
    };
    let source = format!(
        "autonomous(batch({}, {}))",
        operation("left"),
        operation("right")
    );
    let output = fixture.run("graph", &source, true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let interval = |name| {
        fs::read_to_string(fixture.marker(name))
            .unwrap()
            .split_whitespace()
            .map(|part| part.parse::<u128>().unwrap())
            .collect::<Vec<_>>()
    };
    let left = interval("left.interval");
    let right = interval("right.interval");
    assert!(
        left[0] < right[1] && right[0] < left[1],
        "enforcement serialized workers: {left:?} {right:?}"
    );
    let output = fixture.run(
        "graph",
        "autonomous(batch(python^((1, 2))_python, python^(42)_python))",
        true,
    );
    assert!(failure(&output).contains("morphism.unsupported-native"));
}

#[test]
fn unreceipted_or_substituted_adapter_results_cannot_publish() {
    if !support::require_runtime("python3") {
        return;
    }
    let fixture = Fixture::new();
    let shims = fixture.marker("shims");
    fs::create_dir(&shims).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("backends/o_shim_common.py"),
        shims.join("o_shim_common.py"),
    )
    .unwrap();
    for response in [
        r#"{'status': 'ok', 'value': {'t': 'int', 'v': 42}}"#,
        r#"{'status': 'morphism_result_v1', 'receipt': {'contract': cmd['contract'], 'request_id': '00' * 32, 'input_witnesses': {}, 'value': {'t': 'int', 'v': 42}}}"#,
        r#"{'status': 'morphism_result_v1', 'receipt': {'contract': cmd['contract'], 'request_id': cmd['request_id'], 'input_witnesses': {'unexpected': {'t': 'null'}}, 'value': {'t': 'int', 'v': 42}}}"#,
        r#"{'status': 'morphism_result_v1', 'receipt': {'contract': cmd['contract'], 'request_id': cmd['request_id'], 'input_witnesses': {}, 'value': {'t': 'html', 'v': 'outside'}}}"#,
    ] {
        let script = format!(
            r#"import o_shim_common as wire
while True:
    cmd = wire.read_wire_message()
    if cmd is None:
        break
    if cmd['cmd'] == 'shutdown':
        wire.write_wire_message({{'status': 'ok', 'value': {{'t': 'null'}}}})
        break
    wire.write_wire_message({response})
"#
        );
        fs::write(shims.join("python_shim.py"), script).unwrap();
        let error = failure(&fixture.run_with_shims("graph", "python^(42)_python", true, &shims));
        assert!(
            error.contains("morphism.missing-receipt")
                || error.contains("morphism.receipt-mismatch")
                || error.contains("morphism.output-rejected"),
            "{error}"
        );
    }
}

#[test]
fn nested_fresh_callbacks_keep_distinct_actors_and_restore_each_outer_invocation() {
    if !support::require_runtime("python3") {
        return;
    }
    let fixture = Fixture::new();
    let inner = "python^(40 + 2)_python";
    let middle = format!(
        "python^(O.eval(bytes.fromhex('{}').decode()))_python",
        hex::encode(inner)
    );
    let outer = format!("python^(\nleft = O.eval(bytes.fromhex('{}').decode())\nright = O.eval(bytes.fromhex('{}').decode())\n[left, right]\n)_python", hex::encode(&middle), hex::encode(&middle));
    for executor in ["serial", "graph"] {
        for enforced in [false, true] {
            assert_eq!(
                value(&fixture.run(executor, &outer, enforced)),
                OValue::list(vec![OValue::int(42), OValue::int(42)])
            );
        }
    }
}

#[test]
fn suspended_fresh_actors_count_toward_quotas_and_failure_reaps_the_chain() {
    if !support::require_runtime("python3") {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.session_limit = Some(2);
    let child = "python^(42)_python";
    let middle = format!("python^(\nimport os\nopen('pids', 'a').write(str(os.getpid()) + '\\n')\nO.eval(bytes.fromhex('{}').decode())\n)_python", hex::encode(child));
    let outer = format!("python^(\nimport os\nopen('pids', 'a').write(str(os.getpid()) + '\\n')\nO.eval(bytes.fromhex('{}').decode())\n)_python", hex::encode(middle));
    for executor in ["serial", "graph"] {
        for enforced in [false, true] {
            let _ = fs::remove_file(fixture.marker("pids"));
            let error = failure(&fixture.run(executor, &outer, enforced));
            assert!(error.contains("session.capacity-exhausted"), "{error}");
            let pids = fs::read_to_string(fixture.marker("pids")).unwrap();
            assert_eq!(
                pids.lines().count(),
                2,
                "third actor started despite two-actor quota"
            );
            #[cfg(unix)]
            for pid in pids.lines() {
                let pid = pid.parse::<i32>().unwrap();
                // SAFETY: signal zero only probes the exact child PID that the
                // test process wrote; it never delivers a signal.
                let result = unsafe { libc::kill(pid, 0) };
                assert_eq!(result, -1, "fresh callback actor {pid} survived failure");
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
            }
        }
    }
}
