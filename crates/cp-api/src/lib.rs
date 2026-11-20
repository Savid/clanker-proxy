//! clanker-proxy's HTTP API: every request and response body, the
//! operations and who may call them, and the OpenAPI document generated from
//! them. Pure: hosts serve it, `cpctl` calls it.

mod access;
mod doc;
mod error;
mod model;
mod text;
mod webhooks;

pub use access::{Access, Method, Op, prefix};
pub use doc::openapi;
pub use error::{Error, ErrorKind, Problem};
pub use model::*;
pub use text::{
    Code, Cursor, DaemonUrl, Invalid, MAX_NOTE, MAX_PEERS, MAX_URL, MAX_WEBHOOK_URL, Note,
    PeerSecret, Peers, RequestRef, ThreadRef, Token, Version, WebhookHeaderName,
    WebhookHeaderValue, WebhookSecret, WebhookUrl,
};
pub use webhooks::{
    Provider, Webhook, WebhookCreate, WebhookDelivery, WebhookDeliveryList, WebhookEventOrigin,
    WebhookEventType, WebhookEvents, WebhookHeader, WebhookList, WebhookOrigin, WebhookPayload,
    WebhookType, WebhookUpdate,
};

/// The header every call between daemons carries: the caller's release, so
/// owners can see which peers run what.
pub const VERSION_HEADER: &str = "cp-version";

/// The SSE event name thread changes stream under.
pub const STREAM_EVENT: &str = "thread";
/// How long a client waits before reconnecting a dropped stream, in ms.
pub const STREAM_RETRY_MS: u32 = 2000;
/// Largest request body: one thread body plus JSON escaping.
pub const MAX_REQUEST_BODY: usize = 1 << 20;
