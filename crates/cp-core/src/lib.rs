//! clanker-proxy's daemon logic over SQL: threads, peering, agent tokens,
//! and scheduling delivery to peers.
//!
//! The core is sans-IO. Each method is synchronous and runs in one
//! transaction the host opens around the call; it never awaits, reads the
//! clock or makes randomness (the host's [`Runtime`] provides those), and
//! never touches the network. Steps that call another daemon are split into
//! a prepare and a commit (see [`Core::add_peer`], [`Core::begin_approval`]).

mod core;
mod delivery;
mod peering;
mod runtime;
mod schema;
mod secrets;
mod sql;
mod threads;
mod tokens;
mod webhooks;

pub use crate::core::{Config, Core, Effects};
pub use delivery::{Attempt, GIVE_UP_401, MAX_BACKOFF, MIN_BACKOFF, Outcome, backoff};
pub use peering::{
    CONFIRM_TTL, MAX_PENDING_REQUESTS, Outbound, PendingApproval, REQUEST_TTL, REQUESTS_PER_MINUTE,
};
pub use runtime::{Millis, Runtime};
pub use schema::{SCHEMA_VERSION, STATEMENTS};
pub use sql::{Row, Sql, SqlError, Value};
pub use tokens::Agent;
pub use webhooks::{
    MAX_PEERING_NOTICES, Message, WEBHOOK_TIMEOUT_SECS, WEBHOOK_TTL, WebhookJob, WebhookRequest,
    WebhookResult, WebhookSend, render, signature,
};
