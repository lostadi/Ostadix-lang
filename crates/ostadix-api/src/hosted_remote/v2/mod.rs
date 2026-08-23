//! Durable, session-oriented hosted transport.
//!
//! V2 is deliberately separate from the frozen one-operation V1 wire
//! protocol.  A V2 session is bound to the authenticated client certificate
//! and a high-entropy bearer, journals every mutation before acknowledging it,
//! and never executes without an exact, internally consistent placement lease.
//! Manual mode authenticates that lease against a pinned authority. LAN-open
//! mode deliberately accepts any signing key after exact live-request binding.

mod auth;
mod client;
mod crypto;
mod dev;
mod protocol;
mod runtime;
mod server;
mod store;

pub use auth::*;
pub use client::*;
pub use crypto::*;
pub use dev::*;
pub use protocol::*;
pub use runtime::*;
pub use server::*;
pub use store::*;
