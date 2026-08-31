use o_lang::computation_core::{
    DerivationRelationV1, FacetIdV1, FacetKindV1, OComputationManifestV1,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

fn sha256_path(path: &Path) -> String {
    let mut input = File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hex::encode(hasher.finalize())
}

fn facet_id(value: &str) -> FacetIdV1 {
    FacetIdV1::new(value).unwrap()
}

#[test]
fn bounded_semantic_custody_artifact_is_self_describing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = tempfile::tempdir().unwrap();
    let run = Command::new("bash")
        .arg(root.join("scripts/semantic_custody_demo.sh"))
        .arg(output.path())
        .env("O_BIN", env!("CARGO_BIN_EXE_O"))
        .env("OLANGC_BIN", env!("CARGO_BIN_EXE_olangc"))
        .env("O_CLI_BIN", env!("CARGO_BIN_EXE_o-cli"))
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
    assert_eq!(manifest["schema"], "ostadix.semantic-custody-artifact/v2");
    let source_path = root.join("examples/semantic_custody.O");
    let source_sha256 = sha256_path(&source_path);
    assert_eq!(
        manifest["source_sha256"].as_str(),
        Some(source_sha256.as_str())
    );
    let intent_json: serde_json::Value =
        serde_json::from_slice(&fs::read(output.path().join("execution-intent.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest["execution_intent_sha256"],
        intent_json["execution_intent_sha256"]
    );
    assert_eq!(
        manifest["computation_revision_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let claims = manifest["claim_scope"].as_array().unwrap();
    let nonclaims = manifest["nonclaims"].as_array().unwrap();
    assert_eq!(claims.len(), 5);
    assert_eq!(nonclaims.len(), 6);
    assert!(claims.iter().any(|claim| claim
        .as_str()
        .is_some_and(|claim| claim.contains("locked staged workflow"))));
    assert!(nonclaims.iter().any(|claim| claim
        .as_str()
        .is_some_and(|claim| claim.contains("not historical transform execution"))));
    assert!(nonclaims.iter().any(|claim| claim
        .as_str()
        .is_some_and(|claim| claim.contains("does not independently authenticate shim"))));

    let artifact_names = [
        "execution-intent.json",
        "schedule.txt",
        "hgraph.dot",
        "result.json",
        "computation.cbor",
        "computation.json",
    ];
    let artifacts = manifest["artifacts"].as_object().unwrap();
    assert_eq!(artifacts.len(), artifact_names.len());
    for name in artifact_names {
        let actual = sha256_path(&output.path().join(name));
        assert_eq!(artifacts[name].as_str(), Some(actual.as_str()), "{name}");
    }

    let cbor_bytes = fs::read(output.path().join("computation.cbor")).unwrap();
    let json_bytes = fs::read(output.path().join("computation.json")).unwrap();
    let computation = OComputationManifestV1::decode_canonical(&cbor_bytes).unwrap();
    let json_computation = OComputationManifestV1::decode_json(&json_bytes).unwrap();
    assert_eq!(computation, json_computation);
    assert_eq!(computation.canonical_bytes().unwrap(), cbor_bytes);
    assert_eq!(computation.canonical_json_pretty().unwrap(), json_bytes);
    assert_eq!(
        computation.manifest().schema,
        "ostadix.ocomputation-manifest/v1"
    );
    assert_eq!(
        computation.revision().as_sha256(),
        manifest["computation_revision_sha256"].as_str().unwrap()
    );

    let source_id = facet_id("source");
    let o_binary_id = facet_id("tool/o-binary");
    let olangc_binary_id = facet_id("tool/olangc-binary");
    let intent_id = facet_id("execution-intent");
    let schedule_id = facet_id("schedule-explanation");
    let hgraph_id = facet_id("hgraph-rendering");
    let result_id = facet_id("terminal-observation");
    assert_eq!(computation.manifest().facets.len(), 7);
    assert_eq!(computation.manifest().derivations.len(), 4);
    assert_eq!(computation.manifest().roots.len(), 3);
    for root_id in [&source_id, &o_binary_id, &olangc_binary_id] {
        assert!(computation.manifest().roots.contains(root_id));
    }

    for (id, path) in [
        (&source_id, source_path),
        (&intent_id, output.path().join("execution-intent.json")),
        (&schedule_id, output.path().join("schedule.txt")),
        (&hgraph_id, output.path().join("hgraph.dot")),
        (&result_id, output.path().join("result.json")),
    ] {
        computation
            .require_facet_bytes(id, &fs::read(path).unwrap())
            .unwrap();
    }

    assert_eq!(
        computation.facet(&schedule_id).unwrap().kind,
        FacetKindV1::ScheduleExplanation
    );
    assert_eq!(
        computation.facet(&hgraph_id).unwrap().kind,
        FacetKindV1::HgraphRendering
    );
    assert_eq!(
        computation.facet(&result_id).unwrap().kind,
        FacetKindV1::TerminalObservation
    );

    let o_sha256 = sha256_path(Path::new(env!("CARGO_BIN_EXE_O")));
    let olangc_sha256 = sha256_path(Path::new(env!("CARGO_BIN_EXE_olangc")));
    assert_eq!(
        computation.facet(&o_binary_id).unwrap().content.as_sha256(),
        o_sha256
    );
    assert_eq!(
        computation
            .facet(&olangc_binary_id)
            .unwrap()
            .content
            .as_sha256(),
        olangc_sha256
    );

    let result_derivation = computation
        .manifest()
        .derivations
        .iter()
        .find(|derivation| derivation.output == result_id)
        .unwrap();
    assert_eq!(
        result_derivation.relation,
        DerivationRelationV1::ObservedFrom
    );
    assert!(result_derivation
        .inputs
        .iter()
        .any(|input| input.role.as_str() == "source" && input.facet == source_id));
    assert!(result_derivation.inputs.iter().any(|input| {
        input.role.as_str() == "required_execution_intent" && input.facet == intent_id
    }));
    assert!(result_derivation
        .inputs
        .iter()
        .any(|input| input.role.as_str() == "runtime_binary" && input.facet == o_binary_id));
    assert!(result_derivation
        .inputs
        .iter()
        .all(|input| input.facet != schedule_id));
    assert_eq!(
        result_derivation.transform.implementation.as_sha256(),
        o_sha256
    );
    for derivation in computation
        .manifest()
        .derivations
        .iter()
        .filter(|derivation| {
            [intent_id.clone(), schedule_id.clone(), hgraph_id.clone()].contains(&derivation.output)
        })
    {
        assert_eq!(
            derivation.transform.implementation.as_sha256(),
            olangc_sha256
        );
    }
    assert!(computation
        .manifest()
        .derivations
        .iter()
        .all(|derivation| derivation
            .transform
            .name
            .as_str()
            .starts_with("ostadix/workflow-attested/")));

    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(output.path().join("result.json")).unwrap()).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["value"]["t"], "text");
    assert_eq!(result["value"]["v"]["utf8"], "semantic-custody answer=42");

    let dirty_output = tempfile::tempdir().unwrap();
    fs::create_dir(dirty_output.path().join("result.json")).unwrap();
    let dirty = Command::new("bash")
        .arg(root.join("scripts/semantic_custody_demo.sh"))
        .arg(dirty_output.path())
        .env("O_BIN", env!("CARGO_BIN_EXE_O"))
        .env("OLANGC_BIN", env!("CARGO_BIN_EXE_olangc"))
        .env("O_CLI_BIN", env!("CARGO_BIN_EXE_o-cli"))
        .env("O_BACKENDS_DIR", root.join("backends"))
        .output()
        .unwrap();
    assert!(!dirty.status.success());
    assert!(String::from_utf8_lossy(&dirty.stderr).contains("refusing non-regular output entry"));
    assert!(!dirty_output.path().join("manifest.json").exists());
    assert!(!dirty_output.path().join(".semantic-custody.lock").exists());

    let published_manifest = fs::read(output.path().join("manifest.json")).unwrap();
    fs::create_dir(output.path().join(".semantic-custody.lock")).unwrap();
    let blocked = Command::new("bash")
        .arg(root.join("scripts/semantic_custody_demo.sh"))
        .arg(output.path())
        .env("O_BIN", env!("CARGO_BIN_EXE_O"))
        .env("OLANGC_BIN", env!("CARGO_BIN_EXE_olangc"))
        .env("O_CLI_BIN", env!("CARGO_BIN_EXE_o-cli"))
        .env("O_BACKENDS_DIR", root.join("backends"))
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("output is locked"));
    assert_eq!(
        fs::read(output.path().join("manifest.json")).unwrap(),
        published_manifest
    );
}
