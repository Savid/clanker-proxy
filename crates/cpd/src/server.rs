//! The HTTP server: limits, request IDs, access log, panic guard, and the
//! event stream.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use cp_api::{Error, ErrorKind, MAX_REQUEST_BODY};
use cp_http::{Caller, Reply, Service, sse};
use futures_util::{FutureExt as _, StreamExt as _, stream};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Full, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};

use crate::host::NativeHost;

/// The OpenAPI document this build serves.
const SPEC: &str = include_str!("../../../api/openapi.json");

const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
/// Bounds a body trickled in slowly; streams are not affected.
const BODY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HEADERS: usize = 16 << 10;
/// How long shutdown waits for requests in flight.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

type Body = BoxBody<Bytes, Infallible>;

fn full(b: impl Into<Bytes>) -> Body {
    Full::new(b.into())
        .map_err(|e: Infallible| match e {})
        .boxed()
}

fn problem(e: &Error) -> http::Response<Body> {
    let body = serde_json::to_vec(&e.problem()).unwrap_or_default();
    http::Response::builder()
        .status(e.kind.status())
        .header("content-type", "application/problem+json")
        .body(full(body))
        .unwrap_or_default()
}

struct State {
    service: Service<NativeHost>,
    stop: watch::Receiver<bool>,
}

/// Serves the API on `listener` until `stop` changes, then waits briefly for
/// requests in flight; streams end at once.
pub(crate) async fn serve(
    listener: TcpListener,
    service: Service<NativeHost>,
    mut stop: watch::Receiver<bool>,
) {
    let state = Arc::new(State {
        service,
        stop: stop.clone(),
    });
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((tcp, peer)) = accepted else { continue };
                let state = Arc::clone(&state);
                let conn = hyper::server::conn::http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(HEADER_TIMEOUT)
                    .max_buf_size(MAX_HEADERS.max(8192))
                    .serve_connection(
                        TokioIo::new(tcp),
                        hyper::service::service_fn(move |req| {
                            let state = Arc::clone(&state);
                            async move { Ok::<_, Infallible>(handle(&state, req, peer).await) }
                        }),
                    );
                let conn = graceful.watch(conn);
                tokio::spawn(async move {
                    let _ = conn.await;
                });
            }
            _ = stop.changed() => break,
        }
    }
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, graceful.shutdown()).await;
}

fn request_id(req: &http::Request<Incoming>) -> String {
    req.headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .map_or_else(new_request_id, str::to_string)
}

fn new_request_id() -> String {
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

async fn handle(
    state: &State,
    req: http::Request<Incoming>,
    _peer: SocketAddr,
) -> http::Response<Body> {
    let started = Instant::now();
    let id = request_id(&req);
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    let span = tracing::info_span!("request", request_id = %id);
    let _enter = span.enter();

    let mut resp = AssertUnwindSafe(route(state, req))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            tracing::error!("handler panicked");
            problem(&Error::new(ErrorKind::Internal, ""))
        });
    if let Ok(v) = http::HeaderValue::from_str(&id) {
        resp.headers_mut().insert("x-request-id", v);
    }
    let status = resp.status().as_u16();
    if status >= 500 {
        tracing::warn!(%method, %path, status, took_ms = started.elapsed().as_millis(), "request");
    } else {
        tracing::debug!(%method, %path, status, took_ms = started.elapsed().as_millis(), "request");
    }
    resp
}

async fn route(state: &State, req: http::Request<Incoming>) -> http::Response<Body> {
    if req.method() == http::Method::GET && req.uri().path() == "/openapi.json" {
        return http::Response::builder()
            .header("content-type", "application/json")
            .body(full(SPEC))
            .unwrap_or_default();
    }
    let (parts, body) = req.into_parts();
    let body =
        match tokio::time::timeout(BODY_TIMEOUT, Limited::new(body, MAX_REQUEST_BODY).collect())
            .await
        {
            Err(_) => return problem(&Error::new(ErrorKind::Invalid, "request body timed out")),
            Ok(Err(_)) => {
                return problem(&Error::new(
                    ErrorKind::PayloadTooLarge,
                    format!("body is over {MAX_REQUEST_BODY} bytes"),
                ));
            }
            Ok(Ok(collected)) => collected.to_bytes(),
        };
    let req = http::Request::from_parts(parts, body);
    match state.service.handle(req).await {
        Reply::Full(r) => r.map(full),
        Reply::Stream(caller) => stream_response(state, caller),
    }
}

/// The event stream: subscribe first, so a client that lists threads after
/// connecting misses nothing; then a frame per visible change and a ping
/// every 15 seconds, until the client goes, the daemon stops, or an agent
/// token stops working.
fn stream_response(state: &State, caller: Caller) -> http::Response<Body> {
    struct S {
        host: NativeHost,
        caller: Caller,
        rx: broadcast::Receiver<cp_thread::Id>,
        stop: watch::Receiver<bool>,
        ping: tokio::time::Interval,
    }
    let host = state.service.host().clone();
    let rx = host.subscribe();
    let stop = state.stop.clone();
    let mut ping = tokio::time::interval(sse::PING_EVERY);
    ping.reset();

    let frames = stream::unfold(
        Some(S {
            host,
            caller,
            rx,
            stop,
            ping,
        }),
        |state| async move {
            let mut s = state?;
            loop {
                let frame = tokio::select! {
                    _ = s.stop.changed() => return None,
                    _ = s.ping.tick() => Some(sse::PING.to_string()),
                    msg = s.rx.recv() => match msg {
                        Ok(id) => {
                            let summary = s.host.clone();
                            match summary_for(&summary, id).await {
                                Some(t) if sse::visible(&s.caller, &t) => Some(sse::thread(&t)),
                                _ => None,
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => None,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    },
                };
                let Some(frame) = frame else { continue };
                if !matches!(sse::still_allowed(&s.host, &s.caller).await, Ok(true)) {
                    return None;
                }
                return Some((frame, Some(s)));
            }
        },
    );
    let body = stream::once(async { sse::retry() })
        .chain(frames)
        .map(|f| Ok::<_, Infallible>(Frame::data(Bytes::from(f))));
    let mut b = http::Response::builder().status(200);
    for (k, v) in sse::HEADERS {
        b = b.header(*k, *v);
    }
    b.body(http_body_util::BodyExt::boxed(StreamBody::new(body)))
        .unwrap_or_default()
}

async fn summary_for(host: &NativeHost, id: cp_thread::Id) -> Option<cp_api::ThreadSummary> {
    use cp_http::Host as _;
    host.core(move |c| c.summary(&id)).await.ok().flatten()
}
