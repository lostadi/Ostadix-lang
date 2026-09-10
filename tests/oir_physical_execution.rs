use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use o_lang::computation::*;
use o_lang::computation_core::artifact_id_for_bytes;
use o_lang::eval::Evaluator;
use o_lang::execution_contract::Policy;
use o_lang::hgraph::ReadySchedule;
use o_lang::ir::{BackendRegistry, OIrProgram};
use o_lang::parser::Parser;
use o_lang::value::OValue;

fn program(source: &str) -> OIrProgram {
    let tags = BackendRegistry::global().registered_backend_tags();
    OIrProgram::lower(&Parser::new(source, &tags).parse().unwrap())
}

fn evaluator(workers: usize) -> Evaluator {
    Evaluator::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends"))
        .with_registered_backends(BackendRegistry::global().registered_backend_tags())
        .with_runtime_executable(PathBuf::from(env!("CARGO_BIN_EXE_O")))
        .with_local_worker_parallelism(workers)
}

#[test]
fn parsed_program_automatically_plans_every_operation_and_moves_real_values() {
    let program = program("let base = python^(40)_python\nlet answer = python^($base + 2)_python\ntext^($answer)_text");
    let schedule = ReadySchedule::derive(&program.hgraph()).unwrap();
    let produced = schedule
        .ops
        .iter()
        .flat_map(|op| op.outputs.iter().copied())
        .collect::<BTreeSet<_>>();
    let expected_edges = schedule
        .ops
        .iter()
        .map(|op| {
            op.inputs
                .iter()
                .filter(|input| produced.contains(input))
                .count()
        })
        .sum::<usize>();
    let mut scope = HashMap::from([("untouched".into(), OValue::str_("source"))]);
    let report = execute_oir_physical_v1(
        &mut evaluator(2),
        &program,
        &mut scope,
        Policy::Eager,
        Arc::new(LocalSocketTransportV1::default()),
    )
    .unwrap();
    assert_eq!(report.value, OValue::str_("42"));
    assert_eq!(scope["base"], OValue::int(40));
    assert_eq!(scope["answer"], OValue::int(42));
    assert_eq!(scope["untouched"], OValue::str_("source"));
    assert_eq!(report.operations.len(), schedule.ops.len());
    assert!(report.operations.len() > 3);
    let transferred = report
        .transfers
        .iter()
        .filter_map(|transfer| transfer.edge)
        .collect::<BTreeSet<_>>();
    assert_eq!(transferred.len(), expected_edges);
    assert!(report
        .transfers
        .iter()
        .any(|transfer| transfer.observation.content
            == artifact_id_for_bytes(&OValue::int(42).canonical_bytes())));
    for task in report.plan.as_ref().unwrap().tasks() {
        let started = report
            .tasks
            .iter()
            .position(|event| {
                event.task == task.id && event.transition == PhysicalTaskTransitionV1::Started
            })
            .unwrap();
        for dependency in &task.dependencies {
            let finished = report
                .tasks
                .iter()
                .position(|event| {
                    event.task == *dependency
                        && event.transition == PhysicalTaskTransitionV1::Succeeded
                })
                .unwrap();
            assert!(finished < started, "consumer started before {dependency:?}");
        }
    }
}

#[derive(Default)]
struct CountTransfers(AtomicUsize);
impl OirPhysicalTransportV1 for CountTransfers {
    fn transfer(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(bytes.to_vec())
    }
}

fn assert_materialization_only_parity(source: &str) {
    let program = program(source);
    assert!(ReadySchedule::derive(&program.hgraph())
        .unwrap()
        .ops
        .is_empty());
    let initial = HashMap::from([("preserved".into(), OValue::int(7))]);
    let mut ordinary_scope = initial.clone();
    let ordinary = evaluator(2)
        .eval_ir_program_with_scope(&program, &mut ordinary_scope)
        .unwrap();
    let mut scope = initial.clone();
    let transport = Arc::new(CountTransfers::default());
    let report = execute_oir_physical_v1(
        &mut evaluator(2),
        &program,
        &mut scope,
        Policy::Eager,
        transport.clone(),
    )
    .unwrap();
    assert_eq!(report.value, ordinary);
    assert_eq!(scope, ordinary_scope);
    assert_eq!(scope, initial);
    assert!(report.plan.is_none());
    assert!(report.operations.is_empty());
    assert!(report.tasks.is_empty());
    assert!(report.transfers.is_empty());
    assert_eq!(transport.0.load(Ordering::SeqCst), 0);
}

#[test]
fn literal_only_program_matches_ordinary_materialization_without_physical_work() {
    assert_materialization_only_parity("A literal document.\n");
}

#[test]
fn empty_program_matches_ordinary_execution_without_physical_work() {
    assert_materialization_only_parity("");
}

#[derive(Default)]
struct ReverseScopeContainer(AtomicUsize);
impl OirPhysicalTransportV1 for ReverseScopeContainer {
    fn transfer(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut wire: serde_json::Value = serde_json::from_slice(bytes)?;
        if let Some(seed) = wire
            .get_mut("value")
            .and_then(|value| value.get_mut("bindings"))
            .and_then(|bindings| bindings.get_mut("seed"))
        {
            let field = match seed["t"].as_str() {
                Some("entries_map") => Some("entries"),
                Some("set") => Some("items"),
                _ => None,
            };
            if let Some(field) = field {
                seed[field].as_array_mut().unwrap().reverse();
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        Ok(serde_json::to_vec(&wire)?)
    }
}

fn assert_observable_reordering_rejected(value: OValue, reversed: OValue) {
    assert_eq!(value.canonical_bytes(), reversed.canonical_bytes());
    assert_ne!(value, reversed);
    assert_ne!(value.splice_repr(), reversed.splice_repr());
    let mut scope = HashMap::from([("seed".into(), value)]);
    let original = scope.clone();
    let transport = Arc::new(ReverseScopeContainer::default());
    let error = execute_oir_physical_v1(
        &mut evaluator(2),
        &program("text^($seed)_text"),
        &mut scope,
        Policy::Eager,
        transport.clone(),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("source carrier structure"),
        "{error:#}"
    );
    assert_eq!(transport.0.load(Ordering::SeqCst), 1);
    assert_eq!(scope, original);
}

#[test]
fn physical_transport_rejects_entries_map_reordering() {
    let entries = vec![
        (OValue::str_("a"), OValue::int(1)),
        (OValue::str_("b"), OValue::int(2)),
    ];
    let reversed = entries.iter().rev().cloned().collect();
    assert_observable_reordering_rejected(
        OValue::entries_map(entries),
        OValue::entries_map(reversed),
    );
}

#[test]
fn physical_transport_rejects_duplicate_map_key_reordering() {
    let entries = vec![
        (OValue::str_("same"), OValue::int(1)),
        (OValue::str_("same"), OValue::int(2)),
    ];
    let reversed = entries.iter().rev().cloned().collect();
    assert_observable_reordering_rejected(
        OValue::entries_map(entries),
        OValue::entries_map(reversed),
    );
}

#[test]
fn physical_transport_rejects_unordered_set_carrier_reordering() {
    use o_lang::value::SetKind;
    assert_observable_reordering_rejected(
        OValue::set(SetKind::Unordered, vec![OValue::int(1), OValue::int(2)]),
        OValue::set(SetKind::Unordered, vec![OValue::int(2), OValue::int(1)]),
    );
}

#[derive(Default)]
struct CorruptValue(AtomicUsize);
impl OirPhysicalTransportV1 for CorruptValue {
    fn transfer(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut wire: serde_json::Value = serde_json::from_slice(bytes)?;
        if wire["kind"] == "Value"
            && wire["value"] == serde_json::to_value(OValue::str_("secret"))?
            && self.0.fetch_add(1, Ordering::SeqCst) == 1
        {
            wire["value"] = serde_json::to_value(OValue::str_("changed"))?;
        }
        Ok(serde_json::to_vec(&wire)?)
    }
}

#[test]
fn changed_producer_value_cannot_reach_consumer_or_commit_scope() {
    let program = program("let saved = text^(secret)_text\nlet output = text^($saved)_text");
    let original = HashMap::from([("keep".into(), OValue::str_("original"))]);
    let mut scope = original.clone();
    let error = execute_oir_physical_v1(
        &mut evaluator(2),
        &program,
        &mut scope,
        Policy::Eager,
        Arc::new(CorruptValue::default()),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("source observation"),
        "{error:#}"
    );
    assert_eq!(scope, original);
}

struct CorruptScope;
impl OirPhysicalTransportV1 for CorruptScope {
    fn transfer(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut wire: serde_json::Value = serde_json::from_slice(bytes)?;
        if wire["kind"] == "Value" && wire["value"]["t"] == "scope" {
            wire["value"]["bindings"]["secret"] = serde_json::to_value(OValue::str_("forged"))?;
        }
        Ok(serde_json::to_vec(&wire)?)
    }
}

#[test]
fn changed_ambient_scope_cannot_rebind_an_admitted_consumer() {
    let original = HashMap::from([("secret".into(), OValue::str_("original"))]);
    let mut scope = original.clone();
    let error = execute_oir_physical_v1(
        &mut evaluator(2),
        &program("text^($secret)_text"),
        &mut scope,
        Policy::Eager,
        Arc::new(CorruptScope),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("source observation"),
        "{error:#}"
    );
    assert_eq!(scope, original);
}

#[test]
fn physical_transfers_preserve_autonomous_overlap_and_worker_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let source = format!(
        r#"let delay = text^(0.4)_text
autonomous(batch(
python^(import time
start = time.monotonic_ns()
time.sleep(float($delay))
end = time.monotonic_ns()
open({left}, "w").write(str(start) + "," + str(end))
__oval_result__ = "left"
)_python,
python^(import time
start = time.monotonic_ns()
time.sleep(float($delay))
end = time.monotonic_ns()
open({right}, "w").write(str(start) + "," + str(end))
__oval_result__ = "right"
)_python))"#,
        left = serde_json::to_string(&directory.path().join("left")).unwrap(),
        right = serde_json::to_string(&directory.path().join("right")).unwrap()
    );
    let program = program(&source);
    let report = execute_oir_physical_v1(
        &mut evaluator(2),
        &program,
        &mut HashMap::new(),
        Policy::Eager,
        Arc::new(LocalSocketTransportV1::default()),
    )
    .unwrap();
    let interval = |name| {
        std::fs::read_to_string(directory.path().join(name))
            .unwrap()
            .split(',')
            .map(|value| value.parse::<u64>().unwrap())
            .collect::<Vec<_>>()
    };
    let left = interval("left");
    let right = interval("right");
    assert!(
        left[0].max(right[0]) < left[1].min(right[1]),
        "{left:?} {right:?}"
    );
    assert!(report
        .transfers
        .iter()
        .any(|transfer| transfer.edge.is_some()));
    let (mut active, mut peak) = (0usize, 0usize);
    for event in &report.tasks {
        if matches!(event.task, PhysicalTaskIdV1::Operation(_)) {
            match event.transition {
                PhysicalTaskTransitionV1::Started => {
                    active += 1;
                    peak = peak.max(active);
                }
                PhysicalTaskTransitionV1::Succeeded | PhysicalTaskTransitionV1::Failed => {
                    active -= 1
                }
            }
        }
    }
    assert_eq!(active, 0);
    assert_eq!(
        peak, 2,
        "physical transport changed admitted evaluator capacity"
    );
}

#[test]
fn physical_mode_keeps_ordinary_failure_before_later_external_effects() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("forbidden");
    let source = format!("python^(raise RuntimeError('first failure'))_python\npython^(open({}, 'w').write('bad'))_python", serde_json::to_string(&marker).unwrap());
    let error = execute_oir_physical_v1(
        &mut evaluator(2),
        &program(&source),
        &mut HashMap::new(),
        Policy::Eager,
        Arc::new(LocalSocketTransportV1::default()),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("first failure"), "{error:#}");
    assert!(!marker.exists());
}

#[test]
fn physical_scope_transport_preserves_rich_numeric_carriers() {
    let value = OValue::List {
        v: vec![
            OValue::big_int(num_bigint::BigInt::from(1u8) << 200),
            OValue::rational(7, 13).unwrap(),
            OValue::float(f64::from_bits(0x7ff8000000000042)),
            OValue::float(-0.0),
            OValue::number(o_lang::value::ONumber::Decimal {
                coeff: num_bigint::BigInt::from(1200),
                exp10: -3,
                special: None,
            }),
        ],
    };
    let mut scope = HashMap::from([("seed".into(), value.clone())]);
    let report = execute_oir_physical_v1(
        &mut evaluator(2),
        &program("let copied = O^($seed)_O\nO^($copied)_O"),
        &mut scope,
        Policy::Eager,
        Arc::new(LocalSocketTransportV1::default()),
    )
    .unwrap();
    assert_eq!(report.value.canonical_bytes(), value.canonical_bytes());
    assert_eq!(scope["copied"].canonical_bytes(), value.canonical_bytes());
    assert_eq!(scope["seed"].canonical_bytes(), value.canonical_bytes());
    assert_eq!(report.value, value);
    assert_eq!(scope["copied"], value);
    assert_eq!(scope["seed"], value);
}

struct ChangeBackendDuringTransfer {
    path: PathBuf,
    changed: AtomicUsize,
}
impl OirPhysicalTransportV1 for ChangeBackendDuringTransfer {
    fn transfer(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        if self.changed.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut source = std::fs::read(&self.path)?;
            source.extend_from_slice(b"\n# changed after admission\n");
            std::fs::write(&self.path, source)?;
        }
        Ok(bytes.to_vec())
    }
}

#[test]
fn transport_cannot_hide_backend_drift_before_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("backends");
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), directory.path().join(entry.file_name())).unwrap();
        }
    }
    let mut evaluator = Evaluator::new(directory.path().to_path_buf())
        .with_registered_backends(BackendRegistry::global().registered_backend_tags())
        .with_runtime_executable(PathBuf::from(env!("CARGO_BIN_EXE_O")));
    let transport = ChangeBackendDuringTransfer {
        path: directory.path().join("python_shim.py"),
        changed: AtomicUsize::new(0),
    };
    let error = execute_oir_physical_v1(
        &mut evaluator,
        &program("python^(42)_python"),
        &mut HashMap::new(),
        Policy::Eager,
        Arc::new(transport),
    )
    .unwrap_err();
    let trace = evaluator.last_execution_trace().unwrap();
    assert!(!trace
        .events
        .iter()
        .any(|event| matches!(event, o_lang::eval::TraceEvent::NodeStarted(id) if id.0 == 0)));
    let message = format!("{error:#}");
    assert!(
        message.contains("admi") || message.contains("changed") || message.contains("drift"),
        "{message}"
    );
}
