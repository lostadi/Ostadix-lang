//! Bounded remote prepared-operation transport for the hosted-placement V6
//! preview.
//!
//! The frozen V1 surface is deliberately narrow: direct node selection and one
//! exact O source operation per request. The V2 surface adds signed placement
//! records and a durable session/retry ledger. Ordinary LAN-open operation
//! discovers and enrolls peers automatically; manual mode preserves explicitly
//! pinned TLS and placement-authority controls. The separately versioned mesh
//! data plane transfers content-addressed project bundles and executes exact
//! route actors under the client-side project-mesh scheduler. Registry
//! federation, node-to-node work stealing/forwarding, stateful actor migration,
//! and World mutation remain outside this module.

mod client;
pub mod fabric;
mod lan;
pub mod mesh;
mod node;
mod pairing;
mod paths;
pub mod project_mesh;
mod protocol;
mod tls;
pub mod v2;

pub use client::*;
pub use fabric::*;
pub use lan::*;
pub use mesh::*;
pub use node::*;
pub use pairing::*;
pub use paths::*;
pub use project_mesh::*;
pub use protocol::*;
pub use tls::*;
