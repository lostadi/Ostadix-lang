//! Source-backed toolchain discovery. Discovery never starts a child process.
//!
//! A command being located is deliberately weaker than runtime readiness or
//! installed-version support for a source-documented flag. The CLI remains the
//! authority for accepted arguments; this module does not narrow its flags.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

#[derive(Clone, Copy)]
enum Launch {
    Binary,
    Script(&'static str),
    LanguageServer,
}

struct Capability {
    id: &'static str,
    family: &'static str,
    summary: &'static str,
    program: &'static str,
    launch: Launch,
    help: bool,
    docs: &'static [&'static str],
    guide: &'static str,
}

macro_rules! binary {
    ($id:literal, $family:literal, $summary:literal, $help:literal, $guide:literal, $($doc:literal),+ $(,)?) => {
        Capability { id: $id, family: $family, summary: $summary, program: $id,
            launch: Launch::Binary, help: $help, docs: &[$($doc),+], guide: $guide }
    };
}

macro_rules! script {
    ($id:literal, $family:literal, $summary:literal, $path:literal, $interpreter:literal, $guide:literal, $($doc:literal),+ $(,)?) => {
        Capability { id: $id, family: $family, summary: $summary, program: $path,
            launch: Launch::Script($interpreter), help: true, docs: &[$($doc),+], guide: $guide }
    };
}

// Keep the Cargo binary coverage test below when adding a workspace command.
static COMMANDS: &[Capability] = &[
    script!("o", "front-door", "Repository dispatcher for execution, planning, evidence, nodes, Worlds, capacity, devices and kernels.", "scripts/o-cli.sh", "sh", "all", "AGENTS.md", "scripts/o-cli.sh"),
    binary!("O", "runtime", "Evaluate or parse O source; inline expressions, JSON, REPL, graph workers, morphism contracts and crossing evidence.", true, "runtime", "src/main.rs", "README.md"),
    binary!("o-cli", "projects", "Full intent CLI: run, routes, optimize, plan, explain, inspect, computation, objects, operation, realizations, observe and replan.", true, "projects", "src/bin/o-cli.rs", "docs/OPERATION_PLANNING_V1.md"),
    binary!("olangc", "compiler", "Native AOT, WASI, browser bundles, generated-project materialization, runtime bundles, script execution, IR/DOT and schedule analysis.", true, "compiler", "src/bin/olangc.rs", "docs/EMBEDDED_RUNTIME_BUNDLES.md", "docs/LINUX_RUNTIME_ROOTFS.md"),
    binary!("ocorec", "core", "Compile O-core modules to AST, HIR, MIR, assembly or objects for x86_64 and aarch64 freestanding targets.", true, "core", "src/bin/ocorec.rs", "docs/OCORE.md"),
    binary!("o-link", "projects", "Literal linking, inert project lifting, route discovery and policies, parallel groups, mesh execution and trace/selection receipts.", true, "projects", "src/bin/olink.rs", "docs/PROJECT_MESH_V1.md"),
    binary!("o-unlink", "projects", "Recover source files from linked O documents or project bundles; dry-run lists files without extraction.", true, "projects", "src/bin/ounlink.rs", "README.md"),
    binary!("o-notebook", "runtime", "Persistent notebook evaluation server; positional backend directory, optional browser opener, long-running process.", false, "runtime", "src/bin/o-notebook.rs"),
    binary!("ogit", "evidence", "Semantic source comparison and executable semantic-receipt demonstration.", true, "live", "src/bin/ogit.rs", "docs/SEMANTIC_CUSTODY.md"),
    binary!("o-live-host", "live", "Package validation/install, activation/upgrade/rollback, service invocation/composition/restart and World status.", true, "live", "src/bin/o-live-host.rs", "docs/LIVE_SYSTEM.md"),
    binary!("o-node", "mesh", "Local hosted node lifecycle, pairing, TLS/identity, durable-state administration, catalog/doctor and authenticated serving.", true, "mesh", "src/bin/o-node.rs", "docs/PROJECT_MESH_V1.md"),
    binary!("octl", "mesh", "Remote node selection/profile/doctor/run and durable sessions, asynchronous operations, actors and warrant-bound recovery.", true, "mesh", "src/bin/octl.rs", "docs/PROJECT_MESH_V1.md"),
    binary!("o-registry", "mesh", "Signed node-profile registry initialization, local fingerprinting, publication, verification and import/export.", true, "mesh", "src/bin/o-registry.rs"),
    binary!("o-info", "evidence", "Signed public scalar information packs, immutable snapshot heads, verification and conservative import.", true, "live", "src/bin/o-info.rs"),
    binary!("ostadix-device", "device", "Android/Termux device state, diagnostics, deployment and device operations through the canonical controller.", true, "device", "src/bin/ostadix-device.rs", "README.md"),
    binary!("ocore-kernel-world-record", "core", "Convert explicit O-core kernel artifacts into the typed Kernel World record.", true, "core", "src/bin/ocore-kernel-world-record.rs", "docs/KERNEL_WORLD_CONTRACT.md"),
    script!("kernel", "core", "O-core build/boot/console, QEMU gates, disk/ISO/foreign guest media, hosted-live release and physical evidence workflows.", "scripts/o-kernel.sh", "bash", "core", "scripts/o-kernel.sh", "docs/OSTADIX_BOOT.md"),
    script!("capacity", "capacity", "Inspect/install exact absorbed-capacity closures, verify blobs, plan/apply activation, rollback and report unreachable objects.", "scripts/ostadix_capacity.py", "python3", "capacity", "docs/ABSORBED_CAPACITY.md", "evidence/absorbed_capacity_catalog.toml"),
    script!("foreign-kernel-lab", "core", "Inspect, acquire, run and verify the catalogued foreign-kernel laboratory profiles.", "scripts/foreign_kernel_lab.py", "python3", "core", "docs/FOREIGN_KERNEL_LAB.md", "evidence/foreign_kernel_lab.toml"),
    script!("runtime-rootfs", "compiler", "Collect an explicitly supplied Linux executable/runtime rootfs closure for supported runtime packaging.", "scripts/collect_runtime_rootfs.py", "python3", "compiler", "docs/LINUX_RUNTIME_ROOTFS.md"),
    script!("release-evidence", "evidence", "Inspect and validate manifest-backed release artifacts and qualification evidence.", "scripts/release_evidence.py", "python3", "live", "scripts/release_evidence.py"),
    script!("setup", "agents", "Canonical installer/build and dependency diagnostics; platform-specific setup remains owned by setup.sh.", "setup.sh", "bash", "agents", "setup.sh", "AGENTS.md"),
    Capability {
        id: "ostadix-lsp", family: "agents", summary: "O source language server over its native protocol; Python packages are required and availability does not prove dependency readiness.",
        program: "tools/lsp/server.py", launch: Launch::LanguageServer, help: false,
        docs: &["tools/lsp/server.py"], guide: "agents",
    },
];

const GUIDE_TOPICS: &[&str] = &[
    "all", "runtime", "compiler", "projects", "mesh", "core", "live", "capacity", "device",
    "agents",
];

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn physical_executable(path: PathBuf) -> Option<PathBuf> {
    // Keep executable symlinks intact: argv[0] selects multicall behavior and
    // a venv Python locates pyvenv.cfg relative to its invocation path.
    is_executable(&path)
        .then(|| std::path::absolute(path).ok())
        .flatten()
}

fn path_executable(name: &str) -> Option<PathBuf> {
    // All callers supply compile-time names from the catalog or interpreter
    // table, never caller-provided command/path strings.
    which::which(name).ok().and_then(physical_executable)
}

fn binary_path(root: &Path, name: &str) -> Option<PathBuf> {
    let filename = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    physical_executable(root.join("target/release").join(&filename))
        .or_else(|| physical_executable(root.join("target/debug").join(&filename)))
        .or_else(|| path_executable(name))
}

fn script_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = root.join(relative);
    if !path.is_file() {
        return Err(format!(
            "required repository script is missing: {}",
            path.display()
        ));
    }
    path.canonicalize().map_err(|error| {
        format!(
            "cannot resolve repository script {}: {error}",
            path.display()
        )
    })
}

fn resolve(root: &Path, command: &Capability) -> Result<(PathBuf, Vec<String>), String> {
    match command.launch {
        Launch::Binary => binary_path(root, command.program)
            .map(|path| (path, Vec::new()))
            .ok_or_else(|| format!("{} is unavailable in repository release/debug binaries or PATH; use the canonical setup workflow", command.id)),
        Launch::Script(interpreter) => {
            let script = script_path(root, command.program)?;
            let executable = path_executable(interpreter)
                .ok_or_else(|| format!("{interpreter} is required to launch {}", command.id))?;
            Ok((executable, vec![script.to_string_lossy().into_owned()]))
        }
        Launch::LanguageServer => {
            let script = script_path(root, command.program)?;
            // The checked-in shell wrapper contains an absolute developer
            // directory. Launch the repository's script with its own venv when
            // available, retaining a portable python3 fallback.
            let executable = physical_executable(root.join("tools/lsp/venv/bin/python3"))
                .or_else(|| physical_executable(root.join("tools/lsp/venv/Scripts/python.exe")))
                .or_else(|| path_executable("python3"))
                .ok_or_else(|| "python3 is required to launch ostadix-lsp".to_string())?;
            Ok((executable, vec![script.to_string_lossy().into_owned()]))
        }
    }
}

/// Resolve an exact catalog identifier. Arguments are added by the process
/// runner as individual argv entries, so the complete native CLI remains usable.
pub fn resolve_command(root: &Path, name: &str) -> Result<(PathBuf, Vec<String>), String> {
    let command = COMMANDS.iter().find(|entry| entry.id == name)
        .ok_or_else(|| format!("unknown Ostadix command {name:?}; use the capability catalog for exact command identifiers"))?;
    resolve(root, command)
}

/// Return compact source-backed capabilities plus current executable discovery.
/// This performs filesystem/PATH lookup only, including for optional tools.
pub fn catalog(root: &Path, query: Option<&str>) -> Value {
    let query = query.unwrap_or_default().trim().to_ascii_lowercase();
    let commands: Vec<Value> = COMMANDS
        .iter()
        .filter(|entry| {
            query.is_empty()
                || format!(
                    "{} {} {} {} {} {}",
                    entry.id,
                    entry.family,
                    entry.summary,
                    entry.program,
                    entry.guide,
                    entry.docs.join(" ")
                )
                .to_ascii_lowercase()
                .contains(&query)
        })
        .map(|entry| {
            let resolution = resolve(root, entry);
            let (path, prefix, reason) = match resolution {
                Ok((path, prefix)) => (Some(path.to_string_lossy().into_owned()), prefix, None),
                Err(reason) => (None, Vec::new(), Some(reason)),
            };
            json!({
                "id": entry.id,
                "family": entry.family,
                "summary": entry.summary,
                "program": entry.program,
                "available": path.is_some(),
                "resolved_path": path,
                "prefix_args": prefix,
                "unavailable_reason": reason,
                "help_args": if entry.help { Some(vec!["--help"]) } else { None },
                "docs": entry.docs,
                "guide_topic": entry.guide,
            })
        })
        .collect();
    json!({
        "schema": "ostadix.mcp-capabilities/v1",
        "root": root,
        "source_catalog": true,
        "runtime_readiness_verified": false,
        "installed_help_verified": false,
        "availability_meaning": "Executable or script/interpreter located only. Installed CLI flags may lag this source catalog; inspect explicit help before using unfamiliar flags. Discovery never executes help or checks dependencies.",
        "resolution_order": ["repository target/release", "repository target/debug", "known program name on PATH"],
        "commands": commands,
        "guide_topics": GUIDE_TOPICS,
    })
}

/// Embedded portable workflows, available without access to repository docs.
pub fn guide(topic: &str) -> Option<&'static str> {
    match topic {
        "all" => Some(ALL_GUIDE),
        "runtime" | "backends" | "graph" | "notebook" => Some(RUNTIME_GUIDE),
        "compiler" | "packaging" | "wasm" => Some(COMPILER_GUIDE),
        "projects" | "link" | "routes" | "operations" => Some(PROJECTS_GUIDE),
        "mesh" | "node" | "sessions" => Some(MESH_GUIDE),
        "core" | "ocore" | "kernel" | "vm" => Some(CORE_GUIDE),
        "live" | "worlds" | "evidence" | "receipts" | "info" => Some(LIVE_GUIDE),
        "capacity" => Some(CAPACITY_GUIDE),
        "device" | "android" | "termux" => Some(DEVICE_GUIDE),
        "agents" | "setup" | "lsp" => Some(AGENTS_GUIDE),
        _ => None,
    }
}

const ALL_GUIDE: &str = r#"Ostadix agent workflow
Start with o_env, o_runtimes and o_doctor. Discover capabilities by family, then inspect explicit supported help through the command runner. Catalog descriptions are source-backed; a located executable may be older and does not prove runtime readiness. No discovery call starts daemons or probes optional services.
Use direct O source execution for polyglot computation; o-cli for projects/plans/retained evidence; olangc for compiler outputs; o-link/o-unlink for packaging; octl/o-node for hosted sessions; live/info/registry for Worlds and signed records; kernel/capacity for O-core, boot media and absorbed systems.
All native argument forms remain accessible through the named-command runner. Pass arguments as separate strings, use an explicit working directory and absolute backend paths. Use independent jobs for long builds, servers, interactive processes and large output; poll/retrieve retained output and cancel only the job you own. Preserve runtime graph/mesh worker capacity.
Choose guide topics runtime, compiler, projects, mesh, core, live, capacity, device or agents. Source guides describe capabilities, not proof that optional runtimes, devices, peers or current artifacts have been qualified."#;

const RUNTIME_GUIDE: &str = r#"Runtime, backends and graphs
Use O with argv [FILE.O, ABSOLUTE_BACKENDS]. O also accepts --eval EXPR, --json, --check, --crossing-evidence, --morphism-contract, --backend-grant, --executor graph|serial and --workers N. Inspect current help; graph is the source default, and useful worker concurrency must be preserved. O version --json reports compatibility identities.
O source splices $IDENT: never insert shell $VAR syntax in source. Resolve filesystem paths before constructing source or argv. Inline source, existing files and an explicit cwd should all remain usable. The canonical backend catalog describes ordered alternative runtime requirements and typed capabilities; located commands alone do not prove evaluator or native-object closure.
For static graph inspection use olangc FILE --target ir --explain-schedule --format json, or --execution-intent-json separately. Grounding, World ID/epoch and --why PN are distinct inspection views. Runtime crossing evidence reports actual observed boundaries.
O --repl needs a managed interactive process. o-notebook takes an absolute backend directory and starts a persistent server; set OSTADIX_NOTEBOOK_NO_OPEN=1 for headless use. Notebook and LSP do not have a source-supported --help probe. Use a persistent job for services and preserve stdin/outputs through its lifecycle."#;

const COMPILER_GUIDE: &str = r#"Compiler and runtime packaging
Resolve source and output paths against an explicit cwd. olangc FILE --target ir|dot performs non-executing analysis; --target script executes. --target binary is native AOT, and --target wasm produces WASI. Supply --shim-dir ABSOLUTE_BACKENDS when selecting compatibility adapters.
Source supports --materialize-only NEW_DIR for the exact generated Cargo project without Cargo, --browser-bundle NEW_DIR for the browser WASI host, and --runtime-bundle DIR for an explicitly supplied relocatable runtime tree. Check installed help first: binary builds can lag these source flags. AOT invokes Cargo; run long work as a managed job and use an isolated worktree/approved build environment when live development must be preserved.
Runtime bundles embed supplied files, not an inferred universal OS/library/service closure. The runtime-rootfs command provides the canonical Linux collector; inspect its help and docs/LINUX_RUNTIME_ROOTFS.md for its supported boundary. Current compiler internals embed a runtime snapshot, so rebuilding an app with an old compiler does not refresh that snapshot.
Use compiler --route/--routes-policy for project routes, --project-trace-out for supported script evidence, and separate schedule/intent/grounding views. Native O-core compilation is a separate ocorec command; see core guide."#;

const PROJECTS_GUIDE: &str = r#"Projects, linking, operation planning and selection
Inspect before choosing a route: o-cli routes TARGET --json, or o-link TARGET --list-routes on older installations. o-link --project DIR -o BUNDLE.O creates an inert route-preserving project bundle. Bare single-directory o-link implicitly executes selected literal code; --literal explicitly links without that inferred execution. o-unlink BUNDLE.O --dry-run previews source recovery.
o-cli run TARGET accepts --route, --routes-policy, repeated --route-decl, --parallel auto, mesh controls, trace exports, --json and durable record controls. Explicit o-cli bypasses shell alias/dispatcher drift. Route policies include explicit/default/fallback/any_success/race_success/race_settle/all/verify_equivalent/benchmark_and_select/benchmark_validate_and_select; native CLI validates combinations.
o-cli optimize TARGET --route ROUTE_SET --json executes reference and all candidates and verifies declared outputs before selection. Its measured invocation is not itself accelerated. o-cli run TARGET --selection-run RUN_ID reuses one exact validated winner only when current reuse checks pass.
Source supports operation PROJECT, realizations PROJECT, plan PROJECT --explain, observe PROJECT and replan PROJECT --without-target ID for explicitly marked operation projects. Record inspection/verification is inert. A descriptive replan does not dispatch recovery. Use inspect/explain/computation for retained evidence and canonical semantic custody; check installed help for source-version drift."#;

const MESH_GUIDE: &str = r#"Hosted nodes, placement and durable sessions
Use o-node status for local lifecycle state; o-node profile describes the local catalog, and doctor separately checks readiness. o-node start/stop/restart/pair/pki/identity/admin/serve own node lifecycle and durable state. A healthy daemon can hold a state lock: inspect identity/ownership before changing state; do not start duplicates or delete a lock by assumption.
octl node list/use/profile/doctor/run access remote nodes. octl node session start/send/info/stop provides an automatically managed session; run provides a complete session invocation. Expert sessions expose principal/open/exec/status/actors/reset/recover/close with explicit capabilities and signed leases. Poll asynchronous exec status; preserve operation and actor generation identities, ambiguity and recovery warrants.
o-cli run PROJECT --parallel auto uses eligible peers with the documented fallback. Explicit --mesh=required requires remote placement; use --mesh-retries, --mesh-local-fallback, --closed-registry/--mesh-peer-root and trace paths for exact control. Do not replace required remote placement with silent local success.
o-registry exposes init/profile-local/publish-profile/verify/list/export/import. Registry profiles describe exact fingerprints and freshness; they are not placement warrants. All canonical CLI arguments, transport paths and supported authority operations remain reachable through the command runner; discovery itself never enrolls or contacts peers."#;

const CORE_GUIDE: &str = r#"O-core, kernels, boot media and guests
ocorec MODULE.oc --emit ast|hir|mir|asm|obj --target x86_64-unknown-none|aarch64-unknown-none -o OUTPUT compiles native modules. Multiple modules, --keep-asm and --function-sections remain native options. ocore-kernel-world-record consumes explicit artifacts for the typed record.
The kernel command is scripts/o-kernel.sh, equivalent to o kernel. Its explicit help lists doctor/build/image/boot/console/smoke/gates, media/ISO/capacity-ISO creation and inspection, hosted-live release/smoke, Ventoy installation, physical media writing and challenged boot observations. Run lengthy QEMU/build tasks as independent jobs; interactive console requires stdin/PTY support. Keep existing native confirmation-token contracts for concrete media operations.
The foreign-kernel-lab command owns catalogued foreign guest acquisition/run/evidence. The ubuntu_vm backend (alias ubuntu) is a separate hosted backend; do not invent a universal o vm command. A hosted foreign guest is distinct from O-core native governance, hardware virtualization and physical boot qualification.
Build, artifact digest, QEMU/menu boot, physical boot and device support are separate evidence. Read docs/OSTADIX_BOOT.md, docs/OCORE.md and docs/FOREIGN_KERNEL_LAB.md for the current bounded profiles."#;

const LIVE_GUIDE: &str = r#"Live Worlds, signed information and retained evidence
o-live-host pack verifies an explicit manifest/payload; install stores it in immutable CAS; activate/upgrade/rollback manage healthy generations. invoke/compose call active services, restart changes one service, and status reconstructs/health-checks the active set. These are native lifecycle operations with explicit state directories and policy arguments.
ogit diff-semantic compares two sources. ogit demo semantic-receipt (also bare o receipt) executes a demonstration and writes receipts; it is not passive receipt inspection. o-cli inspect/explain examine retained execution, and computation binds canonical semantic-custody artifacts.
o-info init/keygen/record/verify/import/head manages signed public information packs and immutable snapshots. Record requires an explicit acknowledgement of public data. o-registry manages signed node profiles; see mesh guide.
release-evidence validates manifest-backed evidence. Distinguish descriptive records, verified bytes, signatures, successful local execution and remote/hardware qualification. Preserve exact source/runtime/graph/attempt identities and failed outcomes; do not promote a package inventory to executable dependency-closure proof."#;

const CAPACITY_GUIDE: &str = r#"Absorbed capacity and exact activation
The capacity command runs scripts/ostadix_capacity.py, equivalent to o capacity. Use --state and --catalog before its subcommand where required by argparse. inspect PACKAGE examines catalog metadata; install PACKAGE installs its exact dependency closure. list/show/verify/status distinguish installed blobs from active generations and qualified evidence.
plan REFERENCES creates an exact revision-bound activation plan; apply PLAN performs the native compare-and-swap activation; rollback swaps retained generations. gc --dry-run reports unreachable objects without deletion. Canonical source overrides, aliases and license arguments remain accessible through argv.
Catalog: evidence/absorbed_capacity_catalog.toml; contract: docs/ABSORBED_CAPACITY.md. Package names or an active catalog generation do not prove every guest booted, dependencies are universally hermetic, or physical hardware was qualified. Use kernel/foreign-kernel-lab workflows to obtain the corresponding concrete evidence."#;

const DEVICE_GUIDE: &str = r#"Android and device access
Use ostadix-device, also routed by o device. Inspect its current --help and subcommand help for status/doctor, device/root/prime and deployment operations. Pass complete canonical arguments and an explicit environment/cwd through the command runner.
Source availability on macOS/Linux does not prove Android/Termux runtime readiness or connected-device state. Locate the actual device, transport and tool requirements with native status/doctor before executing the intended device workflow. Keep managed jobs for interactive/long-lived processes and report the actual substrate tested."#;

const AGENTS_GUIDE: &str = r#"Agent integration and development tools
The MCP catalog, named-command runner and embedded workflow guides are client-independent; they cover all declared main-repository Cargo binaries plus canonical dispatcher/kernel/capacity, setup, foreign-kernel/rootfs/evidence scripts and the LSP. Tools accept complete native argv rather than a permanently narrowed subset of flags. Treat source capability descriptions and installed executable support as separate states.
Use setup as the canonical setup.sh entry for dependency checks or complete installs/builds. Prefer explicit help to discover current platform options. Preserve active development, uncommitted files and configured worker capacity; use isolated builds when required. The caller owns installation/publication decisions, and the MCP must not perform them while merely discovering capabilities.
ostadix-lsp launches tools/lsp/server.py with the repository venv when available, else python3. Its pygls/lsprotocol dependencies are not checked by catalog discovery. It speaks LSP over stdin/stdout, so use an actual protocol client; --help is not supported. Notebook similarly requires an explicit persistent process.
After rebuilding MCP or changing client registrations, a running client may retain its old server process and tool schemas until reconnect/restart. A successful isolated test does not establish that every already-running agent has refreshed."#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_declared_cargo_binary_has_exact_catalog_coverage() {
        let manifest = include_str!("../../../Cargo.toml");
        let binaries: BTreeSet<&str> = manifest
            .split("[[bin]]")
            .skip(1)
            .filter_map(|block| {
                block.lines().find_map(|line| {
                    line.strip_prefix("name = \"")
                        .and_then(|name| name.strip_suffix('"'))
                })
            })
            .collect();
        let listed: BTreeSet<&str> = COMMANDS
            .iter()
            .filter(|entry| matches!(entry.launch, Launch::Binary))
            .map(|entry| entry.id)
            .collect();
        assert_eq!(listed, binaries);
        assert_eq!(listed.len(), 15);
    }

    #[test]
    fn catalog_ids_are_unique_and_all_guides_exist() {
        let ids: BTreeSet<&str> = COMMANDS.iter().map(|entry| entry.id).collect();
        assert_eq!(ids.len(), COMMANDS.len());
        for entry in COMMANDS {
            assert!(
                guide(entry.guide).is_some(),
                "missing guide for {}",
                entry.id
            );
            assert!(!entry.docs.is_empty());
        }
        for topic in GUIDE_TOPICS {
            assert!(guide(topic).is_some());
        }
        assert!(guide("unknown-topic").is_none());
    }

    #[test]
    fn arbitrary_programs_and_path_inputs_never_resolve() {
        for name in [
            "sh",
            "python3",
            "/bin/sh",
            "../O",
            "o; touch file",
            "",
            "O --help",
        ] {
            let error =
                resolve_command(Path::new("/nonexistent-ostadix-catalog-test"), name).unwrap_err();
            assert!(
                error.starts_with("unknown Ostadix command"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn discovery_is_explicitly_source_backed_and_never_probes_services() {
        let value = catalog(Path::new("/nonexistent-ostadix-catalog-test"), None);
        assert_eq!(value["source_catalog"], true);
        assert_eq!(value["runtime_readiness_verified"], false);
        assert_eq!(value["installed_help_verified"], false);
        let commands = value["commands"].as_array().unwrap();
        for id in ["o-notebook", "ostadix-lsp"] {
            let item = commands.iter().find(|entry| entry["id"] == id).unwrap();
            assert!(item["help_args"].is_null());
        }
        let dispatcher = commands.iter().find(|entry| entry["id"] == "o").unwrap();
        assert_eq!(dispatcher["available"], false);
        assert!(dispatcher["unavailable_reason"]
            .as_str()
            .unwrap()
            .contains("missing"));
    }

    #[test]
    fn discovery_searches_descriptions_without_case_sensitivity() {
        let root = Path::new("/nonexistent-ostadix-catalog-test");
        let wasm = catalog(root, Some(" WASI "));
        assert!(wasm["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == "olangc"));
        assert!(
            catalog(root, Some("unfindable-capability-token"))["commands"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn pinned_binary_precedes_path_and_script_prefix_preserves_spaces() {
        use std::os::unix::fs::PermissionsExt;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("ostadix catalog {} {nonce}", std::process::id()));
        std::fs::create_dir_all(root.join("target/release")).unwrap();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        let bin = root.join("target/release/O");
        std::fs::write(&bin, "#!/bin/sh\nexit 97\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let script = root.join("scripts/o-cli.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 98\n").unwrap();
        let (resolved, prefix) = resolve_command(&root, "O").unwrap();
        assert_eq!(resolved, std::path::absolute(bin).unwrap());
        assert!(prefix.is_empty());
        let multicall = root.join("target/release/ocorec");
        std::os::unix::fs::symlink("O", &multicall).unwrap();
        let (resolved, _) = resolve_command(&root, "ocorec").unwrap();
        assert_eq!(resolved, std::path::absolute(multicall).unwrap());
        let (interpreter, prefix) = resolve_command(&root, "o").unwrap();
        assert!(interpreter.is_absolute());
        assert_eq!(
            prefix,
            vec![script
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()]
        );
        assert_eq!(
            catalog(&root, Some("front-door"))["commands"][0]["available"],
            true
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
