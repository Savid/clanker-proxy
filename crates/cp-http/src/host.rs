//! What the HTTP service needs from the host it runs on.

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use cp_api::Error;
use cp_core::{Core, Runtime, Sql};

/// `Send` on native hosts, nothing on WebAssembly, where everything runs on
/// one thread and JavaScript handles are not `Send`.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSend for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSend for T {}

/// `Sync` on native hosts, nothing on WebAssembly.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync> MaybeSync for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSync for T {}

/// An HTTP request to another daemon or a webhook endpoint.
#[derive(Clone)]
pub struct Outgoing {
    pub method: http::Method,
    /// May hold credentials (a webhook's path or query): never log it.
    pub url: String,
    /// May hold credentials: never log their values.
    pub headers: Vec<(String, String)>,
    pub body: Option<Bytes>,
    pub timeout: Duration,
}

impl std::fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outgoing")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

/// Its answer. The host reads at most [`MAX_RESPONSE`] bytes of body.
#[derive(Clone, Debug)]
pub struct Incoming {
    pub status: u16,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

impl Incoming {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Most of a response the host reads; answers are small, and a larger one
/// is cut off and fails to decode.
pub const MAX_RESPONSE: usize = 64 << 10;

/// Why a request got no answer, by category: never the URL, which can hold
/// credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    Dns,
    TlsCertificate,
    TlsHandshake,
    Timeout,
    Refused,
    /// The other side answered with a redirect, which is never followed:
    /// the request carries a secret.
    Redirect,
    Connection,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FetchError::Dns => "DNS lookup failed",
            FetchError::TlsCertificate => "TLS certificate rejected",
            FetchError::TlsHandshake => "TLS handshake failed",
            FetchError::Timeout => "timed out",
            FetchError::Refused => "connection refused",
            FetchError::Redirect => "redirected; use the daemon's direct URL",
            FetchError::Connection => "connection failed",
        })
    }
}

/// The host: native `cpd`, or the Worker's Durable Object.
pub trait Host: Clone + MaybeSend + MaybeSync + 'static {
    type Sql: Sql + 'static;
    type Rt: Runtime + 'static;

    /// Runs one core call in its own transaction, then acts on its effects
    /// (wakes delivery, tells streams).
    fn core<T, F>(&self, f: F) -> impl Future<Output = Result<T, Error>> + MaybeSend
    where
        T: MaybeSend + 'static,
        F: FnOnce(&mut Core<Self::Sql, Self::Rt>) -> Result<T, Error> + MaybeSend + 'static;

    /// Sends a request without following redirects.
    fn fetch(
        &self,
        req: Outgoing,
    ) -> impl Future<Output = Result<Incoming, FetchError>> + MaybeSend;

    /// The SHA-256 of the owner's token, in hex
    /// ([`cp_core::token_hash`]): the daemon never holds the token itself.
    fn owner_hash(&self) -> &str;

    /// This build's version, as `getMe` reports it.
    fn version(&self) -> &str;
}
