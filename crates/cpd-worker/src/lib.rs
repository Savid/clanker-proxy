//! clanker-proxy's daemon on Cloudflare: a Worker that turns away what it
//! can without state, and one Durable Object holding the owner's daemon.
//!
//! It exists only for WebAssembly; on other targets the crate is empty, so
//! the workspace builds and lints natively too.
#![cfg(target_arch = "wasm32")]

mod host;
mod sql;
mod stream;

use std::sync::OnceLock;

use bytes::Bytes;
use cp_api::{DaemonUrl, Error, ErrorKind, MAX_REQUEST_BODY, Op};
use cp_http::{Gate, Host as _, Reply, Service};
use cp_thread::Name;
use futures_util::StreamExt as _;
use worker::{
    Context, DurableObject, Env, Headers, Request, Response, Result, ScheduleContext,
    ScheduledEvent, State, durable_object, event,
};

use crate::host::{DoHost, now_ms};

/// The internal path the cron heartbeat uses. The Worker forwards only
/// operation-table routes, so outside requests never reach it.
const HEARTBEAT: &str = "/internal/heartbeat";

/// How far ahead the alarm is kept with nothing due, so a lost alarm is
/// noticed; the cron heartbeat re-arms it too.
pub(crate) const ALARM_HORIZON_MS: i64 = 60 * 60 * 1000;

/// Outbound requests one alarm may make, under the Workers free plan's 50
/// subrequests per invocation; more is left to the next alarm, set at once.
const SUBREQUESTS: u32 = 40;

/// After an alarm fails, how long until it tries again.
const ALARM_RETRY_MS: i64 = 60 * 1000;

/// The OpenAPI document this build serves.
const SPEC: &str = include_str!("../../../api/openapi.json");

/// Public peering requests, limited per client address before they wake the
/// object; the daemon also limits them itself.
const PEERING_LIMITER: &str = "PEERING_RATE";

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(Gate::new)
}

/// The Durable Object holding the daemon. `CP_STORAGE` names it, so an
/// owner can start from empty storage by changing it; the old object is
/// left as it was.
fn daemon(env: &Env) -> Result<worker::Stub> {
    let name = env
        .var("CP_STORAGE")
        .map(|v| v.to_string())
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "owner".into());
    env.durable_object("DAEMON")?
        .id_from_name(&name)?
        .get_stub()
}

/// Sixteen random bytes as hex.
fn random_id() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        return "unknown".into();
    }
    b.iter().fold(String::with_capacity(32), |mut s, x| {
        use std::fmt::Write as _;
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// The caller's request ID if it sent a usable one, else Cloudflare's ray
/// ID, else a new one.
fn request_id(req: &Request) -> String {
    let header = |n: &str| req.headers().get(n).ok().flatten();
    header("x-request-id")
        .filter(|v| !v.is_empty() && v.len() <= 128 && v.bytes().all(|c| c.is_ascii_graphic()))
        .or_else(|| header("cf-ray"))
        .unwrap_or_else(random_id)
}

/// One access-log line. Never the query, headers or body: they can carry
/// tokens or content.
fn log(id: &str, method: &str, path: &str, status: u16, started: i64) {
    let line = serde_json::json!({
        "requestId": id,
        "method": method,
        "path": path,
        "status": status,
        "tookMs": now_ms() - started,
    });
    if status >= 500 {
        worker::console_warn!("{line}");
    } else {
        worker::console_log!("{line}");
    }
}

fn to_response(r: http::Response<Bytes>) -> Result<Response> {
    let headers = Headers::new();
    for (k, v) in r.headers() {
        if let Ok(v) = v.to_str() {
            headers.append(k.as_str(), v)?;
        }
    }
    let status = r.status().as_u16();
    let body = r.into_body();
    let builder = worker::ResponseBuilder::new()
        .with_status(status)
        .with_headers(headers);
    // A 204 or an empty answer must have no body at all.
    Ok(if body.is_empty() {
        builder.empty()
    } else {
        builder.fixed(body.to_vec())
    })
}

fn problem(e: &Error) -> Result<Response> {
    let mut b = http::Response::builder()
        .status(e.kind.status())
        .header("content-type", "application/problem+json");
    if let Some(s) = e.retry_after {
        b = b.header("retry-after", s.to_string());
    }
    to_response(
        b.body(Bytes::from(
            serde_json::to_vec(&e.problem()).unwrap_or_default(),
        ))
        .unwrap_or_default(),
    )
}

fn with_id(mut resp: Response, id: &str) -> Response {
    let _ = resp.headers_mut().set("x-request-id", id);
    resp
}

fn http_method(m: &worker::Method) -> http::Method {
    http::Method::from_bytes(m.as_ref().as_bytes()).unwrap_or(http::Method::GET)
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let started = now_ms();
    let method = http_method(&req.method());
    let path = req.path();
    if method == http::Method::GET && path == "/openapi.json" {
        let headers = Headers::new();
        headers.set("content-type", "application/json")?;
        return Ok(Response::ok(SPEC)?.with_headers(headers));
    }
    let authorization = req.headers().get("authorization")?;
    let length = req
        .headers()
        .get("content-length")?
        .and_then(|v| v.parse::<u64>().ok());
    let signed = req.headers().get(cp_api::SIGNATURE_HEADER)?.is_some();
    let refused = match gate().check(&method, &path, authorization.as_deref(), signed, length) {
        Err(r) => Some(to_response(*r)?),
        Ok(Op::RequestPeering) => match env.rate_limiter(PEERING_LIMITER) {
            Ok(limiter) => {
                let client = req
                    .headers()
                    .get("cf-connecting-ip")?
                    .unwrap_or_else(|| "unknown".into());
                // A limiter that fails lets the request through: the daemon
                // keeps its own limit.
                let allowed = !matches!(limiter.limit(client).await, Ok(o) if !o.success);
                if allowed {
                    None
                } else {
                    let mut e = Error::new(
                        ErrorKind::Unavailable,
                        "too many peering requests; try again in a minute",
                    );
                    e.retry_after = Some(60);
                    Some(problem(&e)?)
                }
            }
            // Without the binding, the daemon's own limit still applies.
            Err(_) => None,
        },
        Ok(_) => None,
    };
    if let Some(resp) = refused {
        let id = request_id(&req);
        log(&id, method.as_str(), &path, resp.status_code(), started);
        return Ok(with_id(resp, &id));
    }
    daemon(&env)?.fetch_with_request(req).await
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let beat = async {
        daemon(&env)?
            .fetch_with_str(&format!("https://daemon{HEARTBEAT}"))
            .await
    };
    if let Err(e) = beat.await {
        worker::console_error!("heartbeat: {e}");
    }
}

/// The daemon's settings, from the Worker's variables and secrets.
fn settings(env: &Env) -> std::result::Result<(cp_core::Config, String), String> {
    let text = |name: &str| -> Option<String> {
        env.secret(name)
            .map(|s| s.to_string())
            .or_else(|_| env.var(name).map(|v| v.to_string()))
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let name = text("CP_NAME").ok_or("set the CP_NAME variable to the owner's name")?;
    let name = Name::normalize(&name).map_err(|e| format!("CP_NAME: {e}"))?;
    let url =
        text("CP_URL").ok_or("set the CP_URL variable to the URL peers reach this Worker at")?;
    let url = DaemonUrl::try_from(url).map_err(|e| format!("CP_URL: {e}"))?;
    // The owner keeps the token; the Worker holds only its SHA-256.
    let token = text("CP_OWNER_TOKEN_HASH").ok_or("set the CP_OWNER_TOKEN_HASH secret")?;
    let hex = token.len() == 64
        && token
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
    if !hex {
        return Err(
            "CP_OWNER_TOKEN_HASH must be the owner token's SHA-256 in lower-case hex".into(),
        );
    }
    Ok((
        cp_core::Config {
            name: Some(name),
            url: Some(url),
        },
        token,
    ))
}

#[durable_object]
pub struct Daemon {
    /// The daemon, or why it cannot run.
    service: std::result::Result<Service<DoHost>, String>,
}

impl Daemon {
    async fn serve(&self, mut req: Request) -> Result<Response> {
        let started = now_ms();
        let id = request_id(&req);
        let method = http_method(&req.method());
        let path = req.path();
        let service = match &self.service {
            Ok(s) => s,
            Err(why) => {
                let resp = problem(&Error::new(
                    ErrorKind::Unavailable,
                    format!("the daemon cannot start: {why}"),
                ))?;
                log(&id, method.as_str(), &path, 503, started);
                return Ok(with_id(resp, &id));
            }
        };
        if path == HEARTBEAT {
            let next = service.host().core(next_due).await.ok().flatten();
            service.host().schedule(next).await;
            return Response::empty();
        }

        let mut b = http::Request::builder()
            .method(method.clone())
            .uri(req.url().map_or_else(
                |_| path.clone(),
                |u| match u.query() {
                    Some(q) => format!("{}?{q}", u.path()),
                    None => u.path().to_string(),
                },
            ));
        for (k, v) in req.headers().entries() {
            b = b.header(k, v);
        }
        // Read at most one byte past the limit, whatever the client declared.
        let mut body = Vec::new();
        if let Ok(mut s) = req.stream() {
            while let Some(chunk) = s.next().await {
                body.extend_from_slice(&chunk?);
                if body.len() > MAX_REQUEST_BODY {
                    let resp = problem(&Error::new(
                        ErrorKind::PayloadTooLarge,
                        format!("body is over {MAX_REQUEST_BODY} bytes"),
                    ))?;
                    log(&id, method.as_str(), &path, 413, started);
                    return Ok(with_id(resp, &id));
                }
            }
        }
        let Ok(http_req) = b.body(Bytes::from(body)) else {
            return problem(&Error::new(ErrorKind::Invalid, "malformed request"));
        };
        let resp = match service.handle(http_req).await {
            Reply::Full(r) => to_response(r)?,
            Reply::Stream(caller) => stream::respond(service.host(), caller)?,
        };
        log(&id, method.as_str(), &path, resp.status_code(), started);
        Ok(with_id(resp, &id))
    }
}

/// When delivery next has work: peers or webhooks, whichever is sooner.
fn next_due(c: &mut host::C) -> std::result::Result<Option<i64>, Error> {
    let peers = c.next_due()?;
    let hooks = c.next_webhook_due()?;
    Ok(peers.into_iter().chain(hooks).min().map(|m| m.0))
}

/// Delivers what is due within the subrequest budget and sets the next
/// alarm. Never fails: an alarm that throws is retried only a few times.
async fn deliver(host: &DoHost) {
    let mut sent = 0;
    let mut failed = false;
    match cp_http::peer::deliver(host, SUBREQUESTS).await {
        Ok(n) => sent += n,
        Err(e) => {
            worker::console_error!("deliver: {}", e.problem().title);
            failed = true;
        }
    }
    match cp_http::webhooks::deliver(host, SUBREQUESTS.saturating_sub(sent)).await {
        Ok(n) => sent += n,
        Err(e) => {
            worker::console_error!("webhooks: {}", e.problem().title);
            failed = true;
        }
    }
    let next = if sent >= SUBREQUESTS {
        Some(now_ms())
    } else if failed {
        Some(now_ms() + ALARM_RETRY_MS)
    } else {
        match host.core(next_due).await {
            Ok(next) => next,
            Err(_) => Some(now_ms() + ALARM_RETRY_MS),
        }
    };
    host.schedule(next).await;
}

impl DurableObject for Daemon {
    fn new(state: State, env: Env) -> Self {
        let storage = state.storage();
        let service = settings(&env).and_then(|(config, token)| {
            let core = host::open(&storage, config)?;
            Ok(Service::new(DoHost::new(state.storage(), core, token)))
        });
        if let Err(why) = &service {
            worker::console_error!("daemon cannot start: {why}");
        }
        Self { service }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.serve(req).await
    }

    async fn alarm(&self) -> Result<Response> {
        if let Ok(service) = &self.service {
            deliver(service.host()).await;
        }
        Response::empty()
    }
}
