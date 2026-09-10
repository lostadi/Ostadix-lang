//! Execute a real embedded foreign launcher with ambient command lookup empty.
//! The host OS/dynamic loader remains part of this test's substrate.
#[cfg(unix)]
#[test]
fn generated_binary_runs_embedded_bash_without_ambient_path() {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::process::Command;
    let root = tempfile::tempdir().unwrap();
    let bundle = root.path().join("runtime");
    fs::create_dir_all(bundle.join("bin")).unwrap();
    fs::create_dir_all(bundle.join("libexec")).unwrap();
    fs::create_dir_all(bundle.join("lib/empty")).unwrap();
    fs::create_dir_all(bundle.join("lib/nested/readonly")).unwrap();
    fs::write(bundle.join("lib/nested/readonly/payload"), b"retained").unwrap();
    let readonly_dirs = ["lib/empty", "lib/nested/readonly", "lib/nested", "lib"];
    for name in readonly_dirs {
        fs::set_permissions(bundle.join(name), fs::Permissions::from_mode(0o555)).unwrap();
    }
    fs::write(
        bundle.join("runtime.json"),
        br#"{"schema":"ostadix.embedded-runtime/v1"}"#,
    )
    .unwrap();
    let bash = which::which("bash").expect("Bash is required for this runtime bundle test");
    fs::copy(bash, bundle.join("libexec/bash")).unwrap();
    fs::set_permissions(
        bundle.join("libexec/bash"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink("../libexec/bash", bundle.join("bin/bash")).unwrap();
    symlink(
        bundle.join("libexec/bash").canonicalize().unwrap(),
        bundle.join("bin/bash-absolute"),
    )
    .unwrap();
    let program = root.path().join("program.O");
    fs::write(&program, "bash^(bash-absolute -c 'printf 42')_bash\n").unwrap();
    let generated = root.path().join("generated");
    let materialized = Command::new(env!("CARGO_BIN_EXE_olangc"))
        .arg(&program)
        .args(["-o", "bundled-program", "--materialize-only"])
        .arg(&generated)
        .arg("--runtime-bundle")
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(
        materialized.status.success(),
        "{}",
        String::from_utf8_lossy(&materialized.stderr)
    );
    let inventory: serde_json::Value =
        serde_json::from_slice(&fs::read(generated.join("runtime-bundle-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        inventory["symlinks"],
        serde_json::json!([
            {"path":"bin/bash", "target":"../libexec/bash", "target_mode":0o755},
            {"path":"bin/bash-absolute", "target":"../libexec/bash", "target_mode":0o755}
        ])
    );
    // Remove original inputs before compiling/running: only embedded payloads
    // in the generated project are allowed to supply the runtime.
    for name in readonly_dirs.into_iter().rev() {
        fs::set_permissions(bundle.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::remove_dir_all(&bundle).unwrap();
    fs::remove_file(program).unwrap();
    let built = Command::new(env!("CARGO"))
        .args(["build", "--locked", "--offline"])
        .current_dir(&generated)
        .env("CARGO_TARGET_DIR", generated.join("target"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let extractions = root.path().join("runtime-extractions");
    fs::create_dir(&extractions).unwrap();
    // The scheduler otherwise puts its persistent cache in TMPDIR when the
    // environment has no HOME. Keep that cache separate from bundle ownership.
    let cache = root.path().join("cache");
    let output = Command::new(generated.join("target/debug/bundled-program"))
        .env_clear()
        .env("PATH", "")
        .env("TMPDIR", &extractions)
        .env("XDG_CACHE_HOME", &cache)
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
    let remaining = fs::read_dir(&extractions)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert!(
        remaining.is_empty(),
        "runtime extraction with read-only directories survived normal exit: {remaining:?}"
    );

    // The same bootstrap must hide an ambient executable absent from bin/.
    // Change only the embedded invocation link; the payload digest/data remain real.
    let data = generated.join("src/runtime_bundle/data.rs");
    let content = fs::read_to_string(&data).unwrap();
    fs::write(&data, content.replace("\"bin/bash\"", "\"bin/not-bash\"")).unwrap();
    let rebuilt = Command::new(env!("CARGO"))
        .args(["build", "--locked", "--offline"])
        .current_dir(&generated)
        .env("CARGO_TARGET_DIR", generated.join("target"))
        .output()
        .unwrap();
    assert!(
        rebuilt.status.success(),
        "{}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    let rejected = Command::new(generated.join("target/debug/bundled-program"))
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .env("TMPDIR", &extractions)
        .env("XDG_CACHE_HOME", &cache)
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(
        !rejected.status.success(),
        "missing bundled runtime was supplied by ambient PATH"
    );
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("bash"));
    let remaining = fs::read_dir(&extractions)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert!(
        remaining.is_empty(),
        "runtime extraction with read-only directories survived evaluation failure: {remaining:?}"
    );
}
