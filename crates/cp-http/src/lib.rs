//! clanker-proxy's HTTP API over `cp-core`, independent of the host that
//! serves it: routing from the operation table, access checked before the
//! body is read, dispatch to the core, the two-step calls to other daemons,
//! and delivery. Hosts supply transactions, outbound HTTP and streams.

mod gate;
mod host;
pub mod peer;
mod route;
mod service;
pub mod sse;
pub mod webhooks;

pub use gate::Gate;
pub use host::{FetchError, Host, Incoming, MAX_RESPONSE, MaybeSend, MaybeSync, Outgoing};
pub use service::{Caller, Reply, Service};
