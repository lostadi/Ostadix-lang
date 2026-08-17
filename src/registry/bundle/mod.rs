//! Compatibility projection of the canonical compiled backend catalog.
//!
//! The implementation is compiled exactly once as `crate::backend_catalog`.
//! This historical path remains public so existing embedders and binaries do
//! not need to change imports during the 0.2 compatibility window.

pub use crate::backend_catalog::*;
