//! Sending webhook notifications.

use std::time::Duration;

use bytes::Bytes;
use cp_api::Error;
use cp_core::{WEBHOOK_TIMEOUT_SECS, WebhookJob, WebhookResult};

use crate::host::{FetchError, Host, Outgoing};

/// Endpoints sent to at once; each has at most one request in flight.
const PARALLEL: usize = 4;
/// How much of an answer is kept: enough for Discord's `retry_after`.
const BODY: usize = 4096;

async fn send<H: Host>(h: &H, job: WebhookJob) -> Result<(), Error> {
    let Some(send) = h.core(move |c| c.begin_webhook(&job)).await? else {
        return Ok(());
    };
    let req = Outgoing {
        method: http::Method::POST,
        url: send.request.url.clone(),
        headers: send.request.headers.clone(),
        body: Some(Bytes::from(send.request.body.clone())),
        timeout: Duration::from_secs(WEBHOOK_TIMEOUT_SECS),
    };
    let result = match h.fetch(req).await {
        Ok(r) => WebhookResult::Status {
            code: r.status,
            retry_after: r.header("retry-after").map(str::to_string),
            body: r
                .body
                .get(..BODY.min(r.body.len()))
                .unwrap_or_default()
                .to_vec(),
        },
        Err(FetchError::Redirect) => WebhookResult::Redirect,
        Err(e) => WebhookResult::Transport(e.to_string()),
    };
    h.core(move |c| c.record_webhook(&send, &result)).await
}

/// Expires and prunes old notifications, then sends what is due, at most
/// `budget` requests. Returns how many it sent.
pub async fn deliver<H: Host>(h: &H, budget: u32) -> Result<u32, Error> {
    h.core(cp_core::Core::maintain_webhooks).await?;
    let jobs = h.core(move |c| c.webhook_due(budget)).await?;
    let sent = u32::try_from(jobs.len()).unwrap_or(u32::MAX);
    for batch in jobs.chunks(PARALLEL) {
        let results =
            futures_util::future::join_all(batch.iter().cloned().map(|j| send(h, j))).await;
        for r in results {
            r?;
        }
    }
    Ok(sent)
}
