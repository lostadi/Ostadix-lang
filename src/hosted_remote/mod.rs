//! Bounded remote prepared-operation transport for the hosted-placement V6
//! preview.
//!
//! The frozen V1 surface is deliberately narrow: TLS-authenticated direct node
//! selection and one exact O source operation per request. The opt-in V2
//! surface adds signed placement leases and a durable session/retry ledger.
//! Registry federation, autonomous placement, migration, project bundles, and
//! World mutation remain outside this module.

mod client;
mod node;
mod paths;
mod protocol;
mod tls;
pub mod v2;

pub use client::*;
pub use node::*;
pub use paths::*;
pub use protocol::*;
pub use tls::*;
