use num_bigint::BigInt;

use super::graph::ActorId;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DomainFlags: u16 {
        const INTEGER  = 0x0001;
        const FLOAT    = 0x0002;
        const NUMERIC  = Self::INTEGER.bits() | Self::FLOAT.bits();
        const POINTER  = 0x0004;
        const BOOL     = 0x0008;
        const BITFIELD = 0x0010;
        const STRING   = 0x0020;
        const STRUCT   = 0x0040;
        const ANY      = 0x00ff;
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct RepFlags: u16 {
        const I8   = 0x0001;
        const I16  = 0x0002;
        const I32  = 0x0004;
        const I64  = 0x0008;
        const I128 = 0x0010;
        const BIG  = 0x0020;
        const F32  = 0x0040;
        const F64  = 0x0080;
        const PTR  = 0x0100;
        const BOOL = 0x0200;
        const STR  = 0x0400;
        const ANY  = 0x07ff;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpKind {
    // Type-bearing relations.
    Additive,
    Multiplicative,
    Bitwise,
    Ordered,
    Bounded { value: BigInt },
    AbiFixed { dom: DomainFlags, rep: RepFlags },
    Dereferenceable,
    FieldAccess { field: String },

    // Scheduling-bearing relations.
    DataFlow,
    StructuralBarrier,
    Sequence,
    ActorSerial { actor: ActorId },

    // First-class group topology.
    Batch,
    All,
    Any,
    Race,

    // Backend value crossing.
    BackendCrossing { from_lang: String, to_lang: String },

    // Native/lifted frontends.
    X86 { mnemonic: String },
    OcoreOp { kind: OcoreOpKind },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OcoreOpKind {
    Add,
    Sub,
    Mul,
    Div,
    Load,
    Store,
    Inb,
    Outb,
    VolatileLoad,
    VolatileStore,
    AtomicFetch { order: MemOrder },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemOrder {
    Relaxed,
    Acquire,
    Release,
    AcqRel,
    SeqCst,
}
