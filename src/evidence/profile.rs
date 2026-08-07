//! Soft scheduling measurements.
//!
//! Evidence-Bound Scheduler v1 deliberately carries unknown estimates rather
//! than learning topology from receipts. Later profile calibration can fill
//! these values, but admission must continue to ignore them when deciding
//! which dependency edges are legal.

pub use super::fact::CostEstimateV1;
