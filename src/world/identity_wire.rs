//! Strict, identity-only binary records for crossing the hosted/native World
//! boundary.
//!
//! The format carries descriptive identities only. It is not a capability,
//! authority token, general protocol envelope, or proof of current state.

use thiserror::Error;

use super::identity::{
    AttemptGeneration, AttemptIdentity, CapabilityId, CapabilityIdentity, CheckpointId,
    CheckpointIdentity, DomainGeneration, DomainId, DomainIdentity, GovernorIdentity,
    GovernorLogIndex, GovernorTerm, LeaseId, LeaseIdentity, NodeGeneration, NodeId, NodeIdentity,
    ObjectId, ObjectIdentity, ObjectVersion, ProcessGeneration, ProcessId, ProcessIdentity,
    ReceiptId, ReceiptIdentity, ResourceGeneration, ResourceId, ResourceIdentity, ResourceOwner,
    TaskId, TaskIdentity, WorldEpoch, WorldId, WorldIdentity, WorldIdentityError,
};

pub const IDENTITY_WIRE_MAGIC: &[u8; 8] = b"OWIDENT\0";
pub const IDENTITY_WIRE_VERSION: u16 = 1;
pub const IDENTITY_WIRE_HEADER_BYTES: usize = 16;
pub const MAX_IDENTITY_WIRE_RECORD_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum IdentityWireKind {
    World = 1,
    Governor = 2,
    Node = 3,
    Domain = 4,
    Process = 5,
    Resource = 6,
    Object = 7,
    Capability = 8,
    Lease = 9,
    Task = 10,
    Attempt = 11,
    Checkpoint = 12,
    Receipt = 13,
}

impl IdentityWireKind {
    fn from_u16(value: u16) -> Result<Self, IdentityWireError> {
        match value {
            1 => Ok(Self::World),
            2 => Ok(Self::Governor),
            3 => Ok(Self::Node),
            4 => Ok(Self::Domain),
            5 => Ok(Self::Process),
            6 => Ok(Self::Resource),
            7 => Ok(Self::Object),
            8 => Ok(Self::Capability),
            9 => Ok(Self::Lease),
            10 => Ok(Self::Task),
            11 => Ok(Self::Attempt),
            12 => Ok(Self::Checkpoint),
            13 => Ok(Self::Receipt),
            found => Err(IdentityWireError::UnknownKind { found }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityWireRecord {
    World(WorldIdentity),
    Governor(GovernorIdentity),
    Node(NodeIdentity),
    Domain(DomainIdentity),
    Process(ProcessIdentity),
    Resource(ResourceIdentity),
    Object(ObjectIdentity),
    Capability(CapabilityIdentity),
    Lease(LeaseIdentity),
    Task(TaskIdentity),
    Attempt(AttemptIdentity),
    Checkpoint(CheckpointIdentity),
    Receipt(ReceiptIdentity),
}

impl IdentityWireRecord {
    pub fn kind(&self) -> IdentityWireKind {
        match self {
            Self::World(_) => IdentityWireKind::World,
            Self::Governor(_) => IdentityWireKind::Governor,
            Self::Node(_) => IdentityWireKind::Node,
            Self::Domain(_) => IdentityWireKind::Domain,
            Self::Process(_) => IdentityWireKind::Process,
            Self::Resource(_) => IdentityWireKind::Resource,
            Self::Object(_) => IdentityWireKind::Object,
            Self::Capability(_) => IdentityWireKind::Capability,
            Self::Lease(_) => IdentityWireKind::Lease,
            Self::Task(_) => IdentityWireKind::Task,
            Self::Attempt(_) => IdentityWireKind::Attempt,
            Self::Checkpoint(_) => IdentityWireKind::Checkpoint,
            Self::Receipt(_) => IdentityWireKind::Receipt,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, IdentityWireError> {
        let mut payload = Vec::new();
        match self {
            Self::World(identity) => encode_world(&mut payload, identity)?,
            Self::Governor(identity) => {
                encode_world(&mut payload, identity.world())?;
                put_u64(&mut payload, identity.term().get());
                put_u64(&mut payload, identity.log_index().get());
            }
            Self::Node(identity) => encode_node(&mut payload, identity)?,
            Self::Domain(identity) => encode_domain(&mut payload, identity)?,
            Self::Process(identity) => encode_process(&mut payload, identity)?,
            Self::Resource(identity) => encode_resource(&mut payload, identity)?,
            Self::Object(identity) => {
                put_text(&mut payload, identity.world().as_str())?;
                put_text(&mut payload, identity.object().as_str())?;
                put_u64(&mut payload, identity.version().get());
            }
            Self::Capability(identity) => {
                put_text(&mut payload, identity.world().as_str())?;
                put_text(&mut payload, identity.capability().as_str())?;
            }
            Self::Lease(identity) => {
                put_text(&mut payload, identity.world().as_str())?;
                put_text(&mut payload, identity.lease().as_str())?;
            }
            Self::Task(identity) => {
                put_text(&mut payload, identity.world().as_str())?;
                put_text(&mut payload, identity.task().as_str())?;
            }
            Self::Attempt(identity) => encode_attempt(&mut payload, identity)?,
            Self::Checkpoint(identity) => {
                encode_attempt(&mut payload, identity.attempt())?;
                put_text(&mut payload, identity.checkpoint().as_str())?;
            }
            Self::Receipt(identity) => {
                put_text(&mut payload, identity.world().as_str())?;
                put_text(&mut payload, identity.receipt().as_str())?;
            }
        }

        let total_len = IDENTITY_WIRE_HEADER_BYTES
            .checked_add(payload.len())
            .ok_or(IdentityWireError::RecordTooLarge {
                actual: usize::MAX,
                max: MAX_IDENTITY_WIRE_RECORD_BYTES,
            })?;
        if total_len > MAX_IDENTITY_WIRE_RECORD_BYTES {
            return Err(IdentityWireError::RecordTooLarge {
                actual: total_len,
                max: MAX_IDENTITY_WIRE_RECORD_BYTES,
            });
        }
        let total_len_u32 =
            u32::try_from(total_len).map_err(|_| IdentityWireError::RecordTooLarge {
                actual: total_len,
                max: MAX_IDENTITY_WIRE_RECORD_BYTES,
            })?;

        let mut record = Vec::with_capacity(total_len);
        record.extend_from_slice(IDENTITY_WIRE_MAGIC);
        put_u16(&mut record, IDENTITY_WIRE_VERSION);
        put_u16(&mut record, self.kind() as u16);
        record.extend_from_slice(&total_len_u32.to_be_bytes());
        record.extend_from_slice(&payload);
        Ok(record)
    }

    pub fn decode(record: &[u8]) -> Result<Self, IdentityWireError> {
        if record.len() > MAX_IDENTITY_WIRE_RECORD_BYTES {
            return Err(IdentityWireError::RecordTooLarge {
                actual: record.len(),
                max: MAX_IDENTITY_WIRE_RECORD_BYTES,
            });
        }
        if record.len() < IDENTITY_WIRE_HEADER_BYTES {
            return Err(IdentityWireError::Truncated {
                needed: IDENTITY_WIRE_HEADER_BYTES,
                remaining: record.len(),
            });
        }
        if &record[..IDENTITY_WIRE_MAGIC.len()] != IDENTITY_WIRE_MAGIC {
            return Err(IdentityWireError::BadMagic);
        }

        let version = u16::from_be_bytes([record[8], record[9]]);
        if version != IDENTITY_WIRE_VERSION {
            return Err(IdentityWireError::UnsupportedVersion { found: version });
        }
        let kind = IdentityWireKind::from_u16(u16::from_be_bytes([record[10], record[11]]))?;
        let declared =
            u32::from_be_bytes([record[12], record[13], record[14], record[15]]) as usize;
        if declared > MAX_IDENTITY_WIRE_RECORD_BYTES {
            return Err(IdentityWireError::RecordTooLarge {
                actual: declared,
                max: MAX_IDENTITY_WIRE_RECORD_BYTES,
            });
        }
        if declared != record.len() {
            return Err(IdentityWireError::LengthMismatch {
                declared,
                actual: record.len(),
            });
        }

        let mut cursor = Cursor::new(&record[IDENTITY_WIRE_HEADER_BYTES..]);
        let decoded = match kind {
            IdentityWireKind::World => Self::World(decode_world(&mut cursor)?),
            IdentityWireKind::Governor => Self::Governor(GovernorIdentity::new(
                decode_world(&mut cursor)?,
                read_counter(&mut cursor, GovernorTerm::new)?,
                read_counter(&mut cursor, GovernorLogIndex::new)?,
            )),
            IdentityWireKind::Node => Self::Node(decode_node(&mut cursor)?),
            IdentityWireKind::Domain => Self::Domain(decode_domain(&mut cursor)?),
            IdentityWireKind::Process => Self::Process(decode_process(&mut cursor)?),
            IdentityWireKind::Resource => Self::Resource(decode_resource(&mut cursor)?),
            IdentityWireKind::Object => Self::Object(ObjectIdentity::new(
                read_simple(&mut cursor, WorldId::new)?,
                read_simple(&mut cursor, ObjectId::new)?,
                read_counter(&mut cursor, ObjectVersion::new)?,
            )),
            IdentityWireKind::Capability => Self::Capability(CapabilityIdentity::new(
                read_simple(&mut cursor, WorldId::new)?,
                read_simple(&mut cursor, CapabilityId::new)?,
            )),
            IdentityWireKind::Lease => Self::Lease(LeaseIdentity::new(
                read_simple(&mut cursor, WorldId::new)?,
                read_simple(&mut cursor, LeaseId::new)?,
            )),
            IdentityWireKind::Task => Self::Task(TaskIdentity::new(
                read_simple(&mut cursor, WorldId::new)?,
                read_simple(&mut cursor, TaskId::new)?,
            )),
            IdentityWireKind::Attempt => Self::Attempt(decode_attempt(&mut cursor)?),
            IdentityWireKind::Checkpoint => Self::Checkpoint(CheckpointIdentity::new(
                decode_attempt(&mut cursor)?,
                read_simple(&mut cursor, CheckpointId::new)?,
            )),
            IdentityWireKind::Receipt => Self::Receipt(ReceiptIdentity::new(
                read_simple(&mut cursor, WorldId::new)?,
                read_simple(&mut cursor, ReceiptId::new)?,
            )),
        };

        if cursor.remaining() != 0 {
            return Err(IdentityWireError::TrailingBytes {
                remaining: cursor.remaining(),
            });
        }
        Ok(decoded)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum IdentityWireError {
    #[error("identity record is {actual} bytes; maximum is {max}")]
    RecordTooLarge { actual: usize, max: usize },
    #[error("identity record is truncated: need {needed} bytes, have {remaining}")]
    Truncated { needed: usize, remaining: usize },
    #[error("identity record has invalid magic")]
    BadMagic,
    #[error("unsupported identity wire version {found}")]
    UnsupportedVersion { found: u16 },
    #[error("unknown identity wire kind {found}")]
    UnknownKind { found: u16 },
    #[error("identity record length says {declared} bytes but input has {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("identity text is not valid UTF-8")]
    InvalidUtf8,
    #[error("identity text exceeds the u16 wire length")]
    TextTooLong,
    #[error("unknown resource-owner tag {found}")]
    UnknownResourceOwner { found: u16 },
    #[error("identity payload has {remaining} trailing bytes")]
    TrailingBytes { remaining: usize },
    #[error(transparent)]
    InvalidIdentity(#[from] WorldIdentityError),
}

fn put_u16(target: &mut Vec<u8>, value: u16) {
    target.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(target: &mut Vec<u8>, value: u64) {
    target.extend_from_slice(&value.to_be_bytes());
}

fn put_text(target: &mut Vec<u8>, value: &str) -> Result<(), IdentityWireError> {
    let len = u16::try_from(value.len()).map_err(|_| IdentityWireError::TextTooLong)?;
    put_u16(target, len);
    target.extend_from_slice(value.as_bytes());
    Ok(())
}

fn encode_world(target: &mut Vec<u8>, identity: &WorldIdentity) -> Result<(), IdentityWireError> {
    put_text(target, identity.world().as_str())?;
    put_u64(target, identity.epoch().get());
    Ok(())
}

fn encode_node(target: &mut Vec<u8>, identity: &NodeIdentity) -> Result<(), IdentityWireError> {
    put_text(target, identity.world().as_str())?;
    put_text(target, identity.node().as_str())?;
    put_u64(target, identity.generation().get());
    Ok(())
}

fn encode_domain(target: &mut Vec<u8>, identity: &DomainIdentity) -> Result<(), IdentityWireError> {
    encode_node(target, identity.node())?;
    put_text(target, identity.domain().as_str())?;
    put_u64(target, identity.generation().get());
    Ok(())
}

fn encode_process(
    target: &mut Vec<u8>,
    identity: &ProcessIdentity,
) -> Result<(), IdentityWireError> {
    encode_domain(target, identity.domain())?;
    put_text(target, identity.process().as_str())?;
    put_u64(target, identity.generation().get());
    Ok(())
}

fn encode_attempt(
    target: &mut Vec<u8>,
    identity: &AttemptIdentity,
) -> Result<(), IdentityWireError> {
    put_text(target, identity.world().as_str())?;
    put_text(target, identity.task().as_str())?;
    put_u64(target, identity.attempt().get());
    Ok(())
}

fn encode_resource(
    target: &mut Vec<u8>,
    identity: &ResourceIdentity,
) -> Result<(), IdentityWireError> {
    match identity.owner() {
        ResourceOwner::World { world } => {
            put_u16(target, 1);
            encode_world(target, world)?;
        }
        ResourceOwner::Node { node } => {
            put_u16(target, 2);
            encode_node(target, node)?;
        }
        ResourceOwner::Domain { domain } => {
            put_u16(target, 3);
            encode_domain(target, domain)?;
        }
        ResourceOwner::Process { process } => {
            put_u16(target, 4);
            encode_process(target, process)?;
        }
    }
    put_text(target, identity.resource().as_str())?;
    put_u64(target, identity.generation().get());
    Ok(())
}

fn decode_world(cursor: &mut Cursor<'_>) -> Result<WorldIdentity, IdentityWireError> {
    Ok(WorldIdentity::new(
        read_simple(cursor, WorldId::new)?,
        read_counter(cursor, WorldEpoch::new)?,
    ))
}

fn decode_node(cursor: &mut Cursor<'_>) -> Result<NodeIdentity, IdentityWireError> {
    Ok(NodeIdentity::new(
        read_simple(cursor, WorldId::new)?,
        read_simple(cursor, NodeId::new)?,
        read_counter(cursor, NodeGeneration::new)?,
    ))
}

fn decode_domain(cursor: &mut Cursor<'_>) -> Result<DomainIdentity, IdentityWireError> {
    Ok(DomainIdentity::new(
        decode_node(cursor)?,
        read_simple(cursor, DomainId::new)?,
        read_counter(cursor, DomainGeneration::new)?,
    ))
}

fn decode_process(cursor: &mut Cursor<'_>) -> Result<ProcessIdentity, IdentityWireError> {
    Ok(ProcessIdentity::new(
        decode_domain(cursor)?,
        read_simple(cursor, ProcessId::new)?,
        read_counter(cursor, ProcessGeneration::new)?,
    ))
}

fn decode_attempt(cursor: &mut Cursor<'_>) -> Result<AttemptIdentity, IdentityWireError> {
    Ok(AttemptIdentity::new(
        read_simple(cursor, WorldId::new)?,
        read_simple(cursor, TaskId::new)?,
        read_counter(cursor, AttemptGeneration::new)?,
    ))
}

fn decode_resource(cursor: &mut Cursor<'_>) -> Result<ResourceIdentity, IdentityWireError> {
    let owner = match cursor.take_u16()? {
        1 => ResourceOwner::World {
            world: decode_world(cursor)?,
        },
        2 => ResourceOwner::Node {
            node: decode_node(cursor)?,
        },
        3 => ResourceOwner::Domain {
            domain: decode_domain(cursor)?,
        },
        4 => ResourceOwner::Process {
            process: decode_process(cursor)?,
        },
        found => return Err(IdentityWireError::UnknownResourceOwner { found }),
    };
    Ok(ResourceIdentity::new(
        owner,
        read_resource(cursor)?,
        read_counter(cursor, ResourceGeneration::new)?,
    ))
}

fn read_simple<T>(
    cursor: &mut Cursor<'_>,
    constructor: impl FnOnce(String) -> Result<T, WorldIdentityError>,
) -> Result<T, IdentityWireError> {
    constructor(cursor.take_text()?.to_owned()).map_err(IdentityWireError::from)
}

fn read_resource(cursor: &mut Cursor<'_>) -> Result<ResourceId, IdentityWireError> {
    ResourceId::new(cursor.take_text()?.to_owned()).map_err(IdentityWireError::from)
}

fn read_counter<T>(
    cursor: &mut Cursor<'_>,
    constructor: impl FnOnce(u64) -> Result<T, WorldIdentityError>,
) -> Result<T, IdentityWireError> {
    constructor(cursor.take_u64()?).map_err(IdentityWireError::from)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], IdentityWireError> {
        if self.remaining() < len {
            return Err(IdentityWireError::Truncated {
                needed: len,
                remaining: self.remaining(),
            });
        }
        let end = self.offset + len;
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn take_u16(&mut self) -> Result<u16, IdentityWireError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn take_u64(&mut self) -> Result<u64, IdentityWireError> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn take_text(&mut self) -> Result<&'a str, IdentityWireError> {
        let len = self.take_u16()? as usize;
        std::str::from_utf8(self.take(len)?).map_err(|_| IdentityWireError::InvalidUtf8)
    }
}

/// Fixed cross-implementation corpus for the version-1 identity ABI.
///
/// The record order and literal values are part of the conformance oracle.
pub fn identity_v1_conformance_records() -> Vec<IdentityWireRecord> {
    const WIDE_COUNTER: u64 = 0x0102_0304_0506_0708;

    let world_id = WorldId::new("world-a").expect("conformance WorldId is valid");
    let node_one = NodeIdentity::new(
        world_id.clone(),
        NodeId::new("node-a").expect("conformance NodeId is valid"),
        NodeGeneration::new(1).expect("conformance generation is nonzero"),
    );
    let domain_two = DomainIdentity::new(
        node_one.clone(),
        DomainId::new("domain-a").expect("conformance DomainId is valid"),
        DomainGeneration::new(2).expect("conformance generation is nonzero"),
    );
    let process_three = ProcessIdentity::new(
        domain_two.clone(),
        ProcessId::new("process-a").expect("conformance ProcessId is valid"),
        ProcessGeneration::new(3).expect("conformance generation is nonzero"),
    );
    let attempt = |generation| {
        AttemptIdentity::new(
            world_id.clone(),
            TaskId::new("task-a").expect("conformance TaskId is valid"),
            AttemptGeneration::new(generation).expect("conformance generation is nonzero"),
        )
    };

    vec![
        IdentityWireRecord::World(WorldIdentity::new(
            world_id.clone(),
            WorldEpoch::new(1).expect("conformance epoch is nonzero"),
        )),
        IdentityWireRecord::Governor(GovernorIdentity::new(
            WorldIdentity::new(
                world_id.clone(),
                WorldEpoch::new(WIDE_COUNTER).expect("conformance epoch is nonzero"),
            ),
            GovernorTerm::new(2).expect("conformance term is nonzero"),
            GovernorLogIndex::new(3).expect("conformance log index is nonzero"),
        )),
        IdentityWireRecord::Node(NodeIdentity::new(
            world_id.clone(),
            NodeId::new("node-a").expect("conformance NodeId is valid"),
            NodeGeneration::new(WIDE_COUNTER).expect("conformance generation is nonzero"),
        )),
        IdentityWireRecord::Domain(domain_two.clone()),
        IdentityWireRecord::Process(process_three.clone()),
        IdentityWireRecord::Resource(ResourceIdentity::new(
            ResourceOwner::World {
                world: WorldIdentity::new(
                    world_id.clone(),
                    WorldEpoch::new(1).expect("conformance epoch is nonzero"),
                ),
            },
            ResourceId::new("world/state").expect("conformance ResourceId is valid"),
            ResourceGeneration::new(11).expect("conformance generation is nonzero"),
        )),
        IdentityWireRecord::Resource(ResourceIdentity::new(
            ResourceOwner::Node {
                node: node_one.clone(),
            },
            ResourceId::new("cpu/slot-0").expect("conformance ResourceId is valid"),
            ResourceGeneration::new(12).expect("conformance generation is nonzero"),
        )),
        IdentityWireRecord::Resource(ResourceIdentity::new(
            ResourceOwner::Domain {
                domain: domain_two.clone(),
            },
            ResourceId::new("service/console").expect("conformance ResourceId is valid"),
            ResourceGeneration::new(13).expect("conformance generation is nonzero"),
        )),
        IdentityWireRecord::Resource(ResourceIdentity::new(
            ResourceOwner::Process {
                process: process_three,
            },
            ResourceId::new("fd/stdout").expect("conformance ResourceId is valid"),
            ResourceGeneration::new(14).expect("conformance generation is nonzero"),
        )),
        IdentityWireRecord::Object(ObjectIdentity::new(
            world_id.clone(),
            ObjectId::new("object-a").expect("conformance ObjectId is valid"),
            ObjectVersion::new(u64::MAX).expect("conformance version is nonzero"),
        )),
        IdentityWireRecord::Capability(CapabilityIdentity::new(
            world_id.clone(),
            CapabilityId::new("cap-a").expect("conformance CapabilityId is valid"),
        )),
        IdentityWireRecord::Lease(LeaseIdentity::new(
            world_id.clone(),
            LeaseId::new("lease-a").expect("conformance LeaseId is valid"),
        )),
        IdentityWireRecord::Task(TaskIdentity::new(
            world_id.clone(),
            TaskId::new("task-a").expect("conformance TaskId is valid"),
        )),
        IdentityWireRecord::Attempt(attempt(WIDE_COUNTER)),
        IdentityWireRecord::Checkpoint(CheckpointIdentity::new(
            attempt(2),
            CheckpointId::new("checkpoint-a").expect("conformance CheckpointId is valid"),
        )),
        IdentityWireRecord::Receipt(ReceiptIdentity::new(
            world_id,
            ReceiptId::new("receipt-a").expect("conformance ReceiptId is valid"),
        )),
    ]
}

/// Concatenate the fixed conformance records without adding a stream envelope.
pub fn identity_v1_conformance_bytes() -> Result<Vec<u8>, IdentityWireError> {
    let mut bytes = Vec::new();
    for record in identity_v1_conformance_records() {
        bytes.extend_from_slice(&record.encode()?);
    }
    Ok(bytes)
}
