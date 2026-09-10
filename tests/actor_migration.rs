//! Real process coverage for local supported-state restore and ownership handoff.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use o_lang::eval::migration::{migrate_persistent_actors, restore_persistent_actors};
use o_lang::eval::{Evaluator, PreparedPlacementFragmentV2};
use o_lang::parser::Parser;
use o_lang::placement::{GenerationV1, SemanticDigestV1, TaskAttemptIdV1};
use o_lang::value::OValue;

struct Fixture {
    root: tempfile::TempDir,
    shims: PathBuf,
    runtime: PathBuf,
    backends: HashSet<String>,
}

impl Fixture {
    fn new(instrument_restore: bool) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let shims = root.path().join("backends");
        fs::create_dir(&shims)?;
        for (name, bytes) in o_lang::shims::read_shims(None)? {
            fs::write(shims.join(name), bytes)?;
        }
        if instrument_restore {
            let path = shims.join("python_shim.py");
            let source = fs::read_to_string(&path)?;
            let marker = serde_json::to_string(
                &root
                    .path()
                    .join("restore-observed-source")
                    .to_string_lossy(),
            )?;
            let hook = format!(
                "    restored, deleted = _decode_python_globals(checkpoint[\"payload\"])\n    if restored.get('_reject_restore', False):\n        raise ValueError('injected destination restore refusal')\n    os.kill(restored['_source_pid'], 0)\n    with open({marker}, 'a') as marker:\n        marker.write(str(restored['_source_pid']) + '\\n')"
            );
            let replacement = source.replace(
                "    restored, deleted = _decode_python_globals(checkpoint[\"payload\"])",
                &hook,
            );
            assert_ne!(source, replacement);
            fs::write(path, replacement)?;
        }
        // Admission retains executable identities. A private copy isolates
        // running actor tests from concurrent Cargo relinking of target/O.
        let runtime = root.path().join("O-migration-test");
        fs::copy(env!("CARGO_BIN_EXE_O"), &runtime)?;
        Ok(Self {
            root,
            shims,
            runtime,
            backends: HashSet::from(["python".to_string(), "sql".to_string()]),
        })
    }

    fn evaluator(&self) -> Evaluator {
        Evaluator::new(self.shims.clone())
            .with_registered_backends(self.backends.clone())
            .with_runtime_executable(self.runtime.clone())
    }

    fn eval(&self, evaluator: &mut Evaluator, source: &str) -> Result<OValue> {
        evaluator.eval_document(Parser::new(source, &self.backends).parse()?)
    }

    fn target(
        &self,
        destination: &mut Evaluator,
        backend: &str,
        id: u32,
    ) -> Result<PreparedPlacementFragmentV2> {
        // If migration mistakenly executes this admitted body it destroys
        // the state these tests subsequently require, or raises immediately.
        let body = if backend == "sql" {
            "DROP TABLE sample;"
        } else {
            "raise RuntimeError('migration executed target code')"
        };
        destination.prepare_placement_fragment(
            &format!("{backend}[{id}]^({body})_{backend}[{id}]"),
            TaskAttemptIdV1::new(
                SemanticDigestV1::hash_bytes(
                    "ostadix/actor-migration-test/v1",
                    format!("{backend}:{id}").as_bytes(),
                ),
                GenerationV1::new(1)?,
            ),
        )
    }
}

const LIMIT: u64 = 4 * 1024 * 1024;

#[test]
fn python_graph_and_sqlite_state_move_and_source_cannot_recreate_actors() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut source = fixture.evaluator();
    let mut destination = fixture.evaluator();
    fixture.eval(&mut source, "python[17]^(x = []\nx.append(x)\ny = x\n_source_pid = __import__('os').getpid()\n__oval_result__ = 42)_python[17]")?;
    fixture.eval(&mut source, "sql[23]^(CREATE TABLE sample(v INTEGER); INSERT INTO sample VALUES (41); SELECT 1;)_sql[23]")?;
    let before = source.checkpoint_persistent_actors(LIMIT)?;
    let targets = vec![
        fixture.target(&mut destination, "sql", 23)?,
        fixture.target(&mut destination, "python", 17)?,
    ];
    let receipt = migrate_persistent_actors(&mut source, &mut destination, targets, LIMIT)?;
    assert!(receipt.source_shutdown_failures.is_empty(), "{receipt:?}");
    assert_ne!(
        receipt.source_evaluator,
        receipt.restore.destination_evaluator
    );
    assert_eq!(receipt.restore.snapshot_sha256, before.snapshot_sha256()?);
    assert_eq!(receipt.restore.backend_receipts.len(), 2);
    assert!(receipt
        .restore
        .backend_receipts
        .iter()
        .all(|receipt| receipt.restored));
    // The real destination shim verified that its source Python process was
    // still alive before acknowledging RestoreV1.
    assert!(!fs::read_to_string(fixture.root.path().join("restore-observed-source"))?.is_empty());
    assert_eq!(
        fixture.eval(
            &mut destination,
            "python[17]^(__oval_result__ = x is y and x[0] is x)_python[17]"
        )?,
        OValue::bool_(true)
    );
    assert_eq!(
        fixture.eval(
            &mut destination,
            "sql[23]^(SELECT v + 1 FROM sample;)_sql[23]"
        )?,
        OValue::int(42)
    );
    let error = fixture
        .eval(&mut source, "python[17]^(42)_python[17]")
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("state.actor-migrated"),
        "{error:#}"
    );
    assert!(source
        .checkpoint_persistent_actors(LIMIT)?
        .actors
        .is_empty());
    assert!(
        source
            .stage_persistent_actor_restore(before, LIMIT)
            .is_err(),
        "migration fence must also reject restoration into old owner"
    );
    Ok(())
}

#[test]
fn later_backend_restore_refusal_rolls_back_destination_and_preserves_both_sources() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let mut source = fixture.evaluator();
    let mut destination = fixture.evaluator();
    fixture.eval(
        &mut destination,
        "python[29]^(unrelated = 77\n__oval_result__ = unrelated)_python[29]",
    )?;
    for id in [17, 18] {
        fixture.eval(&mut source, &format!("python[{id}]^(x = {id}\n_source_pid = __import__('os').getpid()\n_reject_restore = {}\n__oval_result__ = x)_python[{id}]", if id == 18 { "True" } else { "False" }))?;
    }
    let targets = vec![
        fixture.target(&mut destination, "python", 17)?,
        fixture.target(&mut destination, "python", 18)?,
    ];
    let error =
        migrate_persistent_actors(&mut source, &mut destination, targets, LIMIT).unwrap_err();
    assert!(
        format!("{error:#}").contains("injected destination restore refusal"),
        "{error:#}"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("restore-observed-source"))?
            .lines()
            .count(),
        1
    );
    let retained = destination.checkpoint_persistent_actors(LIMIT)?;
    assert_eq!(retained.actors.len(), 1);
    assert_eq!(retained.actors[0].environment_id, 29);
    assert_eq!(
        fixture.eval(&mut destination, "python[29]^(unrelated)_python[29]")?,
        OValue::int(77)
    );
    assert_eq!(destination.pending_persistent_actor_restores(), 0);
    for id in [17, 18] {
        assert_eq!(
            fixture.eval(
                &mut source,
                &format!("python[{id}]^(__oval_result__ = x)_python[{id}]")
            )?,
            OValue::int(id)
        );
    }
    // Rollback retires only transaction-owned destination actors; their IDs
    // remain available for a later fresh admission.
    assert_eq!(
        fixture.eval(&mut destination, "python[17]^(42)_python[17]")?,
        OValue::int(42)
    );
    Ok(())
}

#[test]
fn local_recovery_restores_after_source_evaluator_is_gone_without_executing_probe() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let snapshot = {
        let mut source = fixture.evaluator();
        fixture.eval(
            &mut source,
            "python[17]^(saved = {'answer': 42}\n__oval_result__ = 0)_python[17]",
        )?;
        source.checkpoint_persistent_actors(LIMIT)?
    };
    let mut destination = fixture.evaluator();
    let targets = vec![fixture.target(&mut destination, "python", 17)?];
    let receipt = restore_persistent_actors(&mut destination, &snapshot, targets, LIMIT)?;
    assert_eq!(receipt.backend_receipts.len(), 1);
    assert_eq!(
        fixture.eval(
            &mut destination,
            "python[17]^(__oval_result__ = saved['answer'])_python[17]"
        )?,
        OValue::int(42)
    );
    Ok(())
}

#[test]
fn foreign_admission_and_missing_target_leave_source_usable() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let mut source = fixture.evaluator();
    let mut destination = fixture.evaluator();
    let mut foreign = fixture.evaluator();
    fixture.eval(
        &mut source,
        "python[17]^(x = 42\n__oval_result__ = x)_python[17]",
    )?;
    let target = fixture.target(&mut foreign, "python", 17)?;
    let error =
        migrate_persistent_actors(&mut source, &mut destination, vec![target], LIMIT).unwrap_err();
    assert!(format!("{error:#}").contains("state.restore-origin"));
    let target = fixture.target(&mut destination, "python", 18)?;
    assert!(migrate_persistent_actors(&mut source, &mut destination, vec![target], LIMIT).is_err());
    assert_eq!(
        fixture.eval(&mut source, "python[17]^(x)_python[17]")?,
        OValue::int(42)
    );
    assert!(destination
        .checkpoint_persistent_actors(LIMIT)?
        .actors
        .is_empty());
    Ok(())
}

#[test]
fn pinned_native_state_is_not_migrated() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let mut source = fixture.evaluator();
    let mut destination = fixture.evaluator();
    fixture.eval(
        &mut source,
        "python[17]^(f = lambda: 42\n__oval_result__ = 0)_python[17]",
    )?;
    let target = fixture.target(&mut destination, "python", 17)?;
    let error =
        migrate_persistent_actors(&mut source, &mut destination, vec![target], LIMIT).unwrap_err();
    assert!(
        format!("{error:#}").contains("state.pin-required"),
        "{error:#}"
    );
    assert_eq!(
        fixture.eval(&mut source, "python[17]^(f())_python[17]")?,
        OValue::int(42)
    );
    assert!(destination
        .checkpoint_persistent_actors(LIMIT)?
        .actors
        .is_empty());
    Ok(())
}

#[test]
fn recovery_rejects_runtime_generation_and_sandbox_substitution_before_launch() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let mut source = fixture.evaluator();
    fixture.eval(
        &mut source,
        "python[17]^(x = 42\n__oval_result__ = x)_python[17]",
    )?;
    let snapshot = source.checkpoint_persistent_actors(LIMIT)?;
    for field in ["runtime", "generation", "sandbox"] {
        let mut altered = snapshot.clone();
        let actor = &mut altered.actors[0];
        match field {
            "runtime" => {
                actor.runtime_binding_sha256 = "00".repeat(32);
                actor.checkpoint.runtime_binding_sha256 = actor.runtime_binding_sha256.clone();
            }
            "generation" => actor.launch_generation_sha256 = "00".repeat(32),
            "sandbox" => {
                actor.sandbox_permissions.clear();
                actor.sandbox_policy_sha256 = o_lang::backend_state::sandbox_policy_sha256(&[])?;
            }
            _ => unreachable!(),
        }
        altered.validate()?;
        let mut destination = fixture.evaluator();
        let target = fixture.target(&mut destination, "python", 17)?;
        let error =
            restore_persistent_actors(&mut destination, &altered, vec![target], LIMIT).unwrap_err();
        let expected = match field {
            "runtime" => "state.restore-runtime-mismatch",
            "generation" => "state.restore-generation-mismatch",
            _ => "state.restore-targets",
        };
        assert!(
            format!("{error:#}").contains(expected),
            "{field}: {error:#}"
        );
        assert_eq!(destination.pending_persistent_actor_restores(), 0);
        assert!(destination
            .checkpoint_persistent_actors(LIMIT)?
            .actors
            .is_empty());
    }
    assert_eq!(
        fixture.eval(&mut source, "python[17]^(x)_python[17]")?,
        OValue::int(42)
    );
    Ok(())
}
