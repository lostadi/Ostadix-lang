use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn bounded_semantic_custody_artifact_is_self_describing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = tempfile::tempdir().unwrap();
    let run = Command::new("bash")
        .arg(root.join("scripts/semantic_custody_demo.sh"))
        .arg(output.path())
        .env("O_BIN", env!("CARGO_BIN_EXE_O"))
        .env("OLANGC_BIN", env!("CARGO_BIN_EXE_olangc"))
        .env("O_BACKENDS_DIR", root.join("backends"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "custody demo failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.path().join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["schema"], "ostadix.semantic-custody-artifact/v1");
    assert_eq!(manifest["source_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(
        manifest["execution_intent_sha256"].as_str().unwrap().len(),
        64
    );
    assert_eq!(manifest["claim_scope"].as_array().unwrap().len(), 3);
    assert_eq!(manifest["nonclaims"].as_array().unwrap().len(), 3);

    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(output.path().join("result.json")).unwrap()).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["value"]["t"], "text");
    assert_eq!(result["value"]["v"]["utf8"], "semantic-custody answer=42");
}
