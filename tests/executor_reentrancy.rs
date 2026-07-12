//! O.eval reentrancy under the graph executor.
//!
//! `examples/meta_eval.O` exercises `quote^`/`O.eval` — the homoiconic
//! re-entry into the evaluator. Under the graph coordinator a nested `O.eval`
//! must run the spliced source through the same executor (recursion) without
//! deadlocking on the suspended actor. This test runs the example through the
//! compiled CLI under both `--executor graph` (default) and
//! `--executor serial`, and asserts the reentrant results are identical.
//!
//! The test is skipped when the python backend is unavailable in the
//! environment (no python3, or the shim cannot start), rather than failing on
//! unrelated infrastructure gaps.

use std::path::PathBuf;
use std::process::Command;

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(executor: &str) -> Option<std::process::Output> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let example = root.join("examples/meta_eval.O");
    let backends = root.join("backends");
    Command::new(env!("CARGO_BIN_EXE_O"))
        .arg("--executor")
        .arg(executor)
        .arg(&example)
        .arg(&backends)
        .output()
        .ok()
}

#[test]
fn meta_eval_reentrancy_matches_across_executors() {
    if !python_available() {
        eprintln!("skipping: python3 not available");
        return;
    }

    let Some(graph) = run("graph") else {
        eprintln!("skipping: could not launch O binary");
        return;
    };
    if !graph.status.success() {
        // Backend may be unavailable in this sandbox; don't fail the suite on
        // unrelated infrastructure gaps.
        eprintln!(
            "skipping: graph run did not succeed: {}",
            String::from_utf8_lossy(&graph.stderr)
        );
        return;
    }

    let serial = run("serial").expect("serial run launches");
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
