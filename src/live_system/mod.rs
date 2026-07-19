//! Hosted live-system foundations.
//!
//! This module deliberately stops at immutable package artifacts and their
//! local content-addressed store. It does not claim a kernel-resident package
//! manager, supervisor, or foreign-compatibility runtime.

pub mod manifest;
pub mod store;
