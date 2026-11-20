//! The Durable Object as a host for the core: one SQLite transaction per
//! core call, outbound fetch, streams in memory, and the alarm as the
//! delivery scheduler.

use std::cell::RefCell;
use std::pin::pin;
use std::rc::Rc;

use bytes::Bytes;
use cp_api::{Error, ErrorKind};
use cp_core::{Core, Effects, Millis, Runtime};
use cp_http::{FetchError, Host, Incoming, MAX_RESPONSE, Outgoing};
use cp_thread::Id;
use futures_channel::mpsc;
use futures_util::StreamExt as _;
use futures_util::future::{Either, select};
use worker::{
    AbortController, Delay, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, Storage,
};

use crate::sql::{DoSql, TxError, transaction};

/// The isolate's clock and `crypto.getRandomValues`.
pub(crate) struct JsRuntime;

/// Milliseconds since the epoch. Workers advance the clock only across I/O,
/// which is all the core needs: one call sees one time.
pub(crate) fn now_ms() -> i64 {
    #[allow(clippy::cast_possible_truncation)] // whole milliseconds, far below 2^53
    let ms = js_sys::Date::now() as i64;
    ms
}

impl Runtime for JsRuntime {
    fn now(&self) -> Millis {
        Millis(now_ms())
    }

    fn random(&mut self, buf: &mut [u8]) {
        // Workers always provide randomness; without it the daemon cannot
        // mint secrets, so it stops rather than continue weakly.
        if getrandom::fill(buf).is_err() {
            worker::console_error!("no randomness from crypto.getRandomValues");
            std::process::abort();
        }
    }
}

pub(crate) type C = Core<DoSql, JsRuntime>;

/// How many changes a stream may fall behind before it misses some.
const STREAM_BACKLOG: usize = 64;

struct Inner {
    storage: Storage,
    core: RefCell<C>,
    owner_token: String,
    streams: RefCell<Vec<mpsc::Sender<Id>>>,
}

/// The host handle; cheap to clone, and confined to the object's one
/// thread.
#[derive(Clone)]
pub(crate) struct DoHost(Rc<Inner>);

impl DoHost {
    pub(crate) fn new(storage: Storage, core: C, owner_token: String) -> Self {
        Self(Rc::new(Inner {
            storage,
            core: RefCell::new(core),
            owner_token,
            streams: RefCell::default(),
        }))
    }

    /// Thread changes from now on.
    pub(crate) fn subscribe(&self) -> mpsc::Receiver<Id> {
        let (tx, rx) = mpsc::channel(STREAM_BACKLOG);
        self.0.streams.borrow_mut().push(tx);
        rx
    }

    async fn apply(&self, fx: Effects) {
        if !fx.changed.is_empty() {
            // A full stream misses changes rather than slowing the daemon;
            // a closed one is dropped.
            self.0.streams.borrow_mut().retain_mut(|tx| {
                fx.changed
                    .iter()
                    .all(|id| !matches!(tx.try_send(id.clone()), Err(e) if e.is_disconnected()))
            });
        }
        if fx.deliver {
            self.schedule(Some(now_ms())).await;
        }
    }

    /// Moves the alarm to `at`, or to the heartbeat horizon with nothing
    /// due, unless it is already set sooner. Delivery runs only in the
    /// alarm, never inline, so a request never waits on another daemon.
    pub(crate) async fn schedule(&self, at: Option<i64>) {
        let target = at.unwrap_or_else(|| now_ms() + crate::ALARM_HORIZON_MS);
        let storage = &self.0.storage;
        let current = match storage.get_alarm().await {
            Ok(current) => current,
            Err(e) => {
                worker::console_error!("get alarm: {e}");
                None
            }
        };
        // An i64 would be read as an offset from now: pass the instant.
        #[allow(clippy::cast_precision_loss)] // milliseconds since 1970 fit 2^53
        let at =
            worker::ScheduledTime::new(js_sys::Date::new(&js_sys::Number::from(target as f64)));
        if current.is_none_or(|c| c > target)
            && let Err(e) = storage.set_alarm(at).await
        {
            worker::console_error!("set alarm: {e}");
        }
    }
}

/// Opens the core inside one transaction, so a failed start leaves no
/// partial schema behind.
pub(crate) fn open(storage: &Storage, config: cp_core::Config) -> Result<C, String> {
    let sql = storage.sql();
    transaction(storage, move || C::open(DoSql(sql), JsRuntime, config)).map_err(|e| match e {
        TxError::Failed(e) => e.detail,
        TxError::Storage(e) => e,
    })
}

fn fetch_error(e: &worker::Error) -> FetchError {
    let text = e.to_string().to_lowercase();
    if text.contains("dns") || text.contains("resolve") {
        FetchError::Dns
    } else if text.contains("certificate") {
        FetchError::TlsCertificate
    } else if text.contains("tls") || text.contains("ssl") || text.contains("handshake") {
        FetchError::TlsHandshake
    } else if text.contains("refused") {
        FetchError::Refused
    } else {
        FetchError::Connection
    }
}

async fn send(req: Outgoing, signal: &worker::AbortSignal) -> Result<Incoming, FetchError> {
    let headers = Headers::new();
    for (k, v) in &req.headers {
        // Set, not appended: a header given twice keeps the last.
        headers.set(k, v).map_err(|_| FetchError::Connection)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::from(req.method.as_str().to_string()))
        .with_headers(headers)
        .with_redirect(RequestRedirect::Manual);
    if let Some(body) = &req.body {
        init.with_body(Some(js_sys::Uint8Array::from(body.as_ref()).into()));
    }
    let request = Request::new_with_init(&req.url, &init).map_err(|_| FetchError::Connection)?;
    let mut resp = Fetch::Request(request)
        .send_with_signal(signal)
        .await
        .map_err(|e| fetch_error(&e))?;
    let status = resp.status_code();
    if (300..400).contains(&status) {
        return Err(FetchError::Redirect);
    }
    let headers = resp
        .headers()
        .entries()
        .map(|(k, v)| (k.to_lowercase(), v))
        .collect();
    let mut body = Vec::new();
    if let Ok(mut stream) = resp.stream() {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| fetch_error(&e))?;
            let room = MAX_RESPONSE.saturating_sub(body.len());
            body.extend_from_slice(chunk.get(..room.min(chunk.len())).unwrap_or_default());
            if body.len() >= MAX_RESPONSE {
                break;
            }
        }
    }
    Ok(Incoming {
        status,
        headers,
        body: Bytes::from(body),
    })
}

impl Host for DoHost {
    type Sql = DoSql;
    type Rt = JsRuntime;

    async fn core<T, F>(&self, f: F) -> Result<T, Error>
    where
        T: 'static,
        F: FnOnce(&mut C) -> Result<T, Error> + 'static,
    {
        let inner = Rc::clone(&self.0);
        let result = transaction(&self.0.storage, move || {
            // Calls never overlap: each runs to completion synchronously.
            let mut core = inner
                .core
                .try_borrow_mut()
                .map_err(|_| Error::new(ErrorKind::Internal, "overlapping core calls"))?;
            match f(&mut core) {
                Ok(v) => Ok((v, core.take_effects())),
                Err(e) => {
                    core.discard_effects();
                    Err(e)
                }
            }
        });
        match result {
            Ok((v, fx)) => {
                self.apply(fx).await;
                Ok(v)
            }
            Err(TxError::Failed(e)) => Err(e),
            Err(TxError::Storage(e)) => Err(Error::new(ErrorKind::Internal, e)),
        }
    }

    async fn fetch(&self, req: Outgoing) -> Result<Incoming, FetchError> {
        let controller = AbortController::default();
        let timeout = req.timeout;
        let signal = controller.signal();
        let call = pin!(send(req, &signal));
        let timer = pin!(Delay::from(timeout));
        match select(call, timer).await {
            Either::Left((result, _)) => result,
            Either::Right(((), _)) => {
                controller.abort();
                Err(FetchError::Timeout)
            }
        }
    }

    fn owner_token(&self) -> &str {
        &self.0.owner_token
    }

    fn version(&self) -> &'static str {
        concat!("v", env!("CARGO_PKG_VERSION"))
    }
}
