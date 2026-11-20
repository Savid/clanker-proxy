//! Calls to cpd.

use std::net::IpAddr;
use std::time::Duration;

use cp_api::Problem;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::fail::{EXIT_ERROR, EXIT_UNREACHABLE, EXIT_USAGE, Failure, from_problem};

/// How long one API call may take; streams are unbounded.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A connection to cpd for one command.
pub(crate) struct Client {
    http: reqwest::Client,
    stream_http: reqwest::Client,
    base: String,
    token: String,
    /// The command running, for hints.
    pub(crate) command: String,
    /// The thread the command acts on, for hints.
    pub(crate) thread_ref: Option<String>,
}

fn redirect() -> Failure {
    Failure::usage("cpd answered with a redirect, which is refused: the request carries your token")
        .hint("set CP_URL (or --url) to cpd's direct URL; redirects are refused")
}

/// Plain http is only for cpd on this machine: the token travels in every
/// request.
fn check_transport(base: &str) -> Result<(), Failure> {
    let bad = || {
        Failure::usage(format!(
            "CP_URL {base:?}: want https://host, or http on this machine"
        ))
    };
    let rest = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .ok_or_else(bad)?;
    if base.starts_with("https://") {
        return Ok(());
    }
    let host = rest.split('/').next().unwrap_or_default();
    let host = if host.starts_with('[') {
        host.split(']')
            .next()
            .unwrap_or_default()
            .trim_start_matches('[')
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    if host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
        Ok(())
    } else {
        Err(bad())
    }
}

impl Client {
    pub(crate) fn new(base: &str, token: &str, command: &str) -> Result<Self, Failure> {
        check_transport(base)?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let build = |timeout: Option<Duration>| {
            let mut b = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("cpctl");
            if let Some(t) = timeout {
                b = b.timeout(t);
            }
            b.build()
                .map_err(|e| Failure::new(EXIT_ERROR, format!("HTTP client: {e}")))
        };
        Ok(Self {
            http: build(Some(CALL_TIMEOUT))?,
            stream_http: build(None)?,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            command: command.to_string(),
            thread_ref: None,
        })
    }

    /// Whether the token is an agent token, for hints.
    pub(crate) fn is_agent(&self) -> bool {
        self.token.starts_with(cp_api::prefix::AGENT)
    }

    fn unreachable(&self, e: &reqwest::Error) -> Failure {
        if e.is_timeout() {
            return Failure::new(
                EXIT_ERROR,
                format!("cpd at {} did not answer in time", self.base),
            );
        }
        Failure::new(
            EXIT_UNREACHABLE,
            format!("cannot reach cpd at {}", self.base),
        )
        .hint("start cpd, or set CP_URL (or --url) to where it listens")
    }

    async fn failure(&self, resp: reqwest::Response) -> Failure {
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            return redirect();
        }
        let body = resp.bytes().await.unwrap_or_default();
        let p: Problem = serde_json::from_slice(&body).unwrap_or_else(|_| Problem {
            problem_type: None,
            title: format!("HTTP {status}"),
            status,
            detail: None,
            instance: None,
        });
        from_problem(
            status,
            &p,
            &self.command,
            self.thread_ref.as_deref(),
            self.is_agent(),
        )
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, Failure> {
        let resp = req
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| self.unreachable(&e))?;
        if resp.status().is_success() {
            Ok(resp)
        } else {
            Err(self.failure(resp).await)
        }
    }

    async fn decode<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<(T, String), Failure> {
        let text = resp.text().await.map_err(|e| self.unreachable(&e))?;
        let v = serde_json::from_str(&text)
            .map_err(|e| Failure::new(EXIT_ERROR, format!("cpd's answer: {e}")))?;
        Ok((v, text))
    }

    /// GET, returning the decoded body and its JSON text (for `--json`).
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<(T, String), Failure> {
        let resp = self
            .send(self.http.get(format!("{}{path}", self.base)))
            .await?;
        self.decode(resp).await
    }

    pub(crate) async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(T, String), Failure> {
        let json = serde_json::to_vec(body).map_err(|e| Failure::new(EXIT_ERROR, e.to_string()))?;
        let req = self
            .http
            .post(format!("{}{path}", self.base))
            .header("content-type", "application/json")
            .body(json);
        let resp = self.send(req).await?;
        self.decode(resp).await
    }

    pub(crate) async fn delete(&self, path: &str) -> Result<(), Failure> {
        self.send(self.http.delete(format!("{}{path}", self.base)))
            .await
            .map(drop)
    }

    /// Opens the event stream; once this returns, every later change is
    /// delivered on it.
    pub(crate) async fn stream(&self) -> Result<reqwest::Response, Failure> {
        let req = self
            .stream_http
            .get(format!("{}/api/v1/stream", self.base))
            .header("accept", "text/event-stream");
        self.send(req).await
    }
}

/// Characters a URL path segment may carry unescaped; names, IDs and refs
/// never need more.
pub(crate) fn segment(s: &str) -> Result<&str, Failure> {
    if !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        Ok(s)
    } else {
        Err(Failure::new(
            EXIT_USAGE,
            format!("{s:?} is not a name, ID or thread reference"),
        ))
    }
}
