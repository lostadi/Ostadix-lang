//! Observable graph/serial conformance for state-complete HGraph execution.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct RunOutcome {
    output: Output,
    files: BTreeMap<String, Vec<u8>>,
    workdir: tempfile::TempDir,
}

fn runtime_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_source(source: &str, executor: &str) -> RunOutcome {
    run_source_with_mode(source, Some(executor))
}

fn run_source_with_mode(source: &str, executor: Option<&str>) -> RunOutcome {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workdir = tempfile::tempdir().unwrap();
    let program = workdir.path().join("program.O");
    fs::write(&program, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_O"));
    command.env_remove("O_EXECUTOR");
    if let Some(executor) = executor {
        command.arg("--executor").arg(executor);
    }
    let output = command
        .arg(&program)
        .arg(root.join("backends"))
        .current_dir(workdir.path())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("O_TEST_WORKDIR", workdir.path())
        .output()
        .unwrap();
    let files = filesystem_snapshot(workdir.path());
    RunOutcome {
        output,
        files,
        workdir,
    }
}

#[test]
fn graph_execution_is_the_cli_default() {
    let source = "text^(state-complete default)_text\n";
    let default = run_source_with_mode(source, None);
    let graph = run_source(source, "graph");
    assert_equivalent(&default, &graph);
    assert!(
        default.output.status.success(),
        "{}",
        normalized_stderr(&default)
    );
}

fn filesystem_snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files);
            } else if path.file_name().and_then(|name| name.to_str()) != Some("program.O") {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn normalized_stderr(run: &RunOutcome) -> String {
    String::from_utf8_lossy(&run.output.stderr)
        .replace(
            &run.workdir.path().to_string_lossy().to_string(),
            "<WORKDIR>",
        )
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("Generated Python source:") {
                "Generated Python source: <normalized>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_equivalent(serial: &RunOutcome, graph: &RunOutcome) {
    assert_eq!(
        serial.output.status.code(),
        graph.output.status.code(),
        "exit status differs\nserial stderr:\n{}\ngraph stderr:\n{}",
        normalized_stderr(serial),
        normalized_stderr(graph)
    );
    assert_eq!(serial.output.stdout, graph.output.stdout, "stdout differs");
    assert_eq!(
        normalized_stderr(serial),
        normalized_stderr(graph),
        "stderr differs"
    );
    assert_eq!(serial.files, graph.files, "filesystem trees differ");
}

#[test]
fn nested_earlier_effect_preserves_full_file_order() {
    if !runtime_available("python3") {
        return;
    }
    let source = r#"python^(
from pathlib import Path
label = text^(A)_text
with Path("order.txt").open("a", encoding="utf-8") as stream:
    stream.write(label + "\n")
__oval_result__ = label
)_python

python^(
from pathlib import Path
with Path("order.txt").open("a", encoding="utf-8") as stream:
    stream.write("B\n")
__oval_result__ = "B"
)_python
"#;

    let serial = run_source(source, "serial");
    let graph = run_source(source, "graph");
    assert_equivalent(&serial, &graph);
    assert!(
        serial.output.status.success(),
        "{}",
        normalized_stderr(&serial)
    );
    assert_eq!(serial.files.get("order.txt").unwrap(), b"A\nB\n");
}

#[test]
fn bash_file_writes_preserve_exact_source_order() {
    if !runtime_available("bash") {
        return;
    }
    let source = r#"bash^(
printf '%s\n' A >> "\$O_TEST_WORKDIR/order-bash.txt"
)_bash

bash^(
printf '%s\n' B >> "\$O_TEST_WORKDIR/order-bash.txt"
)_bash
"#;

    let serial = run_source(source, "serial");
    let graph = run_source(source, "graph");
    assert_equivalent(&serial, &graph);
    assert!(
        serial.output.status.success(),
        "{}",
        normalized_stderr(&serial)
    );
    assert_eq!(serial.files.get("order-bash.txt").unwrap(), b"A\nB\n");
}

#[test]
fn earlier_failure_prevents_later_irreversible_effect() {
    if !runtime_available("python3") {
        return;
    }
    let source = r#"python^(
label = text^(A)_text
raise RuntimeError("stop")
)_python

python^(
from pathlib import Path
Path("must-not-exist").write_text("wrong", encoding="utf-8")
__oval_result__ = "wrong"
)_python
"#;

    let serial = run_source(source, "serial");
    let graph = run_source(source, "graph");
    assert_equivalent(&serial, &graph);
    assert!(!serial.output.status.success());
    assert!(!serial.files.contains_key("must-not-exist"));
    assert!(normalized_stderr(&serial).contains("RuntimeError: stop"));
}

#[test]
fn operation_stderr_order_matches_serial_bytes() {
    if !runtime_available("python3") {
        return;
    }
    let source = r#"python^(
import sys
label = text^(A)_text
sys.stderr.write(label + "\n")
sys.stderr.flush()
__oval_result__ = label
)_python

python^(
import sys
sys.stderr.write("B\n")
sys.stderr.flush()
__oval_result__ = "B"
)_python
"#;

    let serial = run_source(source, "serial");
    let graph = run_source(source, "graph");
    assert_equivalent(&serial, &graph);
    assert!(
        serial.output.status.success(),
        "{}",
        normalized_stderr(&serial)
    );
    assert_eq!(serial.output.stderr, b"A\nB\n");
}

#[test]
fn persistent_python_state_matches_serial() {
    if !runtime_available("python3") {
        return;
    }
    let source = r#"python[0]^(
counter = 40
__oval_result__ = counter
)_python[0]

python[0]^(
counter += 2
__oval_result__ = counter
)_python[0]
"#;

    let serial = run_source(source, "serial");
    let graph = run_source(source, "graph");
    assert_equivalent(&serial, &graph);
    assert!(
        serial.output.status.success(),
        "{}",
        normalized_stderr(&serial)
    );
    assert_eq!(serial.output.stdout, b"[number] 42\n");
}

#[test]
fn persistent_environment_mutation_and_read_match_serial() {
    if !runtime_available("python3") {
        return;
    }
    let source = r#"python[0]^(
import os
os.environ["O_STATE_COMPLETE_TEST"] = "visible"
__oval_result__ = "set"
)_python[0]

python[0]^(
import os
__oval_result__ = os.environ["O_STATE_COMPLETE_TEST"]
)_python[0]
"#;

    let serial = run_source(source, "serial");
    let graph = run_source(source, "graph");
    assert_equivalent(&serial, &graph);
    assert!(
        serial.output.status.success(),
        "{}",
        normalized_stderr(&serial)
    );
    assert_eq!(serial.output.stdout, b"visible");
}

#[test]
fn persistent_sql_state_matches_serial() {
    if !runtime_available("python3") {
        return;
    }
    let source = r#"sql[0]^(
CREATE TABLE values_table (value INTEGER);
INSERT INTO values_table (value) VALUES (42);
)_sql[0]

sql[0]^(
SELECT value FROM values_table;
)_sql[0]
"#;

    let serial = run_source(source, "serial");
    let graph = run_source(source, "graph");
    assert_equivalent(&serial, &graph);
    assert!(
        serial.output.status.success(),
        "{}",
        normalized_stderr(&serial)
    );
    assert_eq!(serial.output.stdout, b"[number] 42\n");
}

/// `ATTACH DATABASE` is connection-local. The native sql backend must keep one
/// sqlite3 session across blocks so a later `sql[0]` block still sees `t.*`.
#[test]
fn persistent_sql_attach_survives_across_blocks() {
    if !runtime_available("python3") || !runtime_available("sqlite3") {
        return;
    }
    let source = r#"
let db = python^(
import os, sqlite3
p = os.path.join(os.environ["O_TEST_WORKDIR"], "attach_ext.sqlite")
c = sqlite3.connect(p)
c.execute("CREATE TABLE insns(bin TEXT, addr INTEGER)")
c.execute("INSERT INTO insns VALUES ('SK', 42)")
c.commit()
c.close()
__oval_result__ = p
)_python

sql[0]^(
ATTACH DATABASE '$db' AS t
)_sql[0]

sql[0]^(
SELECT addr FROM t.insns
)_sql[0]
"#;

    let serial = run_source(source, "serial");
    let graph = run_source(source, "graph");
    assert_equivalent(&serial, &graph);
    assert!(
        serial.output.status.success(),
        "serial ATTACH failed:\n{}",
        normalized_stderr(&serial)
    );
    assert_eq!(
        serial.output.stdout,
        b"[number] 42\n",
        "expected attached table row; got stdout={:?} stderr={}",
        String::from_utf8_lossy(&serial.output.stdout),
        normalized_stderr(&serial)
    );
}

#[test]
fn ephemeral_evaluators_and_coordination_modes_match_serial() {
    if !runtime_available("python3") {
        return;
    }
    let cases = [
        "python^(local_value = 1\n__oval_result__ = local_value)_python\npython^(local_value = 2\n__oval_result__ = local_value)_python\n",
        "let a = text^(a)_text\nlet b = text^(b)_text\nnow(batch($a, $b))\n",
        "let a = text^(a)_text\nlet b = text^(b)_text\nnow(all($a, $b))\n",
        "let a = text^(a)_text\nlet b = text^(b)_text\nnow(any($a, $b))\n",
        "let a = text^(a)_text\nlet b = text^(b)_text\nnow(race($a, $b))\n",
    ];

    for source in cases {
        let serial = run_source(source, "serial");
        let graph = run_source(source, "graph");
        assert_equivalent(&serial, &graph);
        assert!(
            serial.output.status.success(),
            "{}",
            normalized_stderr(&serial)
        );
    }
}

#[test]
fn deferred_group_member_failures_match_serial() {
    if !runtime_available("python3") {
        return;
    }
    let cases = [
        (
            r#"let bad = python{defer}^(
raise RuntimeError("member-stop")
)_python{defer}
let good = text^(ok)_text
now(batch($bad, $good))
"#,
            true,
            "[error]",
        ),
        (
            r#"let bad = python{defer}^(
raise RuntimeError("member-stop")
)_python{defer}
let good = text^(ok)_text
now(all($bad, $good))
"#,
            false,
            "member-stop",
        ),
        (
            r#"let bad = python{defer}^(
raise RuntimeError("member-stop")
)_python{defer}
let good = text^(ok)_text
now(any($bad, $good))
"#,
            true,
            "ok",
        ),
        (
            r#"let bad = python{defer}^(
raise RuntimeError("member-stop")
)_python{defer}
let good = text^(ok)_text
now(race($bad, $good))
"#,
            false,
            "member-stop",
        ),
    ];

    for (source, succeeds, evidence) in cases {
        let serial = run_source(source, "serial");
        let graph = run_source(source, "graph");
        assert_equivalent(&serial, &graph);
        assert_eq!(serial.output.status.success(), succeeds);
        let observable = format!(
            "{}\n{}",
            String::from_utf8_lossy(&serial.output.stdout),
            normalized_stderr(&serial)
        );
        assert!(
            observable.contains(evidence),
            "missing `{evidence}` in group outcome:\n{observable}"
        );
    }
}

#[test]
fn representative_existing_examples_match_observably() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "examples/hello.O",
        "examples/html_basic.O",
        "examples/nested_splice.O",
    ] {
        let source = fs::read_to_string(root.join(relative)).unwrap();
        let serial = run_source(&source, "serial");
        let graph = run_source(&source, "graph");
        assert_equivalent(&serial, &graph);
        assert!(
            serial.output.status.success(),
            "{relative}: {}",
            normalized_stderr(&serial)
        );
    }
}
