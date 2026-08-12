//! O.eval reentrancy under the graph executor.
//!
//! `examples/meta_eval.O` exercises `quote^`/`O.eval` — the homoiconic
//! re-entry into the evaluator. Under the graph coordinator a nested `O.eval`
//! must run the spliced source through the same executor (recursion) without
//! deadlocking on the suspended actor. This test runs the example through the
//! compiled CLI under both `--executor graph` (default) and
//! `--executor serial`, and asserts the reentrant results are identical.
//!
//! Developer runs may explicitly skip when Python is absent; release-evidence
//! CI requires it. Once Python is present, launch or shim failures are test
//! failures rather than silently accepted infrastructure gaps.

use std::path::PathBuf;
use std::process::Command;

mod support;

fn run(executor: &str) -> std::process::Output {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let example = root.join("examples/meta_eval.O");
    let backends = root.join("backends");
    Command::new(env!("CARGO_BIN_EXE_O"))
        .arg("--executor")
        .arg(executor)
        .arg(&example)
        .arg(&backends)
        .output()
        .expect("launch compiled O binary")
}

#[test]
fn meta_eval_reentrancy_matches_across_executors() {
    if !support::require_runtime("python3") {
        return;
    }

    let graph = run("graph");
    assert!(
        graph.status.success(),
        "graph executor failed: {}",
        String::from_utf8_lossy(&graph.stderr)
    );

    let serial = run("serial");
    assert!(
        serial.status.success(),
        "serial executor failed: {}",
        String::from_utf8_lossy(&serial.stderr)
    );

    let graph_out = String::from_utf8_lossy(&graph.stdout);
    let serial_out = String::from_utf8_lossy(&serial.stdout);
    assert_eq!(
        graph_out, serial_out,
        "O.eval reentrancy diverged between graph and serial executors"
    );

    // Sanity: the example produces the canonical answers (42 appears twice —
    // once from O.eval(q) and once from the scoped form).
    assert!(
        graph_out.contains("42"),
        "expected reentrant O.eval result in output, got:\n{graph_out}"
    );
}
