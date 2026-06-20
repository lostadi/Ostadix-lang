//! Hosted bridge between `OValue::Capability` and a live O-core kernel session.
//!
//! The serialized capability identity is never interpreted as a slot, pointer,
//! or kernel handle. A process-local broker resolves an unpredictable bearer
//! token through its private binding table and sends only the bound u64 handle
//! over an authenticated transport for the corresponding kernel session.
//!
//! Threat boundary:
//! - Prevented: guessed identities, serialized forgeries, metadata-based rights
//!   escalation, stale or revoked tokens, and cross-session token replay.
//! - Not prevented: theft of a still-live bearer inside the same broker
//!   session, compromise of the broker process, or compromise of the
//!   authenticated kernel transport. Bearer possession is delegation.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::capability::fresh_bearer_identity;
use crate::value::{CapabilityKind, OValue};

pub const RIGHT_DEBUG_WRITE: u64 = 1 << 0;
pub const RIGHT_PAGE_ALLOC: u64 = 1 << 1;
pub const RIGHT_TRANSFER: u64 = 1 << 2;

pub const SYS_DEBUG_WRITE: u64 = 0;
pub const SYS_CAP_CLOSE: u64 = 1;
pub const SYS_CAP_COPY: u64 = 2;
pub const SYS_PAGE_ALLOC: u64 = 3;
pub const SYS_YIELD: u64 = 4;

/// Transport for one authenticated, live O-core kernel session.
///
/// Implementations may use a VM socket, shared memory, a monitor channel, or a
/// native syscall instruction. The broker never derives authority from wire
/// metadata; only a token already bound in this session reaches this method.
pub trait KernelSyscallTransport {
    fn invoke(&mut self, number: u64, capability: u64, args: [u64; 5]) -> Result<u64>;
}

#[derive(Debug, Clone)]
struct Binding {
    handle: u64,
    kind: CapabilityKind,
    rights: u64,
}

pub struct CapabilityBroker<T> {
    transport: T,
    bindings: HashMap<String, Binding>,
}

impl<T: KernelSyscallTransport> CapabilityBroker<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            bindings: HashMap::new(),
        }
    }

    /// Bind a kernel-issued `(generation << 32) | slot` handle into this live
    /// session and return its hosted `OCapability` bearer value.
    pub fn bind(
        &mut self,
        kind: CapabilityKind,
        handle: u64,
        rights: u64,
        mut metadata: HashMap<String, OValue>,
    ) -> Result<OValue> {
        let identity = loop {
            let identity = fresh_bearer_identity("ocore-live")?;
            if !self.bindings.contains_key(&identity) {
                break identity;
            }
        };
        metadata.insert("live".into(), OValue::bool_(true));
        self.bindings.insert(
            identity.clone(),
            Binding {
                handle,
                kind,
                rights,
            },
        );
        Ok(OValue::capability(kind, identity, metadata))
    }

    pub fn invoke(
        &mut self,
        capability: &OValue,
        kind: CapabilityKind,
        required_rights: u64,
        syscall: u64,
        args: [u64; 5],
    ) -> Result<u64> {
        let OValue::Capability {
            kind: supplied_kind,
            identity,
            ..
        } = capability
        else {
            bail!("expected OCapability, got {}", capability.type_name());
        };
        if *supplied_kind != kind {
            bail!("capability kind mismatch");
        }
        let binding = self.bindings.get(identity).ok_or_else(|| {
            anyhow::anyhow!("capability is forged, revoked, or belongs to another session")
        })?;
        if binding.kind != kind {
            bail!("broker binding kind mismatch");
        }
        if binding.rights & required_rights != required_rights {
            bail!("capability lacks required rights 0x{required_rights:x}");
        }
        self.transport.invoke(syscall, binding.handle, args)
    }

    pub fn revoke(&mut self, capability: &OValue) -> Result<()> {
        let OValue::Capability { identity, .. } = capability else {
            bail!("expected OCapability, got {}", capability.type_name());
        };
        self.bindings
            .remove(identity)
            .ok_or_else(|| anyhow::anyhow!("capability is not bound in this session"))?;
        Ok(())
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingTransport {
        calls: Vec<(u64, u64, [u64; 5])>,
    }

    impl KernelSyscallTransport for RecordingTransport {
        fn invoke(&mut self, number: u64, capability: u64, args: [u64; 5]) -> Result<u64> {
            self.calls.push((number, capability, args));
            Ok(7)
        }
    }

    #[test]
    fn bound_ocapability_resolves_to_kernel_handle() {
        let mut broker = CapabilityBroker::new(RecordingTransport::default());
        let capability = broker
            .bind(
                CapabilityKind::Service,
                (3u64 << 32) | 9,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .unwrap();
        let result = broker
            .invoke(
                &capability,
                CapabilityKind::Service,
                RIGHT_DEBUG_WRITE,
                SYS_DEBUG_WRITE,
                [0x1000, 7, 0, 0, 0],
            )
            .unwrap();
        assert_eq!(result, 7);
        assert_eq!(broker.transport().calls[0].1, (3u64 << 32) | 9);
    }

    #[test]
    fn forged_or_revoked_identity_never_becomes_a_handle() {
        let mut broker = CapabilityBroker::new(RecordingTransport::default());
        let capability = OValue::capability(
            CapabilityKind::Service,
            "ocore-live:0000000000000009",
            HashMap::new(),
        );
        assert!(broker
            .invoke(
                &capability,
                CapabilityKind::Service,
                RIGHT_DEBUG_WRITE,
                SYS_DEBUG_WRITE,
                [0; 5],
            )
            .is_err());
    }

    #[test]
    fn rights_are_checked_before_transport() {
        let mut broker = CapabilityBroker::new(RecordingTransport::default());
        let capability = broker
            .bind(
                CapabilityKind::MemoryRegion,
                (1u64 << 32) | 2,
                RIGHT_PAGE_ALLOC,
                HashMap::new(),
            )
            .unwrap();
        assert!(broker
            .invoke(
                &capability,
                CapabilityKind::MemoryRegion,
                RIGHT_TRANSFER,
                SYS_CAP_COPY,
                [0; 5],
            )
            .is_err());
        assert!(broker.transport().calls.is_empty());
    }

    #[test]
    fn live_identity_has_256_bits_and_is_session_local() {
        let mut first = CapabilityBroker::new(RecordingTransport::default());
        let capability = first
            .bind(
                CapabilityKind::Service,
                (2u64 << 32) | 4,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .unwrap();
        let OValue::Capability { identity, .. } = &capability else {
            panic!("bind must return a capability")
        };
        assert!(identity.starts_with("ocore-live:"));
        assert_eq!(identity.len(), "ocore-live:".len() + 64);

        let mut second = CapabilityBroker::new(RecordingTransport::default());
        let err = second
            .invoke(
                &capability,
                CapabilityKind::Service,
                RIGHT_DEBUG_WRITE,
                SYS_DEBUG_WRITE,
                [0; 5],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("another session"));
        assert!(second.transport().calls.is_empty());
    }
}
