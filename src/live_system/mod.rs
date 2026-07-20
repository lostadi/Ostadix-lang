//! Hosted Live-World semantic reference.
//!
//! This module implements immutable package artifacts, a local
//! content-addressed store, a bounded worker protocol, and transactional host
//! child-process supervision. It does not claim a kernel-resident package
//! manager, native O-core service supervisor, or foreign-compatibility runtime.

pub mod manifest;
pub mod protocol;
pub mod store;
pub mod supervisor;
