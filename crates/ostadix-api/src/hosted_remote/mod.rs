//! Bounded remote prepared-operation transport for the hosted-placement V6
//! preview.
//!
//! The frozen V1 surface is deliberately narrow: direct node selection and one
//! exact O source operation per request. The V2 surface adds signed placement
//! records and a durable session/retry ledger. Ordinary LAN-open operation
//! discovers and enrolls peers automatically; manual mode preserves explicitly
//! pinned TLS and placement-authority controls. Registry federation, autonomous
//! placement, migration, project bundles, and World mutation remain outside
//! this module.

mod client;
mod lan;
mod node;
mod paths;
mod protocol;
mod tls;
pub mod v2;

pub use client::*;
pub use lan::*;
pub use node::*;
pub use paths::*;
pub use protocol::*;
pub use tls::*;
