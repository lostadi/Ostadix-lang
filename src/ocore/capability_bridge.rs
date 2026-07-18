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

const SYS_DEBUG_WRITE: u64 = 0;
const SYS_CAP_CLOSE: u64 = 1;
const CAP_CLOSE_SUCCEEDED: u64 = 1;

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
        if handle == 0 {
            bail!("cannot bind a null kernel capability handle");
        }
        if handle >> 32 == 0 {
            bail!("cannot bind a kernel capability with generation zero");
        }
        if rights == 0 {
            bail!("cannot bind a kernel capability with no rights");
        }
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

    /// Invoke the native debug-write operation with broker-owned policy.
    ///
    /// Callers supply only operation data. The broker fixes the capability
    /// kind, required right, syscall number, and transport argument layout.
    pub fn debug_write(&mut self, capability: &OValue, pointer: u64, length: u64) -> Result<u64> {
        let handle = {
            let binding = self.resolve_binding(capability)?;
            if binding.kind != CapabilityKind::Service {
                bail!("debug-write requires a service capability");
            }
            if binding.rights & RIGHT_DEBUG_WRITE != RIGHT_DEBUG_WRITE {
                bail!("capability lacks required rights 0x{RIGHT_DEBUG_WRITE:x}");
            }
            binding.handle
        };
        self.transport
            .invoke(SYS_DEBUG_WRITE, handle, [pointer, length, 0, 0, 0])
    }

    /// Close the kernel capability and forget its hosted bearer on success.
    ///
    /// A transport failure or kernel rejection leaves the binding intact so
    /// the caller can retry or explicitly forget only the hosted token.
    pub fn cap_close(&mut self, capability: &OValue) -> Result<u64> {
        let (identity, handle) = {
            let (identity, binding) = self.resolve_binding_with_identity(capability)?;
            (identity.to_owned(), binding.handle)
        };
        let result = self.transport.invoke(SYS_CAP_CLOSE, handle, [0; 5])?;
        if result != CAP_CLOSE_SUCCEEDED {
            bail!("kernel rejected capability close with status 0x{result:016x}");
        }
        self.bindings.remove(&identity);
        Ok(result)
    }

    /// Forget only the hosted bearer without changing kernel authority.
    pub fn forget(&mut self, capability: &OValue) -> Result<()> {
        let identity = self.resolve_binding_with_identity(capability)?.0.to_owned();
        self.bindings.remove(&identity);
        Ok(())
    }

    fn resolve_binding<'a>(&'a self, capability: &OValue) -> Result<&'a Binding> {
        Ok(self.resolve_binding_with_identity(capability)?.1)
    }

    fn resolve_binding_with_identity<'a, 'b>(
        &'a self,
        capability: &'b OValue,
    ) -> Result<(&'b str, &'a Binding)> {
        let OValue::Capability {
            kind: supplied_kind,
            identity,
            ..
        } = capability
        else {
            bail!("expected OCapability, got {}", capability.type_name());
        };
        let binding = self.bindings.get(identity).ok_or_else(|| {
            anyhow::anyhow!("capability is forged, revoked, or belongs to another session")
        })?;
        if binding.kind != *supplied_kind {
            bail!("broker binding kind mismatch");
        }
        Ok((identity, binding))
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingTransport {
        calls: Vec<(u64, u64, [u64; 5])>,
        result: u64,
    }

    impl Default for RecordingTransport {
        fn default() -> Self {
            Self {
                calls: Vec::new(),
                result: 7,
            }
        }
    }

    impl RecordingTransport {
        fn returning(result: u64) -> Self {
            Self {
                calls: Vec::new(),
                result,
            }
        }
    }

    impl KernelSyscallTransport for RecordingTransport {
        fn invoke(&mut self, number: u64, capability: u64, args: [u64; 5]) -> Result<u64> {
            self.calls.push((number, capability, args));
            Ok(self.result)
        }
    }

    #[derive(Default)]
    struct FailingTransport {
        calls: usize,
    }

    impl KernelSyscallTransport for FailingTransport {
        fn invoke(&mut self, _number: u64, _capability: u64, _args: [u64; 5]) -> Result<u64> {
            self.calls += 1;
            bail!("transport unavailable")
        }
    }

    #[test]
    fn debug_write_fixes_policy_and_resolves_to_kernel_handle() {
        let mut broker = CapabilityBroker::new(RecordingTransport::default());
        let capability = broker
            .bind(
                CapabilityKind::Service,
                (3u64 << 32) | 9,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .unwrap();
        let result = broker.debug_write(&capability, 0x1000, 7).unwrap();
        assert_eq!(result, 7);
        assert_eq!(
            broker.transport().calls,
            vec![(SYS_DEBUG_WRITE, (3u64 << 32) | 9, [0x1000, 7, 0, 0, 0])]
        );
    }

    #[test]
    fn forged_or_forgotten_identity_never_becomes_a_handle() {
        let mut broker = CapabilityBroker::new(RecordingTransport::default());
        let forged = OValue::capability(
            CapabilityKind::Service,
            "ocore-live:0000000000000009",
            HashMap::new(),
        );
        assert!(broker.debug_write(&forged, 0, 0).is_err());

        let capability = broker
            .bind(
                CapabilityKind::Service,
                (1u64 << 32) | 1,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .unwrap();
        broker.forget(&capability).unwrap();
        assert!(broker.debug_write(&capability, 0, 0).is_err());
        assert!(broker.transport().calls.is_empty());
    }

    #[test]
    fn rights_are_checked_before_transport() {
        let mut broker = CapabilityBroker::new(RecordingTransport::default());
        let capability = broker
            .bind(
                CapabilityKind::Service,
                (1u64 << 32) | 2,
                1 << 2,
                HashMap::new(),
            )
            .unwrap();
        assert!(broker.debug_write(&capability, 0, 0).is_err());
        assert!(broker.transport().calls.is_empty());
    }

    #[test]
    fn wrong_kind_is_rejected_before_transport() {
        let mut broker = CapabilityBroker::new(RecordingTransport::default());
        let capability = broker
            .bind(
                CapabilityKind::MemoryRegion,
                (1u64 << 32) | 2,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .unwrap();
        assert!(broker.debug_write(&capability, 0, 0).is_err());
        assert!(broker.transport().calls.is_empty());
    }

    #[test]
    fn bind_rejects_null_generation_zero_and_rights_free_handles() {
        let mut broker = CapabilityBroker::new(RecordingTransport::default());
        assert!(broker
            .bind(
                CapabilityKind::Service,
                0,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .is_err());
        assert!(broker
            .bind(
                CapabilityKind::Service,
                9,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .is_err());
        assert!(broker
            .bind(CapabilityKind::Service, (1u64 << 32) | 9, 0, HashMap::new(),)
            .is_err());
    }

    #[test]
    fn successful_close_removes_binding_after_kernel_accepts_it() {
        let mut broker = CapabilityBroker::new(RecordingTransport::returning(1));
        let capability = broker
            .bind(
                CapabilityKind::Service,
                (4u64 << 32) | 3,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .unwrap();
        assert_eq!(broker.cap_close(&capability).unwrap(), 1);
        assert_eq!(
            broker.transport().calls,
            vec![(SYS_CAP_CLOSE, (4u64 << 32) | 3, [0; 5])]
        );
        assert!(broker.debug_write(&capability, 0, 0).is_err());
        assert_eq!(broker.transport().calls.len(), 1);
    }

    #[test]
    fn rejected_close_keeps_binding_for_retry_or_forget() {
        let mut broker = CapabilityBroker::new(RecordingTransport::returning(u64::MAX));
        let capability = broker
            .bind(
                CapabilityKind::Service,
                (5u64 << 32) | 4,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .unwrap();
        let error = broker.cap_close(&capability).unwrap_err().to_string();
        assert!(error.contains("kernel rejected capability close"));
        broker.forget(&capability).unwrap();
        assert_eq!(broker.transport().calls.len(), 1);
    }

    #[test]
    fn transport_failure_keeps_close_binding_for_retry_or_forget() {
        let mut broker = CapabilityBroker::new(FailingTransport::default());
        let capability = broker
            .bind(
                CapabilityKind::Service,
                (6u64 << 32) | 5,
                RIGHT_DEBUG_WRITE,
                HashMap::new(),
            )
            .unwrap();
        let error = broker.cap_close(&capability).unwrap_err().to_string();
        assert!(error.contains("transport unavailable"));
        broker.forget(&capability).unwrap();
        assert_eq!(broker.transport().calls, 1);
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
            .debug_write(&capability, 0, 0)
            .unwrap_err()
            .to_string();
        assert!(err.contains("another session"));
        assert!(second.transport().calls.is_empty());
    }

    #[test]
    fn hosted_bridge_constants_match_the_freestanding_native_abi() {
        let native_abi = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/ocore/runtime/x86_64/native_abi.oc"
        ));
        let capability_abi = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/ocore/runtime/x86_64/capability.oc"
        ));
        assert!(native_abi.contains(&format!(
            "pub const SYS_DEBUG_WRITE: u64 = {SYS_DEBUG_WRITE};"
        )));
        assert!(native_abi.contains(&format!("pub const SYS_CAP_CLOSE: u64 = {SYS_CAP_CLOSE};")));
        assert!(capability_abi.contains(&format!(
            "pub const RIGHT_DEBUG_WRITE: u64 = {RIGHT_DEBUG_WRITE};"
        )));
    }
}
