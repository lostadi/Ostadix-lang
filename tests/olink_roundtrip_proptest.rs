#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use proptest::prelude::*;
use tempfile::TempDir;

fn relative_path_strategy() -> impl Strategy<Value = String> {
    (
        prop::collection::vec("[a-z]{1,5}", 0..3),
        "[a-z]{1,7}",
        prop_oneof![
            Just(Some("py")),
            Just(Some("sh")),
            Just(Some("rs")),
            Just(Some("js")),
            Just(Some("rb")),
            Just(Some("java")),
            Just(Some("ml")),
            Just(Some("md")),
            Just(Some("txt")),
            Just(Some("c")),
            Just(Some("hs")),
            Just(Some("rkt")),
            Just(Some("sql")),
            Just(Some("ts")),
            Just(Some("svelte")),
            Just(Some("toml")),
            Just(None),
        ],
    )
        .prop_map(|(directories, name, extension)| {
            let mut path = directories.join("/");
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(&name);
            if let Some(extension) = extension {
                path.push('.');
                path.push_str(extension);
            }
            path
        })
}

fn content_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            "[a-zA-Z0-9 _=+*/.,;:{}'\"-]{0,30}",
            Just("\n".to_string()),
            Just("$HOME".to_string()),
            Just("$value".to_string()),
            Just("python^(".to_string()),
            Just(")_python".to_string()),
            Just(")_python[0]".to_string()),
            Just("bash[3]^(".to_string()),
            Just(")_bash[3]".to_string()),
            Just("\\python^(".to_string()),
            Just("lambda λ and snowman ☃".to_string()),
        ],
        0..10,
    )
    .prop_map(|parts| parts.concat())
}

fn write_tree(root: &Path, files: &BTreeMap<String, String>) {
    for (relative, content) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content.as_bytes()).unwrap();
    }
}

fn run(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn setup(files: &BTreeMap<String, String>, symlink_mode: u8) -> (TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let expected = temp.path().join("expected");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&expected).unwrap();
    write_tree(&source, files);
    write_tree(&expected, files);
    if symlink_mode & 1 != 0 {
        std::os::unix::fs::symlink(&source, source.join("zz-loop")).unwrap();
    }
    if symlink_mode & 2 != 0 {
        let target = files.keys().next().unwrap();
        let alias = Path::new(target)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| format!("zzzzzzzzzzzz-alias.{extension}"))
            .unwrap_or_else(|| "zzzzzzzzzzzz-alias".to_string());
        std::os::unix::fs::symlink(source.join(target), source.join(alias)).unwrap();
    }
    (temp, source, expected)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_file_trees_round_trip_with_empty_recursive_diff(
        files in prop::collection::btree_map(relative_path_strategy(), content_strategy(), 1..9),
        symlink_mode in 0u8..4,
    ) {
        let (temp, source, expected) = setup(&files, symlink_mode);
        let combined = temp.path().join("combined.O");
        let restored = temp.path().join("restored");

        let link = run(
            Command::new(env!("CARGO_BIN_EXE_o-link"))
                .arg(&source)
                .arg("-o")
                .arg(&combined),
        );
        let link_stderr = String::from_utf8_lossy(&link.stderr);
        prop_assert!(link_stderr.contains("o-link scan:"));

        let combined_source = fs::read_to_string(&combined).unwrap();
        prop_assert!(!combined_source.contains(&source.display().to_string()));

        run(
            Command::new(env!("CARGO_BIN_EXE_o-unlink"))
                .arg(&combined)
                .arg("-o")
                .arg(&restored),
        );

        let diff = Command::new("diff")
            .arg("-r")
            .arg(&expected)
            .arg(&restored)
            .output()
            .unwrap();
        prop_assert!(
            diff.status.success(),
            "round-trip diff was not empty:\n{}",
            String::from_utf8_lossy(&diff.stdout)
        );
    }
}
