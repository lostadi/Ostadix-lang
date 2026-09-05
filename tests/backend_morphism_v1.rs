//! Executable conformance for the bounded BackendMorphism V1 profiles.
//!
//! These tests exercise the real O executable and current adapters. Runtime
//! absence is an explicit optional skip for developer portability and a hard
//! failure under the release CI runtime policy.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};

use o_lang::backend_morphism::{
    render_rust_scalar_stdout_program_v1, BackendMorphismKernelV1, BackendMorphismRejectionKindV1,
    BackendMorphismV1, BackendNativeValueV1,
};
use o_lang::hgraph::{
    solve::{backend_morphism_shadow_assessment_for_value, fidelity_for_value, solve_types},
    HEdge, HGraph, HNode, OpKind, Port, PortRole,
};
use o_lang::value::{Fidelity, FidelityAssessmentV2, OValue};

mod support;

fn run_observed(source: &str) -> (Output, serde_json::Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_O"))
        .args([
            "--executor",
            "graph",
            "--crossing-evidence",
            "--eval",
            source,
        ])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .output()
        .expect("launch observed O execution");
    let envelope = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid observed response: {error}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output, envelope)
}

#[test]
fn execution_records_directional_profile_limits_without_blocking_work() {
    use o_lang::backend_morphism::{
        observed_value_sha256, BackendCrossingObservationV1, RuntimeCrossingStateV1,
        RuntimeInputProfileV1,
    };
    if !support::require_runtimes(&["node", "python3"]) {
        return;
    }
    let source = r#"let payload = python^([1, 2])_python
javascript^(console.log(payload.reduce((a, b) => a + b, 0));)_javascript"#;
    let (output, envelope) = run_observed(source);
    let value = successful_value(&output);
    assert_eq!(
        value,
        successful_value(&run_o(source)),
        "observation must preserve executable out-of-profile behavior"
    );
    let records = envelope["backend_crossings"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    let record = records
        .iter()
        .find(|record| record["observation"]["backend"] == "javascript")
        .unwrap();
    let observation: BackendCrossingObservationV1 =
        serde_json::from_value(record["observation"].clone()).unwrap();
    assert_eq!(observation.state, RuntimeCrossingStateV1::ResultObserved);
    assert!(observation.published && !observation.discarded);
    assert_eq!(
        observation.result.as_ref().unwrap().value_sha256,
        observed_value_sha256(&value)
    );
    let input = observation
        .bindings
        .iter()
        .find(|binding| binding.name == "payload")
        .unwrap();
    assert!(
        matches!(&input.input_profile, RuntimeInputProfileV1::OutsideProfile { reason }
        if reason.kind == BackendMorphismRejectionKindV1::CurrentAdapterDoesNotBindContainers)
    );
    let digest = record["sha256"].as_str().unwrap();
    observation
        .verify(
            digest,
            &observation.admission_sha256,
            &observation.graph_sha256,
        )
        .unwrap();
    let mut tampered = observation.clone();
    tampered.plan_node += 1;
    assert!(tampered
        .verify(
            digest,
            &observation.admission_sha256,
            &observation.graph_sha256
        )
        .is_err());
    assert!(observation
        .verify(digest, &"0".repeat(64), &observation.graph_sha256)
        .is_err());
    let mut forged = record["observation"].clone();
    forged["invented_native_egress_guarantee"] = true.into();
    assert!(serde_json::from_value::<BackendCrossingObservationV1>(forged).is_err());
}

#[test]
fn runtime_crossings_distinguish_program_transformation_from_projection_loss() {
    use o_lang::backend_morphism::{BackendCrossingObservationV1, RuntimeInputProfileV1};
    if !support::require_runtime("python3") {
        return;
    }
    let (output, envelope) =
        run_observed("let input = python^(41)_python\npython^(input + 1)_python");
    assert_eq!(successful_value(&output), OValue::int(42));
    let records = envelope["backend_crossings"].as_array().unwrap();
    let observation: BackendCrossingObservationV1 =
        serde_json::from_value(records.last().unwrap()["observation"].clone()).unwrap();
    assert!(matches!(
        &observation.bindings[0].input_profile,
        RuntimeInputProfileV1::Assessed {
            fidelity: FidelityAssessmentV2::Lossless
        }
    ));
    assert_ne!(
        observation.bindings[0].value_sha256,
        observation.result.unwrap().value_sha256,
        "lossless adapter projection must not be confused with an identity backend program"
    );
}

#[test]
fn failed_crossing_retains_no_success_or_publication_claim() {
    use o_lang::backend_morphism::{BackendCrossingObservationV1, RuntimeCrossingStateV1};
    if !support::require_runtime("python3") {
        return;
    }
    let (output, envelope) = run_observed("python^(raise ValueError('observed failure'))_python");
    assert!(!output.status.success());
    assert_eq!(envelope["ok"], false);
    let record = &envelope["backend_crossings"][0];
    let observation: BackendCrossingObservationV1 =
        serde_json::from_value(record["observation"].clone()).unwrap();
    assert_eq!(observation.state, RuntimeCrossingStateV1::Failed);
    assert!(observation.result.is_none() && !observation.published);
    observation
        .verify(
            record["sha256"].as_str().unwrap(),
            &observation.admission_sha256,
            &observation.graph_sha256,
        )
        .unwrap();
}

#[test]
fn observations_preserve_worker_overlap_and_distinguish_discarded_results() {
    use o_lang::backend_morphism::{BackendCrossingObservationV1, RuntimeCrossingStateV1};
    if !support::require_runtime("python3") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let member = |mine: &str, other: &str| {
        format!(
            r#"python^(
import os, time
from pathlib import Path
root = Path(os.environ["O_TEST_WORKDIR"])
(root / "{mine}").write_text("ready")
deadline = time.monotonic() + 5
while not (root / "{other}").exists():
    if time.monotonic() > deadline:
        raise RuntimeError("other admitted worker did not overlap")
    time.sleep(0.01)
__oval_result__ = "{mine}"
)_python"#
        )
    };
    let source = format!(
        "autonomous(batch({}, {}))",
        member("left", "right"),
        member("right", "left")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_O"))
        .args([
            "--executor",
            "graph",
            "--workers",
            "2",
            "--crossing-evidence",
            "--eval",
            &source,
        ])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .env("O_TEST_WORKDIR", directory.path())
        .output()
        .unwrap();
    successful_value(&output);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let records = envelope["backend_crossings"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    for record in records {
        let observation: BackendCrossingObservationV1 =
            serde_json::from_value(record["observation"].clone()).unwrap();
        assert!(observation.published && !observation.discarded);
        observation
            .verify(
                record["sha256"].as_str().unwrap(),
                &observation.admission_sha256,
                &observation.graph_sha256,
            )
            .unwrap();
    }
    let (failed, envelope) = run_observed(
        "autonomous(batch(python^(raise ValueError('first'))_python, python^(42)_python))",
    );
    assert!(!failed.status.success());
    let observations = envelope["backend_crossings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| {
            serde_json::from_value::<BackendCrossingObservationV1>(record["observation"].clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(observations.iter().any(|observation| observation.state
        == RuntimeCrossingStateV1::Failed
        && !observation.published));
    assert!(observations.iter().any(|observation| observation.state == RuntimeCrossingStateV1::ResultObserved && observation.discarded && !observation.published),
        "a later physical result must not acquire semantic publication after the earlier failure: {observations:?}");
}

fn run_o(source: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_O"))
        .args(["--json", "--eval", source])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .output()
        .expect("launch compiled O binary")
}

fn successful_value(output: &Output) -> OValue {
    assert!(
        output.status.success(),
        "O execution failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("O --json emitted invalid JSON");
    assert_eq!(envelope["ok"], true);
    serde_json::from_value(envelope["value"].clone()).expect("O emitted an invalid OValue")
}

fn recursive_plain_value() -> OValue {
    OValue::map(HashMap::from([(
        "nested".to_owned(),
        OValue::list(vec![
            OValue::int(1),
            OValue::bool_(true),
            OValue::Null,
            OValue::text("x"),
        ]),
    )]))
}

fn recursive_native_value() -> BackendNativeValueV1 {
    BackendNativeValueV1::Map {
        entries: vec![(
            BackendNativeValueV1::string("nested"),
            BackendNativeValueV1::List {
                items: vec![
                    BackendNativeValueV1::Integer { value: 1.into() },
                    BackendNativeValueV1::Bool { value: true },
                    BackendNativeValueV1::Null,
                    BackendNativeValueV1::string("x"),
                ],
            },
        )],
    }
}

#[test]
fn python_recursive_plain_data_conforms_and_cycle_fails_visibly() {
    if !support::require_runtime("python3") {
        return;
    }

    let source = r#"python^(
{"nested": [1, True, None, "x"]}
)_python"#;
    let expected = recursive_plain_value();
    assert_eq!(successful_value(&run_o(source)), expected);

    let assessment = backend_morphism_shadow_assessment_for_value(&expected, "py").unwrap();
    assert!(assessment.is_supported());
    assert_eq!(assessment.composed_fidelity, FidelityAssessmentV2::Lossless);
    assert_eq!(assessment.lossless_law_holds, Some(true));

    let bound_value = successful_value(&run_o(
        r#"let payload = python^(
{"nested": [40, {"delta": 2}]}
)_python
python^(
payload["nested"][0] + payload["nested"][1]["delta"]
)_python"#,
    ));
    assert_eq!(
        bound_value,
        OValue::int(42),
        "the Python input leg must consume the real nested OValue binding"
    );

    let cycle = run_o(
        r#"python^(
cycle = []
cycle.append(cycle)
cycle
)_python"#,
    );
    assert!(!cycle.status.success(), "cyclic Python value was accepted");
    let failure: serde_json::Value =
        serde_json::from_slice(&cycle.stdout).expect("cycle failure was not structured JSON");
    assert_eq!(failure["ok"], false);
    assert!(
        failure["error"]
            .as_str()
            .is_some_and(|error| error.contains("RecursionError")),
        "cycle failure did not preserve its rejection reason: {failure}"
    );
}

#[test]
fn javascript_recursive_stdout_is_one_way_and_container_binding_is_not_claimed() {
    if !support::require_runtimes(&["node", "python3"]) {
        return;
    }

    let expected = recursive_plain_value();
    let stdout_value = successful_value(&run_o(
        r#"javascript^(
console.log(JSON.stringify({"nested":[1,true,null,"x"]}));
)_javascript"#,
    ));
    assert_eq!(stdout_value, expected);

    let egress = BackendMorphismKernelV1::Javascript
        .inject(&recursive_native_value())
        .unwrap();
    assert_eq!(egress.value, expected);
    assert!(matches!(
        egress.fidelity,
        FidelityAssessmentV2::Structural { .. }
    ));

    // Catalog V5 binds this bounded profile, but the compatibility capability
    // model still reports this generic container as lossless. The V1 morphism
    // remains a shadow result and exposes the current native-binding gap
    // without reducing execution capacity.
    assert_eq!(
        fidelity_for_value(&expected, "javascript"),
        Fidelity::Lossless
    );
    let mut graph = HGraph::default();
    let input = graph.add_node(HNode::with_value(expected.clone()));
    let output = graph.add_node(HNode::fresh());
    graph.add_edge(HEdge::constraint(
        OpKind::BackendCrossing {
            from_lang: "O".to_owned(),
            to_lang: "javascript".to_owned(),
        },
        vec![
            Port {
                node: input,
                role: PortRole::Input,
            },
            Port {
                node: output,
                role: PortRole::Output,
            },
        ],
    ));
    solve_types(&mut graph).unwrap();
    assert_eq!(
        graph.node(output).and_then(|node| node.fidelity.clone()),
        Some(Fidelity::Lossless),
        "Catalog V5 profile data must not rewrite production BackendCrossing fidelity"
    );
    let shadow = backend_morphism_shadow_assessment_for_value(&expected, "javascript").unwrap();
    assert!(!shadow.is_supported());
    assert_eq!(
        shadow.profiled_backend_output_to_o_boundary,
        "profiled JSON/scalar stdout decoded by the native adapter"
    );

    let scalar_binding = successful_value(&run_o(
        r#"let scalar = python^(
41
)_python
javascript^(
console.log(scalar + 1);
)_javascript"#,
    ));
    assert_eq!(
        scalar_binding,
        OValue::int(42),
        "the JavaScript scalar input leg must consume a real cross-runtime binding"
    );

    let native_use = successful_value(&run_o(
        r#"let payload = python^(
[1, 2]
)_python
javascript^(
console.log(payload.reduce((a, b) => a + b, 0));
)_javascript"#,
    ));
    assert_eq!(
        native_use,
        OValue::text("0[object Object][object Object]"),
        "a changed JavaScript container binding requires a simultaneous morphism-profile update"
    );
}

#[test]
fn rust_is_bounded_to_source_scalars_and_profiled_stdout() {
    if !support::require_runtime("rustc") {
        return;
    }

    let expected = recursive_plain_value();
    let stdout_value = successful_value(&run_o(
        r###"rust^(
fn main() {
    println!("{}", r#"{"nested":[1,true,null,"x"]}"#);
}
)_rust"###,
    ));
    assert_eq!(stdout_value, expected);
    assert_eq!(
        BackendMorphismKernelV1::Rust
            .inject(&recursive_native_value())
            .unwrap()
            .value,
        expected
    );

    let scalar = OValue::int(42);
    let scalar_assessment = BackendMorphismKernelV1::Rust.shadow_assess(&scalar);
    assert!(scalar_assessment.is_supported());
    assert!(matches!(
        scalar_assessment.composed_fidelity,
        FidelityAssessmentV2::Structural { .. }
    ));
    for value in [
        OValue::Null,
        OValue::bool_(true),
        scalar,
        OValue::float(1.5),
        OValue::text("line\n\"quote\"\u{0007}"),
    ] {
        let projected = BackendMorphismKernelV1::Rust.project(&value).unwrap();
        let rust_program = render_rust_scalar_stdout_program_v1(&projected.value).unwrap();
        let executable_source = format!("rust^(\n{rust_program})_rust");
        assert_eq!(
            successful_value(&run_o(&executable_source)),
            value,
            "the Rust input leg must compile and run the exact emitted scalar source"
        );
    }

    let error = BackendMorphismKernelV1::Rust
        .project(&expected)
        .unwrap_err();
    assert_eq!(
        error.kind,
        BackendMorphismRejectionKindV1::CurrentAdapterDoesNotBindContainers
    );
}
