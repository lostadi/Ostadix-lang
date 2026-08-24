//! Black-box boundaries for the unified `o plan` front door.
//!
//! These tests invoke the compiled `o-cli` binary.  They deliberately poison
//! node startup and isolate XDG roots so a successful assertion demonstrates
//! both the rendered plan and the absence of planner-side state mutation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn serial_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn one_route_project(root: &Path) {
    write(
        root.join("payload.txt").as_path(),
        b"static project payload\n",
    );
    write(
        root.join("olang.project.toml").as_path(),
        br#"[project]
name = "unified-plan-boundary"
default_route = "main"

[[routes]]
id = "main"
label = "read-only plan route"
kind = "shell"
command = ["sh", "-c", "printf 'must-not-execute\\n'"]
default = true
pure = true
guards = { requires_command = "sh" }
"#,
    );
}

#[derive(Debug, PartialEq, Eq)]
struct TreeEntry {
    relative: PathBuf,
    kind: &'static str,
    bytes: Vec<u8>,
    #[cfg(unix)]
    mode: u32,
}

fn snapshot_tree(root: &Path) -> Vec<TreeEntry> {
    fn visit(root: &Path, current: &Path, entries: &mut Vec<TreeEntry>) {
        let metadata = fs::symlink_metadata(current).unwrap();
        let relative = current.strip_prefix(root).unwrap().to_path_buf();
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        if metadata.file_type().is_symlink() {
            entries.push(TreeEntry {
                relative,
                kind: "symlink",
                bytes: fs::read_link(current)
                    .unwrap()
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec(),
                #[cfg(unix)]
                mode,
            });
            return;
        }
        if metadata.is_dir() {
            entries.push(TreeEntry {
                relative,
                kind: "directory",
                bytes: Vec::new(),
                #[cfg(unix)]
                mode,
            });
            let mut children = fs::read_dir(current)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, entries);
            }
            return;
        }
        entries.push(TreeEntry {
            relative,
            kind: "file",
            bytes: fs::read(current).unwrap(),
            #[cfg(unix)]
            mode,
        });
    }

    if !root.exists() {
        return Vec::new();
    }
    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries
}

struct IsolatedPlanEnvironment {
    xdg_state: PathBuf,
    xdg_config: PathBuf,
    home: PathBuf,
    poison_bin: PathBuf,
    node_marker: PathBuf,
}

impl IsolatedPlanEnvironment {
    fn new(root: &Path) -> Self {
        let poison_bin = root.join("poison-bin");
        fs::create_dir_all(&poison_bin).unwrap();
        let node_marker = root.join("o-node-invoked");
        let poison_node = poison_bin.join("o-node");
        write(
            &poison_node,
            b"#!/bin/sh\nprintf 'invoked\\n' >> \"$O_NODE_POISON_MARKER\"\nexit 97\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&poison_node, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            xdg_state: root.join("xdg-state"),
            xdg_config: root.join("xdg-config"),
            home: root.join("isolated-home"),
            poison_bin,
            node_marker,
        }
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(binary);
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let mut path_entries = vec![self.poison_bin.clone()];
        path_entries.extend(std::env::split_paths(&inherited_path));
        command
            .env("XDG_STATE_HOME", &self.xdg_state)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("HOME", &self.home)
            .env("O_NODE_POISON_MARKER", &self.node_marker)
            .env("O_LANG_NODE_BIN", self.poison_bin.join("o-node"))
            .env("PATH", std::env::join_paths(path_entries).unwrap())
            .env(
                "O_BACKENDS_DIR",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"),
            );
        command
    }

    fn o_cli(&self) -> Command {
        self.command(env!("CARGO_BIN_EXE_o-cli"))
    }

    fn olangc(&self) -> Command {
        self.command(env!("CARGO_BIN_EXE_olangc"))
    }

    fn assert_no_node_start(&self) {
        assert!(
            !self.node_marker.exists(),
            "read-only planning invoked the poisoned o-node launcher"
        );
    }
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn static_ordinary_and_project_plans_match_olangc_without_state_access() {
    let _serial = serial_guard();
    let temp = tempfile::tempdir().unwrap();
    let environment = IsolatedPlanEnvironment::new(temp.path());
    let ordinary = temp.path().join("static.O");
    write(&ordinary, b"text^(static-plan-parity)_text\n");
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    one_route_project(&project);

    // A nonexistent XDG state root proves planning does not create it.
    assert!(!environment.xdg_state.exists());
    let ordinary_front = environment
        .o_cli()
        .args(["plan", ordinary.to_str().unwrap()])
        .output()
        .unwrap();
    let ordinary_compiler = environment
        .olangc()
        .args([ordinary.to_str().unwrap(), "--target", "ir"])
        .output()
        .unwrap();
    assert_success(&ordinary_front, "ordinary root plan");
    assert_success(&ordinary_compiler, "ordinary olangc plan");
    assert_eq!(ordinary_front.stdout, ordinary_compiler.stdout);
    assert!(!environment.xdg_state.exists());

    // A preexisting tree catches content, entry, and permission mutations.
    fs::create_dir_all(environment.xdg_state.join("private/nested")).unwrap();
    write(
        &environment.xdg_state.join("private/nested/sentinel"),
        b"immutable planner sentinel\n",
    );
    let state_before = snapshot_tree(&environment.xdg_state);
    let project_before = snapshot_tree(&project);
    let project_front = environment
        .o_cli()
        .args(["plan", project.to_str().unwrap()])
        .output()
        .unwrap();
    let project_compiler = environment
        .olangc()
        .args([project.to_str().unwrap(), "--target", "ir"])
        .output()
        .unwrap();
    assert_success(&project_front, "project root plan");
    assert_success(&project_compiler, "project olangc plan");
    assert_eq!(project_front.stdout, project_compiler.stdout);
    assert_eq!(snapshot_tree(&environment.xdg_state), state_before);
    assert_eq!(snapshot_tree(&project), project_before);
    assert!(!environment.xdg_config.exists());
    environment.assert_no_node_start();
}

#[test]
fn ordinary_parallel_auto_live_is_local_only_and_writes_no_state() {
    let _serial = serial_guard();
    let temp = tempfile::tempdir().unwrap();
    let environment = IsolatedPlanEnvironment::new(temp.path());
    let ordinary = temp.path().join("local-live.O");
    write(&ordinary, b"text^(ordinary-live-readiness)_text\n");

    let output = environment
        .o_cli()
        .args([
            "plan",
            ordinary.to_str().unwrap(),
            "--parallel",
            "auto",
            "--live",
            "--json",
        ])
        .output()
        .unwrap();
    assert_success(&output, "ordinary live plan");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["schema"],
        serde_json::Value::String("ostadix.intent-plan-summary/v1".to_string())
    );
    let preview = &value["placement_preview"];
    assert_eq!(
        preview["schema"],
        serde_json::Value::String("ostadix.placement-preview/v1".to_string())
    );
    assert_eq!(preview["local"]["runtime_ready"], true);
    assert_eq!(preview["candidates"].as_array().unwrap().len(), 0);
    assert!(preview["selected_node_id"].is_null());
    assert!(preview["explanation"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line
            .as_str()
            .is_some_and(|line| line.contains("discovery and remote RPCs were not performed"))));
    assert!(!environment.xdg_state.exists());
    assert!(!environment.xdg_config.exists());
    assert!(!environment.home.exists());
    environment.assert_no_node_start();
}

#[test]
fn project_live_closed_registry_is_read_only_and_creates_no_history_or_peer_root() {
    let _serial = serial_guard();
    let temp = tempfile::tempdir().unwrap();
    let environment = IsolatedPlanEnvironment::new(temp.path());
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    one_route_project(&project);
    let peer_root = temp.path().join("pinned-peers-must-not-be-created");

    fs::create_dir_all(environment.xdg_state.join("preexisting")).unwrap();
    write(
        &environment.xdg_state.join("preexisting/sentinel"),
        b"state remains unchanged\n",
    );
    let state_before = snapshot_tree(&environment.xdg_state);
    let project_before = snapshot_tree(&project);
    let output = environment
        .o_cli()
        .args([
            "plan",
            project.to_str().unwrap(),
            "--parallel",
            "auto",
            "--live",
            "--closed-registry",
            "--mesh-peer-root",
            peer_root.to_str().unwrap(),
            "--mesh-discovery-timeout-ms",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    assert_success(&output, "closed-registry project live plan");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let preview = &value["placement_preview"];
    assert_eq!(preview["candidates"].as_array().unwrap().len(), 0);
    assert!(preview["selected_node_id"].is_null());
    assert!(preview["explanation"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line
            .as_str()
            .is_some_and(|line| line.contains("no already-pinned authenticated peer"))));
    assert_eq!(snapshot_tree(&environment.xdg_state), state_before);
    assert!(!environment.xdg_state.join("ostadix/runs-v1").exists());
    assert!(!peer_root.exists());
    assert!(!environment.xdg_config.exists());
    assert_eq!(snapshot_tree(&project), project_before);
    environment.assert_no_node_start();
}

#[test]
fn explain_schedule_is_rejected_explicitly_without_planning_or_state_changes() {
    let _serial = serial_guard();
    let temp = tempfile::tempdir().unwrap();
    let environment = IsolatedPlanEnvironment::new(temp.path());
    let ordinary = temp.path().join("rejected-option.O");
    write(&ordinary, b"text^(must-not-silently-ignore)_text\n");

    let output = environment
        .o_cli()
        .args(["plan", ordinary.to_str().unwrap(), "--explain-schedule"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("root static plan already includes"),
        "{stderr}"
    );
    assert!(
        stderr.contains("olangc INPUT --target ir --explain-schedule"),
        "{stderr}"
    );
    assert!(!environment.xdg_state.exists());
    assert!(!environment.xdg_config.exists());
    environment.assert_no_node_start();
}
