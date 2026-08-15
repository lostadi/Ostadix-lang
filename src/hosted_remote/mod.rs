//! Bounded remote prepared-operation transport for the hosted-placement V6
//! preview.
//!
//! Scope is deliberately narrow: TLS-authenticated direct node selection and
//! one exact O source operation per request. Registry federation, autonomous
//! placement, leases, retry ledgers, migration, project bundles, and World
//! mutation are outside this module.

mod client;
mod node;
mod paths;
mod protocol;
mod tls;

pub use client::*;
pub use node::*;
pub use paths::*;
pub use protocol::*;
pub use tls::*;
