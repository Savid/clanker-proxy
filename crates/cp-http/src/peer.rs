//! Calls to other daemons: asking to peer, confirming an approval, and
//! delivering events.

use std::time::Duration;

use bytes::Bytes;
use cp_api::{Approval, Error, ErrorKind, Peer, PeerInput, Problem, RequestRef};
use cp_core::{Attempt, Outbound, Outcome};

use crate::host::{FetchError, Host, Incoming, Outgoing};

/// How long a call to another daemon may take.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// Longest text kept from another daemon's answer: it reaches the owner's
/// agent.
const MAX_DETAIL: usize = 300;

fn clip(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_control() || cp_thread::is_hidden(c) {
                ' '
            } else {
                c
            }
        })
        .take(MAX_DETAIL)
        .collect();
    if s.chars().count() > MAX_DETAIL {
        out.push('…');
    }
    out
}

/// What an answer says, as `status title: detail`. A proxy can answer with
/// HTML or nothing, so this never depends on the body decoding.
fn describe(r: &Incoming) -> String {
    match serde_json::from_slice::<Problem>(&r.body) {
        Ok(p) => clip(&match p.detail {
            Some(d) => format!("{} {}: {d}", r.status, p.title),
            None => format!("{} {}", r.status, p.title),
        }),
        Err(_) => format!("HTTP {}", r.status),
    }
}

async fn post<H: Host>(
    h: &H,
    url: String,
    bearer: Option<&str>,
    body: Option<Vec<u8>>,
) -> Result<Incoming, FetchError> {
    let mut headers = vec![("user-agent".to_string(), "cpd".to_string())];
    if let Some(b) = bearer {
        headers.push(("authorization".to_string(), format!("Bearer {b}")));
    }
    if body.is_some() {
        headers.push(("content-type".to_string(), "application/json".to_string()));
    }
    h.fetch(Outgoing {
        method: http::Method::POST,
        url,
        headers,
        body: body.map(Bytes::from),
        timeout: TIMEOUT,
    })
    .await
}

fn upstream(what: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Upstream, format!("{what}: {detail}"))
}

/// Asks the daemon at the peer's URL to become peers. The core saves the
/// requested peer first and forgets it if the request does not get through.
pub async fn add<H: Host>(h: &H, input: PeerInput) -> Result<Peer, Error> {
    let name = input.name.clone();
    let out: Outbound = h.core(move |c| c.add_peer(input)).await?;
    let body = serde_json::to_vec(&out.request)
        .map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))?;
    let result = post(
        h,
        out.url.join("/api/v1/peering-requests"),
        None,
        Some(body),
    )
    .await;
    let failure = match &result {
        Ok(r) if (200..300).contains(&r.status) => None,
        Ok(r) => Some(describe(r)),
        Err(e) => Some(e.to_string()),
    };
    if let Some(why) = failure {
        let (n, secret) = (name.clone(), out.secret.clone());
        h.core(move |c| c.abort_add_peer(&n, &secret)).await?;
        return Err(upstream(&format!("ask {}", out.url), why));
    }
    h.core(move |c| c.requested_peer(&name)).await
}

/// Approves a peering request: the requester's daemon must first confirm,
/// at the URL it gave, that it holds the secret it offered.
pub async fn approve<H: Host>(h: &H, id: RequestRef, approval: Approval) -> Result<Peer, Error> {
    let pending = h.core(move |c| c.begin_approval(&id, approval)).await?;
    let url = pending.call.url.clone();
    let result = post(
        h,
        url.join("/api/v1/federation/accepted"),
        Some(pending.call.secret.expose()),
        None,
    )
    .await;
    let failure = match &result {
        Ok(r) if (200..300).contains(&r.status) => None,
        Ok(r) => Some(describe(r)),
        Err(e) => Some(e.to_string()),
    };
    match failure {
        None => h.core(move |c| c.commit_approval(&pending)).await,
        Some(why) => {
            h.core(move |c| c.abort_approval(&pending)).await?;
            Err(upstream(
                &format!("the daemon at {url} did not confirm this request, so it is not approved"),
                why,
            ))
        }
    }
}

/// Sends one event and says what became of it.
pub async fn send<H: Host>(h: &H, a: &Attempt) -> Outcome {
    let body = match serde_json::to_vec(&a.event) {
        Ok(b) => b,
        Err(e) => return Outcome::Refused(e.to_string()),
    };
    match post(
        h,
        a.url.join("/api/v1/federation/events"),
        Some(a.secret.expose()),
        Some(body),
    )
    .await
    {
        Ok(r) => Outcome::from_status(r.status, describe(&r)),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Sends what is due, each peer's events in order, until nothing more can
/// go now; at most `budget` requests. Returns how many it sent.
pub async fn deliver<H: Host>(h: &H, budget: u32) -> Result<u32, Error> {
    let mut sent = 0u32;
    while sent < budget {
        let due = h.core(move |c| c.due(budget)).await?;
        if due.is_empty() {
            break;
        }
        let mut progressed = false;
        for a in due {
            if sent >= budget {
                break;
            }
            let check = a.clone();
            if !h.core(move |c| c.still_pending(&check)).await? {
                continue;
            }
            let outcome = send(h, &a).await;
            sent += 1;
            progressed |= matches!(outcome, Outcome::Delivered | Outcome::Refused(_));
            h.core(move |c| c.record(&a, outcome)).await?;
        }
        if !progressed {
            break; // the rest wait out their backoff
        }
    }
    Ok(sent)
}
