//! ONative owner handles through real OIR dispatch and Python shim processes.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use o_lang::eval::migration::migrate_persistent_actors;
use o_lang::eval::{Evaluator, PreparedPlacementFragmentV2};
use o_lang::parser::Parser;
use o_lang::placement::{GenerationV1, SemanticDigestV1, TaskAttemptIdV1};
use o_lang::value::{NativeCodecSafety, OValue, RehydratePolicy};

struct Fixture {
    _root: tempfile::TempDir,
    shims: PathBuf,
    runtime: PathBuf,
    backends: HashSet<String>,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let shims = root.path().join("backends");
        fs::create_dir(&shims)?;
        for (name, bytes) in o_lang::shims::read_shims(None)? {
            fs::write(shims.join(name), bytes)?;
        }
        let runtime = root.path().join("O-native-test");
        fs::copy(env!("CARGO_BIN_EXE_O"), &runtime)?;
        Ok(Self {
            _root: root,
            shims,
            runtime,
            backends: HashSet::from(["python".into(), "javascript".into(), "text".into()]),
        })
    }

    fn evaluator(&self) -> Evaluator {
        Evaluator::new(self.shims.clone())
            .with_registered_backends(self.backends.clone())
            .with_runtime_executable(self.runtime.clone())
    }

    fn eval(
        &self,
        evaluator: &mut Evaluator,
        source: &str,
        scope: &mut HashMap<String, OValue>,
    ) -> Result<OValue> {
        evaluator.eval_document_with_scope(Parser::new(source, &self.backends).parse()?, scope)
    }

    fn eval_mode(
        &self,
        evaluator: &mut Evaluator,
        source: &str,
        scope: &mut HashMap<String, OValue>,
        serial: bool,
    ) -> Result<OValue> {
        let program = o_lang::ir::OIrProgram::lower(&Parser::new(source, &self.backends).parse()?);
        if serial {
            evaluator.eval_ir_program_serial_with_scope(&program, scope)
        } else {
            evaluator.eval_ir_program_graph_with_scope(&program, scope)
        }
    }

    fn target(&self, evaluator: &mut Evaluator) -> Result<PreparedPlacementFragmentV2> {
        evaluator.prepare_placement_fragment(
            "python[7]^(raise RuntimeError('restore must not execute this body'))_python[7]",
            TaskAttemptIdV1::new(
                SemanticDigestV1::hash_bytes("ostadix/native-migration-test/v1", b"target"),
                GenerationV1::new(1)?,
            ),
        )
    }
}

const LIMIT: u64 = 4 * 1024 * 1024;

#[test]
fn javascript_operates_on_exact_python_object_without_reconstruction() -> Result<()> {
    for serial in [true, false] {
        let fixture = Fixture::new()?;
        let mut evaluator = fixture.evaluator();
        let mut scope = HashMap::new();
        let result = fixture.eval_mode(
            &mut evaluator,
            r#"
let handle = python[7]^(
class Box:
    def __init__(self): self.value = 0
    def __call__(self, value):
        self.value += value
        return self
    def same(self, other): return self is other
box = Box()
O.native(box)
)_python[7]
javascript^(
const again = O.native_call(handle, 40);
O.native_set(again, 'value', 42);
const method = O.native_get(again, 'same');
if (!O.native_call(method, handle)) throw Error('identity lost');
console.log(O.native_get(handle, 'value'));
O.native_release(method);
O.native_release(again);
)_javascript
"#,
            &mut scope,
            serial,
        )?;
        assert_eq!(result, OValue::int(42));
        assert_eq!(
            fixture.eval(
                &mut evaluator,
                "python[7]^(O.resolve_native($handle) is box and box.value == 42)_python[7]",
                &mut scope
            )?,
            OValue::bool_(true)
        );
    }
    Ok(())
}

#[test]
fn native_operations_reject_altered_and_released_handles_before_descriptor_effects() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut evaluator = fixture.evaluator();
    let mut scope = HashMap::new();
    let handle = fixture.eval(
        &mut evaluator,
        r#"
let handle = python[7]^(
effects = []
class Box:
    @property
    def value(self):
        effects.append('get')
        return 42
O.native(Box())
)_python[7]
$handle
"#,
        &mut scope,
    )?;
    let mut altered = handle.clone();
    if let OValue::Native { v } = &mut altered {
        v.type_name.push_str("-changed");
    }
    scope.insert("bad".into(), altered);
    let error = fixture
        .eval(
            &mut evaluator,
            "native_get($bad, text^(value)_text)",
            &mut scope,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("native.altered-handle"),
        "{error:#}"
    );
    assert_eq!(
        fixture.eval(
            &mut evaluator,
            "python[7]^(len(effects))_python[7]",
            &mut scope
        )?,
        OValue::int(0)
    );
    let result = fixture.eval(
        &mut evaluator,
        r#"
javascript^(
O.native_release(handle);
try { O.native_get(handle, 'value'); throw Error('released handle succeeded'); }
catch(error) { if (!String(error).includes('native.handle-expired')) throw error; }
console.log('expired');
)_javascript
"#,
        &mut scope,
    )?;
    assert_eq!(result, OValue::text("expired"));
    assert_eq!(
        fixture.eval(
            &mut evaluator,
            "python[7]^(len(effects))_python[7]",
            &mut scope
        )?,
        OValue::int(0)
    );
    // Replacing a physical owner cannot revive its deterministic logical session.
    fixture
        .eval(
            &mut evaluator,
            "python[7]^(import os; os._exit(0))_python[7]",
            &mut scope,
        )
        .unwrap_err();
    fixture.eval(&mut evaluator, "python[7]^(42)_python[7]", &mut scope)?;
    let error = fixture
        .eval(
            &mut evaluator,
            "native_get($handle, text^(value)_text)",
            &mut scope,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("native.owner-expired"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn owner_callback_reentry_orders_native_operations_without_reentering_exec() -> Result<()> {
    for serial in [true, false] {
        let fixture = Fixture::new()?;
        let mut evaluator = fixture.evaluator();
        let mut scope = HashMap::new();
        let result = fixture.eval_mode(&mut evaluator, r#"
let handle = python[7]^(
events = []
class Box:
    value = 42
    def __call__(self):
        events.append('enter')
        nested = O.native(self)
        result = O.eval("javascript" + "^(console.log(O.native_get(owner, 'value')))_javascript", O.scope({'owner': nested}))
        O.release_native(nested)
        events.append('leave')
        return int(result)
O.native(Box())
)_python[7]
javascript^(console.log(O.native_call(handle) + O.native_call(handle)))_javascript
"#, &mut scope, serial)?;
        assert_eq!(result, OValue::int(84));
        assert_eq!(
            fixture.eval(
                &mut evaluator,
                "python[7]^(events == ['enter', 'leave', 'enter', 'leave'])_python[7]",
                &mut scope
            )?,
            OValue::bool_(true)
        );
    }
    Ok(())
}

#[test]
fn foreign_python_and_autonomous_javascript_callbacks_use_the_same_owner() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut evaluator = fixture.evaluator();
    let mut scope = HashMap::new();
    fixture.eval(
        &mut evaluator,
        "let handle = python[7]^(O.native(lambda value: value + 2))_python[7]",
        &mut scope,
    )?;
    assert_eq!(
        fixture.eval(
            &mut evaluator,
            "python[8]^(O.native_call($handle, 40))_python[8]",
            &mut scope
        )?,
        OValue::int(42)
    );
    assert_eq!(
        fixture.eval(
            &mut evaluator,
            "autonomous(javascript^(console.log(O.native_call(handle, 40)))_javascript)",
            &mut scope
        )?,
        OValue::int(42)
    );
    assert_eq!(
        fixture.eval(
            &mut evaluator,
            "python[7]^(O.native_call($handle, 40))_python[7]",
            &mut scope
        )?,
        OValue::int(42)
    );
    assert_eq!(
        fixture.eval(
            &mut evaluator,
            "python^(O.native_call(O.native(lambda: 42)))_python",
            &mut scope
        )?,
        OValue::int(42)
    );
    Ok(())
}

#[test]
fn javascript_native_scalars_keep_big_integers_and_negative_zero_exact() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut evaluator = fixture.evaluator();
    let mut scope = HashMap::new();
    let result = fixture.eval(
        &mut evaluator,
        r#"
let identity = python[7]^(O.native(lambda value: value))_python[7]
javascript^(
const large = (1n << 180n) + 7n;
if (O.native_call(identity, large) !== large) throw Error('integer changed');
if (!Object.is(O.native_call(identity, -0), -0)) throw Error('signed zero changed');
const shared=[];
try { O.native_call(identity, [shared,shared]); throw Error('alias flattened'); }
catch(error) { if (!String(error).includes('native.aliased-argument')) throw error; }
console.log('exact');
)_javascript
"#,
        &mut scope,
    )?;
    assert_eq!(result, OValue::text("exact"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn native_operation_timeout_retires_the_owner_before_returning_failure() -> Result<()> {
    use std::process::Command;
    let fixture = Fixture::new()?;
    let pid_file = fixture._root.path().join("native-owner-pid");
    let source = format!(
        r#"
let handle = python[7]^(
import os, time
with open({}, 'w') as file: file.write(str(os.getpid()))
O.native(lambda: time.sleep(30))
)_python[7]
native_call($handle, python[8]^([])_python[8])
"#,
        serde_json::to_string(&pid_file.to_string_lossy())?
    );
    let output = Command::new(&fixture.runtime)
        .args(["--json", "--eval", &source])
        .arg(&fixture.shims)
        .env("O_BACKEND_OPERATION_TIMEOUT_MS", "100")
        .env("O_BACKEND_SHUTDOWN_TIMEOUT_MS", "1000")
        .output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did not answer within") || stderr.contains("native.operation-timeout"),
        "{stderr}"
    );
    let pid: i32 = fs::read_to_string(pid_file)?.parse()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "expired native owner {pid} remained alive"
    );
    Ok(())
}

#[test]
fn native_descriptor_round_trips_through_oir_and_a_foreign_python_actor() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut evaluator = fixture.evaluator();
    let mut scope = HashMap::new();
    let handle = fixture.eval(&mut evaluator,
        "let handle = python[7]^(shared = []\nshared.append(shared)\nvalue = {'left': shared, 'right': shared, 'function': (lambda: 42)}\nO.native(value))_python[7]\n$handle",
        &mut scope)?;
    let OValue::Native { v } = &handle else {
        panic!("not an ONative handle: {handle:?}")
    };
    assert_eq!(v.safety, NativeCodecSafety::LiveHandle);
    assert_eq!(v.rehydrate, RehydratePolicy::SameProcess);
    assert!(v.identity.stable.is_none() && v.identity.live.is_some() && v.payload.is_none());
    assert!(!handle.is_cache_safe() && !handle.is_replay_safe() && !handle.is_boot_persistable());
    let carried = fixture.eval(&mut evaluator, "python[8]^($handle)_python[8]", &mut scope)?;
    assert_eq!(carried, handle);
    let resolved = fixture.eval(&mut evaluator,
        "python[7]^(resolved = O.resolve_native($handle)\nresolved is value and resolved['left'] is resolved['right'] and resolved['left'][0] is resolved['left'] and resolved['function']() == 42)_python[7]",
        &mut scope)?;
    assert_eq!(resolved, OValue::bool_(true));
    let error = fixture
        .eval(
            &mut evaluator,
            "python[8]^(O.resolve_native($handle))_python[8]",
            &mut scope,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("native.owner-mismatch"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn live_handle_blocks_migration_without_killing_its_owner() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut source = fixture.evaluator();
    let mut destination = fixture.evaluator();
    let mut scope = HashMap::new();
    fixture.eval(
        &mut source,
        "let handle = python[7]^(O.native(lambda: 42))_python[7]\n$handle",
        &mut scope,
    )?;
    let target = fixture.target(&mut destination)?;
    let error =
        migrate_persistent_actors(&mut source, &mut destination, vec![target], LIMIT).unwrap_err();
    assert!(
        format!("{error:#}").contains("$native_handles"),
        "{error:#}"
    );
    assert!(destination
        .checkpoint_persistent_actors(LIMIT)?
        .actors
        .is_empty());
    assert_eq!(
        fixture.eval(
            &mut source,
            "python[7]^(O.resolve_native($handle)())_python[7]",
            &mut scope
        )?,
        OValue::int(42)
    );
    Ok(())
}

#[test]
fn release_unpins_portable_actor_state_for_real_migration() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut source = fixture.evaluator();
    let mut destination = fixture.evaluator();
    let mut scope = HashMap::new();
    fixture.eval(
        &mut source,
        "let handle = python[7]^(values = [20, 22]\nO.native(values))_python[7]\n$handle",
        &mut scope,
    )?;
    assert!(source.checkpoint_persistent_actors(LIMIT).is_err());
    fixture.eval(
        &mut source,
        "python[7]^(O.release_native($handle))_python[7]",
        &mut scope,
    )?;
    let target = fixture.target(&mut destination)?;
    let receipt = migrate_persistent_actors(&mut source, &mut destination, vec![target], LIMIT)?;
    assert!(receipt.source_shutdown_failures.is_empty());
    assert_eq!(
        fixture.eval(
            &mut destination,
            "python[7]^(sum(values))_python[7]",
            &mut HashMap::new()
        )?,
        OValue::int(42)
    );
    Ok(())
}

#[test]
fn fresh_actor_handle_expires_visibly_after_its_exporting_block_finishes() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut evaluator = fixture.evaluator();
    let mut scope = HashMap::new();
    let error = fixture.eval(&mut evaluator,
        "let handle = python^(O.native(lambda: 42))_python\npython^(O.resolve_native($handle)())_python",
        &mut scope).unwrap_err();
    assert!(
        format!("{error:#}").contains("native.owner-mismatch"),
        "{error:#}"
    );
    Ok(())
}
