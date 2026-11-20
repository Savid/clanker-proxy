//! The event stream, from the Durable Object's memory.

use std::pin::pin;

use cp_http::{Caller, Host as _, sse};
use cp_thread::Id;
use futures_channel::mpsc;
use futures_util::future::{Either, select};
use futures_util::{StreamExt as _, stream};
use worker::{Delay, Headers, Response};

use crate::host::DoHost;

struct Open {
    host: DoHost,
    caller: Caller,
    changes: mpsc::Receiver<Id>,
}

/// What to send next, or `None` when the stream is over.
async fn next_frame(s: &mut Open) -> Option<String> {
    loop {
        let change = pin!(s.changes.next());
        let ping = pin!(Delay::from(sse::PING_EVERY));
        let frame = match select(change, ping).await {
            Either::Left((Some(id), _)) => match s.host.core(move |c| c.summary(&id)).await {
                Ok(Some(t)) if sse::visible(&s.caller, &t) => sse::thread(&t),
                _ => continue,
            },
            Either::Left((None, _)) => return None,
            Either::Right(((), _)) => sse::PING.to_string(),
        };
        // Revoking or expiring an agent token ends its streams.
        if !matches!(sse::still_allowed(&s.host, &s.caller).await, Ok(true)) {
            return None;
        }
        return Some(frame);
    }
}

/// Subscribes before answering, so a client that lists threads after
/// connecting misses nothing; then a frame per visible change and a ping
/// every 15 seconds, until the client goes or an agent token stops working.
/// An open stream keeps the object in memory.
pub(crate) fn respond(host: &DoHost, caller: Caller) -> worker::Result<Response> {
    let open = Open {
        host: host.clone(),
        changes: host.subscribe(),
        caller,
    };
    let frames = stream::unfold(Some(open), |state| async move {
        let mut s = state?;
        let frame = next_frame(&mut s).await?;
        Some((frame, Some(s)))
    });
    let body = stream::once(async { sse::retry() })
        .chain(frames)
        .map(|f| Ok::<Vec<u8>, worker::Error>(f.into_bytes()));
    let headers = Headers::new();
    for (k, v) in sse::HEADERS {
        headers.set(k, v)?;
    }
    Ok(Response::from_stream(body)?.with_headers(headers))
}
