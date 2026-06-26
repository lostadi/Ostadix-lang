use std::collections::HashMap;

use crate::{
    ir::{InvokeMode, OIr, OIrProgram},
    value::{GroupMode, OValue},
};

use super::{
    graph::{ActorId, EdgeId, HEdge, HGraph, HNode, NodeId, Port, PortRole},
    kinds::{DomainFlags, OpKind, RepFlags},
};

pub fn build_program(program: &OIrProgram) -> HGraph {
    build(&program.nodes)
}

pub fn build(nodes: &[OIr]) -> HGraph {
    let mut builder = Builder {
        graph: HGraph::default(),
        scopes: vec![HashMap::new()],
    };
    let mut previous = None;
    for node in nodes {
        let id = builder.build_node(node);
        if let Some(prev) = previous {
            builder.add_sequence(prev, id);
        }
        previous = Some(id);
    }
    builder.graph
}

struct Builder {
    graph: HGraph,
    scopes: Vec<HashMap<String, NodeId>>,
}

impl Builder {
    fn build_node(&mut self, node: &OIr) -> NodeId {
        match node {
            OIr::Text(text) => {
                let id = self
                    .graph
                    .add_node(HNode::with_value(OValue::str_(text.clone())));
                if looks_like_integer(text) {
                    if let Ok(value) = text.trim().parse::<num_bigint::BigInt>() {
                        self.graph.add_edge(HEdge {
                            id: EdgeId(0),
                            kind: OpKind::Bounded { value },
                            ports: vec![Port {
                                node: id,
                                role: PortRole::Output,
                            }],
                        });
                    }
                } else if !text.is_empty() {
                    if let Some(node) = self.graph.node_mut(id) {
                        node.domain = DomainFlags::STRING;
                        node.rep = RepFlags::STR;
                    }
                }
                id
            }
            OIr::Load(name) => {
                let consumer = self.graph.add_node(HNode::fresh());
                if let Some(producer) = self.lookup(name) {
                    self.graph.add_edge(HEdge {
                        id: EdgeId(0),
                        kind: OpKind::DataFlow,
                        ports: vec![
                            Port {
                                node: producer,
                                role: PortRole::Input,
                            },
                            Port {
                                node: consumer,
                                role: PortRole::Output,
                            },
                        ],
                    });
                }
                consumer
            }
            OIr::Store { name, expr } => {
                self.scopes.push(HashMap::new());
                let value = self.build_node(expr);
                self.scopes.pop();
                self.bind(name.clone(), value);
                value
            }
            OIr::Invoke { mode, args, .. } => {
                self.scopes.push(HashMap::new());
                let mut child_ids = Vec::new();
                let mut previous = None;
                for arg in args {
                    let child = self.build_node(arg);
                    if let Some(prev) = previous {
                        self.add_sequence(prev, child);
                    }
                    previous = Some(child);
                    child_ids.push(child);
                }
                self.scopes.pop();

                let result = self.graph.add_node(HNode::fresh());
                let kind = match mode {
                    InvokeMode::Group(GroupMode::Batch) => OpKind::Batch,
                    InvokeMode::Group(GroupMode::All) => OpKind::All,
                    InvokeMode::Group(GroupMode::Any) => OpKind::Any,
                    InvokeMode::Group(GroupMode::Race) => OpKind::Race,
                    _ => OpKind::StructuralBarrier,
                };
                self.add_barrier(kind, &child_ids, result);
                result
            }
            OIr::Exec {
                lang, env_id, body, ..
            } => {
                self.scopes.push(HashMap::new());
                let mut child_ids = Vec::new();
                let mut previous = None;
                for child in body {
                    let child_id = self.build_node(child);
                    if let Some(prev) = previous {
                        self.add_sequence(prev, child_id);
                    }
                    previous = Some(child_id);
                    child_ids.push(child_id);
                }
                self.scopes.pop();

                let result = self.graph.add_node(HNode::fresh());
                self.add_barrier(OpKind::StructuralBarrier, &child_ids, result);
                for child in &child_ids {
                    self.graph.add_edge(HEdge {
                        id: EdgeId(0),
                        kind: OpKind::BackendCrossing {
                            from_lang: "O".to_string(),
                            to_lang: lang.clone(),
                        },
                        ports: vec![
                            Port {
                                node: *child,
                                role: PortRole::Input,
                            },
                            Port {
                                node: result,
                                role: PortRole::Output,
                            },
                        ],
                    });
                }
                if *env_id != u32::MAX {
                    let actor = ActorId {
                        lang: intern_lang(lang),
                        env: *env_id,
                    };
                    if let Some(node) = self.graph.node_mut(result) {
                        node.actor = Some(actor);
                    }
                    self.graph.add_edge(HEdge {
                        id: EdgeId(0),
                        kind: OpKind::ActorSerial { actor },
                        ports: vec![Port {
                            node: result,
                            role: PortRole::InOut,
                        }],
                    });
                }
                result
            }
        }
    }

    fn add_sequence(&mut self, before: NodeId, after: NodeId) {
        self.graph.add_edge(HEdge {
            id: EdgeId(0),
            kind: OpKind::Sequence,
            ports: vec![
                Port {
                    node: before,
                    role: PortRole::Input,
                },
                Port {
                    node: after,
                    role: PortRole::Output,
                },
            ],
        });
    }

    fn add_barrier(&mut self, kind: OpKind, inputs: &[NodeId], output: NodeId) {
        let mut ports = inputs
            .iter()
            .copied()
            .map(|node| Port {
                node,
                role: PortRole::Input,
            })
            .collect::<Vec<_>>();
        ports.push(Port {
            node: output,
            role: PortRole::Output,
        });
        self.graph.add_edge(HEdge {
            id: EdgeId(0),
            kind,
            ports,
        });
    }

    fn bind(&mut self, name: String, node: NodeId) {
        self.graph.bind(name.clone(), node);
        self.scopes
            .last_mut()
            .expect("scope stack always has a root")
            .insert(name, node);
    }

    fn lookup(&self, name: &str) -> Option<NodeId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .or_else(|| self.graph.lookup(name))
    }
}

fn looks_like_integer(text: &str) -> bool {
    let trimmed = text.trim();
    let rest = trimmed
        .strip_prefix('+')
        .or_else(|| trimmed.strip_prefix('-'))
        .unwrap_or(trimmed);
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn intern_lang(lang: &str) -> u32 {
    lang.bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32))
}
