use std::process::Command;

use o_lang::version::{OstadixVersionReportV1, VERSION_REPORT_SCHEMA_V1};

#[test]
fn plain_and_machine_readable_version_surfaces_agree() {
    let plain = Command::new(env!("CARGO_BIN_EXE_O"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert_eq!(String::from_utf8(plain.stdout).unwrap(), "O 0.4.0\n");

    let plain_subcommand = Command::new(env!("CARGO_BIN_EXE_O"))
        .arg("version")
        .output()
        .unwrap();
    assert!(plain_subcommand.status.success());
    assert_eq!(
        String::from_utf8(plain_subcommand.stdout).unwrap(),
        "O 0.4.0\n"
    );

    let json = Command::new(env!("CARGO_BIN_EXE_O"))
        .args(["version", "--json"])
        .output()
        .unwrap();
    assert!(json.status.success());
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let direct = OstadixVersionReportV1::current();
    assert_eq!(report, serde_json::to_value(&direct).unwrap());
    assert_eq!(report["schema"], VERSION_REPORT_SCHEMA_V1);
    assert_eq!(report["package_version"], "0.4.0");
    assert_eq!(report["minimum_rust_version"], "1.93.1");
    assert_eq!(report["release_rust_toolchain"], "1.97.1");
    assert_eq!(report["evidence_schema"], "oexec.evidence/v6");
    assert_eq!(report["admission_schema"], "oexec.admission/v6");
    assert_eq!(
        report["evidence_analyzer"],
        "ostadix-oir-evidence-compiler/v6"
    );
    assert_eq!(
        report["execution_intent_schema"],
        "oexec.execution-intent/v1"
    );
    assert_eq!(
        report["backend_catalog_schema"],
        "ostadix.backend-catalog/v6"
    );

    assert_eq!(
        report["backend_catalog_schema"],
        direct.backend_catalog_schema
    );
    assert_eq!(report["admission_schema"], direct.admission_schema);
}

#[test]
fn version_subcommand_rejects_ambiguous_extra_arguments() {
    let extra = Command::new(env!("CARGO_BIN_EXE_O"))
        .args(["version", "--json", "unexpected"])
        .output()
        .unwrap();
    assert!(!extra.status.success());
    assert!(String::from_utf8_lossy(&extra.stderr).contains("usage: O version [--json]"));

    let extra = Command::new(env!("CARGO_BIN_EXE_O"))
        .args(["--version", "unexpected"])
        .output()
        .unwrap();
    assert!(!extra.status.success());
    assert!(String::from_utf8_lossy(&extra.stderr)
        .contains("--version does not accept additional arguments"));
}
