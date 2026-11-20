//! Calls to other daemons: asking to peer, confirming an approval, and
//! delivering events. The core signs them; this sends them.

use std::time::Duration;

use bytes::Bytes;
use cp_api::{
    Approval, DaemonUrl, Error, ErrorKind, Peer, PeerInput, Problem, RequestReceipt, RequestRef,
};
use cp_core::{Attempt, Call, EVENTS_PATH, Outcome};

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

/// POSTs `body` to `path` on `url` with `headers`, which sign the call
/// when it needs signing.
async fn post<H: Host>(
    h: &H,
    url: &DaemonUrl,
    path: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> Result<Incoming, FetchError> {
    let mut all = vec![
        ("user-agent".to_string(), "cpd".to_string()),
        (cp_api::VERSION_HEADER.to_string(), h.version().to_string()),
    ];
    all.extend_from_slice(headers);
    if !body.is_empty() {
        all.push(("content-type".to_string(), "application/json".to_string()));
    }
    h.fetch(Outgoing {
        method: http::Method::POST,
        url: url.join(path),
        headers: all,
        body: (!body.is_empty()).then(|| Bytes::from(body)),
        timeout: TIMEOUT,
    })
    .await
}

fn upstream(what: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Upstream, format!("{what}: {detail}"))
}

/// Asks the daemon at the peer's URL to become peers. The peer is recorded
/// only once that daemon has the request and has sent back its key.
pub async fn add<H: Host>(h: &H, input: PeerInput) -> Result<Peer, Error> {
    let name = input.name.clone();
    let call: Call = h.core(move |c| c.add_peer(input)).await?;
    let asked = format!("ask {}", call.url);
    let answer = match post(h, &call.url, call.path, &call.headers, call.body).await {
        Ok(r) if (200..300).contains(&r.status) => r,
        Ok(r) => return Err(upstream(&asked, describe(&r))),
        Err(e) => return Err(upstream(&asked, e)),
    };
    let receipt: RequestReceipt = serde_json::from_slice(&answer.body)
        .map_err(|e| upstream(&asked, format!("unreadable answer: {e}")))?;
    let url = call.url;
    h.core(move |c| c.finish_add_peer(&name, &url, &receipt))
        .await
}

/// Approves a peering request: the requester's daemon must first take the
/// signed acceptance at the URL it gave.
pub async fn approve<H: Host>(h: &H, id: RequestRef, approval: Approval) -> Result<Peer, Error> {
    let pending = h.core(move |c| c.begin_approval(&id, approval)).await?;
    let call = pending.call.clone();
    let result = post(h, &call.url, call.path, &call.headers, call.body).await;
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
                &format!(
                    "the daemon at {} did not confirm this request, so it is not approved",
                    call.url
                ),
                why,
            ))
        }
    }
}

/// Sends one event and says what became of it.
pub async fn send<H: Host>(h: &H, a: &Attempt) -> Outcome {
    match post(h, &a.url, EVENTS_PATH, &a.headers, a.body.clone()).await {
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
