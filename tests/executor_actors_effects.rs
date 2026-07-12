//! Actor-identity and effect-model tests for the graph executor.
//!
//! These cover: unique ephemeral actor identities (so unrelated ephemeral
//! computations never serialize), persistent-actor sharing, effect-declaration
//! parsing, and the conflict relation that gates parallel readiness.

use o_lang::executor::{
    ActorTable, DeclaredPurity, EffectDeclaration, EffectSummary, ResourceKey,
};
use o_lang::ir::{BackendRegistry, OIr, OIrProgram, PlanNodeKind};

fn shim(lang: &str) -> o_lang::ir::BackendInterface {
    BackendRegistry::global().interface_for(lang)
}

#[test]
fn ephemeral_blocks_get_unique_actor_identities() {
    // Two independent ephemeral (env_id == u32::MAX) python blocks.
    let program = OIrProgram {
        nodes: vec![
            OIr::Exec {
                lang: "python".into(),
                env_id: u32::MAX,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 1".into())],
            },
            OIr::Exec {
                lang: "python".into(),
                env_id: u32::MAX,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 2".into())],
            },
        ],
    };
    let plan = program.plan();
    let actors = ActorTable::build(&plan, |_, _| 0);

    let exec_ids: Vec<_> = plan
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, PlanNodeKind::Exec { .. }))
        .map(|n| n.id)
        .collect();
    assert_eq!(exec_ids.len(), 2);

    let a = actors.actor_for(exec_ids[0]).expect("actor for first exec");
    let b = actors.actor_for(exec_ids[1]).expect("actor for second exec");

    assert!(a.is_ephemeral() && b.is_ephemeral());
    assert_ne!(
        a.ephemeral_instance, b.ephemeral_instance,
        "each ephemeral block must get a unique instance id: {a:?} vs {b:?}"
    );
}

#[test]
fn persistent_same_env_shares_one_actor() {
    let program = OIrProgram {
        nodes: vec![
            OIr::Exec {
                lang: "python".into(),
                env_id: 3,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 1".into())],
            },
            OIr::Exec {
                lang: "python".into(),
                env_id: 3,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 2".into())],
            },
            OIr::Exec {
                lang: "python".into(),
                env_id: 4,
                attr: None,
                backend: shim("python"),
                body: vec![OIr::Text("__oval_result__ = 3".into())],
            },
        ],
    };
    let plan = program.plan();
    let actors = ActorTable::build(&plan, |_, _| 0);
    let exec_ids: Vec<_> = plan
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, PlanNodeKind::Exec { .. }))
        .map(|n| n.id)
        .collect();

    let a = actors.actor_for(exec_ids[0]).unwrap();
    let b = actors.actor_for(exec_ids[1]).unwrap();
    let c = actors.actor_for(exec_ids[2]).unwrap();

    // Same (lang, env) → same persistent actor identity.
    assert_eq!(a.persistent_id(), b.persistent_id());
    assert_eq!(a.persistent_id(), Some(("python".to_string(), 3)));
    // Different env of the same language → different actor.
    assert_ne!(a.persistent_id(), c.persistent_id());
}

#[test]
fn effect_declaration_parses_all_forms() {
    let decl = EffectDeclaration::parse(Some(
        "lazy, effects=unknown, reads=project:src+host:/etc/hosts, writes=env:PATH, serial=host",
    ));
    assert_eq!(decl.purity, Some(DeclaredPurity::Unknown));
    assert!(decl.reads.contains(&ResourceKey::ProjectPath("src".into())));
    assert!(decl
        .reads
        .contains(&ResourceKey::HostPath("/etc/hosts".into())));
    assert!(decl.writes.contains(&ResourceKey::EnvVar("PATH".into())));
    assert!(decl.serial_host);

    // Unknown / unrelated attributes must not break parsing.
    let plain = EffectDeclaration::parse(Some("lazy, defer"));
    assert!(plain.is_empty());
}

#[test]
fn pure_declaration_overrides_unknown_base() {
    // A shim block defaults to unknown/impure, but `effects=pure` makes it pure.
    let decl = EffectDeclaration::parse(Some("effects=pure"));
    let effective = decl.apply(EffectSummary::unknown());
    assert!(!effective.unknown, "declared-pure block must not be unknown");
    assert!(effective.deterministic);
}

#[test]
fn effect_conflicts_gate_parallel_readiness() {
    // Read/read: no conflict → may run in parallel.
    let mut read_a = EffectSummary::pure();
    read_a.reads.insert(ResourceKey::ProjectPath("data".into()));
    let mut read_b = EffectSummary::pure();
    read_b.reads.insert(ResourceKey::ProjectPath("data".into()));
    assert!(!read_a.conflicts_with(&read_b));

    // Write vs read on the same resource: conflict → must serialize.
    let mut write = EffectSummary::pure();
    write.writes.insert(ResourceKey::ProjectPath("data".into()));
    assert!(write.conflicts_with(&read_a));

    // Unknown conflicts conservatively.
    let unknown = EffectSummary::unknown();
    assert!(unknown.conflicts_with(&EffectSummary::unknown()));

    // Two pure, resource-free summaries never conflict.
    assert!(!EffectSummary::pure().conflicts_with(&EffectSummary::pure()));
}
