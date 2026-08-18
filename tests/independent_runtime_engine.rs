use std::any::TypeId;
use std::fs;
use std::path::Path;

#[test]
fn compatibility_modules_preserve_the_engine_type_identities() {
    macro_rules! assert_same {
        ($shell:ty, $engine:ty) => {
            assert_eq!(TypeId::of::<$shell>(), TypeId::of::<$engine>());
        };
    }

    assert_same!(o_lang::value::OValue, ostadix_api::value::OValue);
    assert_same!(o_lang::eval::Evaluator, ostadix_api::eval::Evaluator);
    assert_same!(
        o_lang::parser::ParsedDocumentV1,
        ostadix_api::parser::ParsedDocumentV1
    );
    assert_same!(
        o_lang::ir::BackendRegistry,
        ostadix_api::ir::BackendRegistry
    );
    assert_same!(o_lang::hgraph::HGraph, ostadix_api::hgraph::HGraph);
    assert_same!(
        o_lang::evidence::EvidenceBundleV5,
        ostadix_api::evidence::EvidenceBundleV5
    );
    assert_same!(
        o_lang::evidence::EvidenceBundleV6,
        ostadix_api::evidence::EvidenceBundleV6
    );
    assert_same!(
        o_lang::evidence::ExecutionAdmissionV5,
        ostadix_api::evidence::ExecutionAdmissionV5
    );
    assert_same!(
        o_lang::evidence::ExecutionAdmissionV6,
        ostadix_api::evidence::ExecutionAdmissionV6
    );
    assert_same!(
        o_lang::eval::PlacementFragmentBindingsV2,
        ostadix_api::eval::PlacementFragmentBindingsV2
    );
    assert_same!(
        o_lang::hosted_remote::v2::HostedV2RuntimeConfig,
        ostadix_api::hosted_remote::v2::HostedV2RuntimeConfig
    );
    assert_same!(
        o_lang::project::LogicalHGraphV1,
        ostadix_api::project::LogicalHGraphV1
    );
    assert_same!(
        o_lang::information::InformationSnapshotV1,
        ostadix_api::information::InformationSnapshotV1
    );
    assert_same!(
        o_lang::information::InformationProvenanceV2,
        ostadix_api::information::InformationProvenanceV2
    );
    assert_same!(
        o_lang::information_provenance::InformationProvenanceAnalyzerV2,
        ostadix_api::information_provenance::InformationProvenanceAnalyzerV2
    );
    assert_same!(
        o_lang::world::WorldIdentity,
        ostadix_api::world::WorldIdentity
    );

    let shell_reflection = std::any::type_name::<o_lang::value::OValue>();
    let engine_reflection = std::any::type_name::<ostadix_api::value::OValue>();
    assert_eq!(shell_reflection, engine_reflection);
    assert!(engine_reflection.starts_with("ostadix_api::"));
}

#[test]
fn dependency_direction_is_shell_to_engine_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let shell_manifest: toml::Value = fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let shell_version = shell_manifest["package"]["version"].as_str().unwrap();
    let engine_dependency = &shell_manifest["dependencies"]["ostadix-api"];
    let exact_version = engine_dependency["version"].as_str().unwrap();
    assert_eq!(exact_version, format!("={shell_version}"));

    let engine_manifest_path = root.join("crates/ostadix-api/Cargo.toml");
    if engine_manifest_path.is_file() {
        let engine_manifest: toml::Value = fs::read_to_string(engine_manifest_path)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            engine_manifest["package"]["version"].as_str(),
            Some(shell_version)
        );
        assert_eq!(
            engine_dependency["path"].as_str(),
            Some("crates/ostadix-api")
        );
        assert!(engine_manifest["dependencies"].get("o-lang").is_none());
    } else {
        // Cargo normalizes a published shell manifest to the registry
        // dependency; package consumers must not require a sibling workspace.
        assert!(engine_dependency.get("path").is_none());
    }
}

#[test]
fn root_backend_directory_is_an_exact_compatibility_mirror() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(ostadix_api::shims::BUNDLED_SHIMS.len(), 23);
    for &(name, engine_bytes) in ostadix_api::shims::BUNDLED_SHIMS {
        let compatibility_bytes = fs::read(root.join("backends").join(name)).unwrap();
        assert_eq!(
            compatibility_bytes, engine_bytes,
            "root compatibility shim drifted from engine-owned {name}"
        );
    }

    let engine_root = root.join("crates/ostadix-api");
    if engine_root.is_dir() {
        assert_eq!(
            fs::read(root.join("LICENSE")).unwrap(),
            fs::read(engine_root.join("LICENSE")).unwrap()
        );
        assert_eq!(
            fs::read(root.join("NOTICE")).unwrap(),
            fs::read(engine_root.join("NOTICE")).unwrap()
        );
    }
}
