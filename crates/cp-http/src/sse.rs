//! The event stream's frames and rules, for hosts to run.

use std::time::Duration;

use cp_api::{Error, ErrorKind, STREAM_EVENT, STREAM_RETRY_MS, ThreadSummary};

use crate::host::Host;
use crate::service::Caller;

/// How often an idle stream is pinged: below common proxy idle timeouts,
/// and how a dead client is found.
pub const PING_EVERY: Duration = Duration::from_secs(15);

/// The stream's first frame: how long a client waits before reconnecting.
pub fn retry() -> String {
    format!("retry: {STREAM_RETRY_MS}\n\n")
}

/// A keepalive comment.
pub const PING: &str = ": ping\n\n";

/// A thread change. Compact JSON has no line breaks, so one data line holds
/// it.
pub fn thread(t: &ThreadSummary) -> String {
    let data = serde_json::to_string(t).unwrap_or_default();
    format!("event: {STREAM_EVENT}\ndata: {data}\n\n")
}

/// Response headers for a stream.
pub const HEADERS: &[(&str, &str)] = &[
    ("content-type", "text/event-stream"),
    // no-transform keeps proxies (Cloudflare among them) from compressing,
    // and so buffering, the stream.
    ("cache-control", "no-cache, no-transform"),
    ("x-accel-buffering", "no"),
];

/// Whether `caller` may see `t`.
pub fn visible(caller: &Caller, t: &ThreadSummary) -> bool {
    caller.scope().is_none_or(|peers| peers.reaches(&t.peer))
}

/// Whether the caller's stream may go on: an agent token is checked again
/// before each event and ping, so revoking or expiring it ends its streams.
pub async fn still_allowed<H: Host>(h: &H, caller: &Caller) -> Result<bool, Error> {
    let Caller::Agent { token, .. } = caller else {
        return Ok(true);
    };
    let t = token.clone();
    match h.core(move |c| c.authenticate_agent(&t)).await {
        Ok(_) => Ok(true),
        Err(e) if e.kind == ErrorKind::Unauthorized => Ok(false),
        Err(e) => Err(e),
    }
}
