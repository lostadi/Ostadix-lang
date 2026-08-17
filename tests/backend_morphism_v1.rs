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
