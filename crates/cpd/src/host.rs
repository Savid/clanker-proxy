//! The native host: one thread owns the core and its SQLite connection;
//! requests reach it over a channel.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use cp_api::{Error, ErrorKind};
use cp_core::{Core, Effects, Millis, Runtime};
use cp_http::{FetchError, Host, Incoming, MAX_RESPONSE, Outgoing};
use cp_sqlite::Sqlite;
use cp_thread::Id;
use tokio::sync::{Notify, broadcast, oneshot};

/// The machine's clock and randomness.
pub struct OsRuntime;

impl Runtime for OsRuntime {
    fn now(&self) -> Millis {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        Millis(ms)
    }

    fn random(&mut self, buf: &mut [u8]) {
        // The OS source does not fail in practice; a daemon without it
        // cannot mint secrets, so it stops rather than continue weakly.
        if let Err(e) = getrandom::fill(buf) {
            tracing::error!(error = %e, "no randomness from the OS");
            std::process::abort();
        }
    }
}

type C = Core<Sqlite, OsRuntime>;
type Job = Box<dyn FnOnce(&mut C) + Send>;

struct Inner {
    jobs: mpsc::Sender<Job>,
    http: reqwest::Client,
    owner_token: String,
    version: String,
    /// Thread changes, for streams. A slow stream misses changes rather
    /// than slowing the daemon.
    changes: broadcast::Sender<Id>,
    /// Wakes delivery.
    deliver: Notify,
}

/// The host handle; cheap to clone.
#[derive(Clone)]
pub struct NativeHost(Arc<Inner>);

impl NativeHost {
    /// Starts the thread that owns `core`.
    pub fn start(core: C, owner_token: String, version: String) -> Result<Self, String> {
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("cpd-sqlite".into())
            .spawn(move || {
                let mut core = core;
                // Calls run one at a time, as on a Durable Object.
                while let Ok(job) = rx.recv() {
                    job(&mut core);
                }
            })
            .map_err(|e| format!("start storage thread: {e}"))?;
        // reqwest is built without a default TLS provider, to keep aws-lc
        // and its C build out; ring it is. Installing again is harmless.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("cpd")
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        Ok(Self(Arc::new(Inner {
            jobs,
            http,
            owner_token,
            version,
            changes: broadcast::channel(64).0,
            deliver: Notify::new(),
        })))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Id> {
        self.0.changes.subscribe()
    }

    /// Resolves when delivery may have something new to send.
    pub async fn delivery_wanted(&self) {
        self.0.deliver.notified().await;
    }

    pub fn wake_delivery(&self) {
        self.0.deliver.notify_one();
    }

    fn apply(&self, fx: Effects) {
        for id in fx.changed {
            let _ = self.0.changes.send(id);
        }
        if fx.deliver {
            self.wake_delivery();
        }
    }
}

fn fetch_error(e: &reqwest::Error) -> FetchError {
    let mut text = e.to_string().to_lowercase();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push(' ');
        text.push_str(&s.to_string().to_lowercase());
        source = s.source();
    }
    if e.is_timeout() {
        FetchError::Timeout
    } else if text.contains("dns") || text.contains("resolve") || text.contains("name or service") {
        FetchError::Dns
    } else if text.contains("certificate") {
        FetchError::TlsCertificate
    } else if text.contains("tls") || text.contains("handshake") {
        FetchError::TlsHandshake
    } else if text.contains("refused") {
        FetchError::Refused
    } else {
        FetchError::Connection
    }
}

impl Host for NativeHost {
    type Sql = Sqlite;
    type Rt = OsRuntime;

    async fn core<T, F>(&self, f: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut C) -> Result<T, Error> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel::<(Result<T, Error>, Effects)>();
        let job: Job = Box::new(move |core| {
            let result = cp_sqlite::run(core, f);
            let _ = tx.send((result, core.take_effects()));
        });
        self.0
            .jobs
            .send(job)
            .map_err(|_| Error::new(ErrorKind::Internal, "storage thread stopped"))?;
        let (result, fx) = rx
            .await
            .map_err(|_| Error::new(ErrorKind::Internal, "storage thread dropped a call"))?;
        self.apply(fx);
        result
    }

    async fn fetch(&self, req: Outgoing) -> Result<Incoming, FetchError> {
        let mut b = self
            .0
            .http
            .request(req.method, &req.url)
            .timeout(req.timeout);
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        if let Some(body) = req.body {
            b = b.body(body);
        }
        let mut resp = b.send().await.map_err(|e| fetch_error(&e))?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            return Err(FetchError::Redirect);
        }
        let headers = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_lowercase(), v.to_str().ok()?.to_string())))
            .collect();
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| fetch_error(&e))? {
            let room = MAX_RESPONSE.saturating_sub(body.len());
            body.extend_from_slice(chunk.get(..room.min(chunk.len())).unwrap_or_default());
            if body.len() >= MAX_RESPONSE {
                break;
            }
        }
        Ok(Incoming {
            status,
            headers,
            body: Bytes::from(body),
        })
    }

    fn owner_token(&self) -> &str {
        &self.0.owner_token
    }

    fn version(&self) -> &str {
        &self.0.version
    }
}

/// How long the delivery loop sleeps with nothing due, in case a wake-up
/// was missed.
const IDLE: Duration = Duration::from_secs(60);

/// Delivers what is due, then sleeps until the next delivery is due or a
/// core call says there is more, until `stop` resolves.
pub(crate) async fn run_delivery(host: NativeHost, mut stop: tokio::sync::watch::Receiver<bool>) {
    loop {
        if let Err(e) = cp_http::peer::deliver(&host, u32::MAX).await {
            tracing::error!(error = %e, "deliver");
        }
        let next = host.core(|c| c.next_due()).await.ok().flatten();
        let now = OsRuntime.now();
        let wait = next
            .map_or(IDLE, |t| {
                Duration::from_millis(u64::try_from((t.0 - now.0).max(0)).unwrap_or(0))
            })
            .min(IDLE);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = host.delivery_wanted() => {}
            _ = stop.changed() => return,
        }
    }
}
