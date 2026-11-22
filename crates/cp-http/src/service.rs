//! The API: route, check access before reading the body, decode, call the
//! core, encode.

use bytes::Bytes;
use cp_api::{
    Access, AgentTokenInput, Approval, Error, ErrorKind, GetThreadQuery, Health, HealthStatus,
    ListThreadsQuery, MAX_REQUEST_BODY, Me, NewThread, Op, PeerAliases, PeerInput, PeeringRequest,
    Peers, RequestRef, ThreadAction, ThreadList, ThreadRef, WireEvent, prefix,
};
use cp_core::SignedCall;
use cp_thread::Name;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::host::Host;
use crate::peer;
use crate::route::{Route, Router};

/// Who security let in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    Public,
    Owner,
    /// An agent token; `peers` empty means every peer.
    Agent {
        name: Name,
        token: String,
        peers: Peers,
    },
    /// Another daemon, by the key its signature verified with.
    Peer {
        key: cp_api::PublicKey,
    },
}

impl Caller {
    /// The peers whose threads the caller reaches; `None` is every peer.
    pub fn scope(&self) -> Option<&Peers> {
        match self {
            Caller::Agent { peers, .. } if !peers.is_empty() => Some(peers),
            _ => None,
        }
    }
}

/// What the service answers.
pub enum Reply {
    Full(http::Response<Bytes>),
    /// An event stream for this caller; the host runs it (see `crate::sse`).
    Stream(Caller),
}

/// The API over a host.
pub struct Service<H> {
    host: H,
    router: Router,
}

pub(crate) fn problem(e: &Error) -> http::Response<Bytes> {
    let body = serde_json::to_vec(&e.problem()).unwrap_or_default();
    let mut b = http::Response::builder()
        .status(e.kind.status())
        .header(http::header::CONTENT_TYPE, "application/problem+json");
    if e.kind == ErrorKind::Unauthorized {
        b = b.header(
            http::header::WWW_AUTHENTICATE,
            r#"Bearer realm="clanker-proxy""#,
        );
    }
    if let Some(s) = e.retry_after {
        b = b.header(http::header::RETRY_AFTER, s.to_string());
    }
    b.body(Bytes::from(body)).unwrap_or_default()
}

fn json<T: Serialize>(status: u16, v: &T) -> Reply {
    match serde_json::to_vec(v) {
        Ok(body) => Reply::Full(
            http::Response::builder()
                .status(status)
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Bytes::from(body))
                .unwrap_or_default(),
        ),
        Err(e) => Reply::Full(problem(&Error::new(ErrorKind::Internal, e.to_string()))),
    }
}

fn empty() -> Reply {
    Reply::Full(
        http::Response::builder()
            .status(204)
            .body(Bytes::new())
            .unwrap_or_default(),
    )
}

/// The error every authentication failure gets: which part was wrong is not
/// the caller's business.
pub(crate) fn unauthorized() -> Error {
    Error::new(ErrorKind::Unauthorized, "missing or invalid bearer token")
}

fn bearer(req: &http::Request<Bytes>) -> Option<&str> {
    bearer_token(
        req.headers()
            .get(http::header::AUTHORIZATION)?
            .to_str()
            .ok()?,
    )
}

/// The token in an `Authorization: Bearer` header value.
pub(crate) fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// 405 with the methods the path does take.
pub(crate) fn wrong_method(methods: &[cp_api::Method]) -> http::Response<Bytes> {
    let allow: Vec<&str> = methods.iter().map(|m| m.as_str()).collect();
    let mut r = problem(&Error::new(ErrorKind::MethodNotAllowed, ""));
    if let Ok(v) = http::HeaderValue::from_str(&allow.join(", ")) {
        r.headers_mut().insert(http::header::ALLOW, v);
    }
    r
}

fn decode<T: DeserializeOwned>(req: &http::Request<Bytes>) -> Result<T, Error> {
    let ct = req
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if ct.split(';').next().map(str::trim) != Some("application/json") {
        return Err(Error::new(
            ErrorKind::UnsupportedMediaType,
            "send application/json",
        ));
    }
    if req.body().len() > MAX_REQUEST_BODY {
        return Err(Error::new(
            ErrorKind::PayloadTooLarge,
            format!("body is over {MAX_REQUEST_BODY} bytes"),
        ));
    }
    serde_json::from_slice(req.body())
        .map_err(|e| Error::new(ErrorKind::Invalid, format!("invalid request body: {e}")))
}

fn query<T: DeserializeOwned + Default>(req: &http::Request<Bytes>) -> Result<T, Error> {
    match req.uri().query() {
        None | Some("") => Ok(T::default()),
        Some(q) => serde_urlencoded::from_str(q)
            .map_err(|e| Error::new(ErrorKind::Invalid, format!("invalid query: {e}"))),
    }
}

fn param<T: TryFrom<String>>(params: &[(String, String)], name: &str) -> Result<T, Error> {
    let raw = params
        .iter()
        .find(|(k, _)| k == name)
        .map_or_else(String::new, |(_, v)| v.clone());
    T::try_from(raw).map_err(|_| Error::new(ErrorKind::Invalid, format!("invalid {name}")))
}

fn param_name(params: &[(String, String)], name: &str) -> Result<Name, Error> {
    let raw = params
        .iter()
        .find(|(k, _)| k == name)
        .map_or("", |(_, v)| v.as_str());
    Name::normalize(raw).map_err(|e| Error::new(ErrorKind::Invalid, e.to_string()))
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq as _;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

impl<H: Host> Service<H> {
    pub fn new(host: H) -> Self {
        Self {
            host,
            router: Router::new(),
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    /// Answers one request. The body is already read, within
    /// [`MAX_REQUEST_BODY`].
    pub async fn handle(&self, req: http::Request<Bytes>) -> Reply {
        match self.serve(req).await {
            Ok(r) => r,
            Err(e) => Reply::Full(problem(&e)),
        }
    }

    async fn serve(&self, req: http::Request<Bytes>) -> Result<Reply, Error> {
        let (op, params) = match self.router.route(req.method(), req.uri().path()) {
            Route::Found(op, params) => (op, params),
            Route::WrongMethod(methods) => return Ok(Reply::Full(wrong_method(&methods))),
            Route::NotFound => return Err(Error::new(ErrorKind::NotFound, "no such API route")),
        };
        let header = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok());
        let signed = (op.access() == Access::Peer)
            .then(|| SignedCall::new(header, req.method().as_str(), req.uri().path(), req.body()))
            .flatten();
        let caller = self
            .authenticate(op, bearer(&req), header(cp_api::VERSION_HEADER), signed)
            .await?;
        self.dispatch(op, caller, &params, &req).await
    }

    /// Checks the caller may call `op`: owners and agents by bearer token,
    /// other daemons by the signature `signed` read from the request. A
    /// peer's reported release is recorded with it.
    pub async fn authenticate(
        &self,
        op: Op,
        token: Option<&str>,
        version: Option<&str>,
        signed: Option<SignedCall>,
    ) -> Result<Caller, Error> {
        let token = token.unwrap_or_default().to_string();
        // The daemon holds only the token's hash; comparing hashes in
        // constant time keeps timing from leaking how much of it matched.
        let owner = token.starts_with(prefix::OWNER)
            && constant_time_eq(&cp_core::token_hash(&token), self.host.owner_hash());
        match op.access() {
            Access::Public => Ok(Caller::Public),
            Access::Owner | Access::OwnerOrAgent if owner => Ok(Caller::Owner),
            Access::Owner | Access::OwnerOrAgent | Access::Agent
                if token.starts_with(prefix::AGENT) =>
            {
                let t = token.clone();
                let revoking = op == Op::RevokeOwnToken;
                let agent = self
                    .host
                    .core(move |c| {
                        if revoking {
                            c.authenticate_agent_to_revoke(&t)
                        } else {
                            c.authenticate_agent(&t)
                        }
                    })
                    .await
                    .map_err(|e| {
                        if e.kind == ErrorKind::Internal {
                            e
                        } else {
                            unauthorized()
                        }
                    })?;
                if op.access() == Access::Owner {
                    return Err(Error::new(
                        ErrorKind::Forbidden,
                        "this needs the owner token; an agent token is limited to the operations the API grants it",
                    ));
                }
                Ok(Caller::Agent {
                    name: agent.name,
                    token,
                    peers: agent.peers,
                })
            }
            Access::Peer => {
                let call = signed.ok_or_else(|| {
                    Error::new(
                        ErrorKind::Unauthorized,
                        "calls between daemons need Cp-Key, Cp-Date and Cp-Signature",
                    )
                })?;
                let key = call.key.clone();
                // A malformed report is ignored, not refused: it is only shown.
                let v = version.and_then(|v| cp_api::Version::try_from(v.to_string()).ok());
                self.host
                    .core(move |c| {
                        let name = c.authenticate_peer(&call)?;
                        if let Some(v) = &v {
                            c.note_peer_version(&name, v)?;
                        }
                        Ok(name)
                    })
                    .await?;
                Ok(Caller::Peer { key })
            }
            _ => Err(unauthorized()),
        }
    }

    #[allow(clippy::too_many_lines)] // one arm per operation reads best together
    async fn dispatch(
        &self,
        op: Op,
        caller: Caller,
        params: &[(String, String)],
        req: &http::Request<Bytes>,
    ) -> Result<Reply, Error> {
        let h = &self.host;
        let scope = caller.scope().cloned();
        Ok(match op {
            Op::GetHealth => {
                let at = h.core(|c| Ok(c.now_at())).await?;
                json(
                    200,
                    &Health {
                        status: HealthStatus::Ok,
                        at,
                    },
                )
            }
            Op::GetMe => {
                let version = h.version().to_string();
                let peers = scope.clone();
                let me = h
                    .core(move |c| {
                        Ok(Me {
                            name: c.me().clone(),
                            url: c.url().cloned(),
                            version,
                            peers,
                        })
                    })
                    .await?;
                json(200, &me)
            }
            Op::RequestPeering => {
                let body: PeeringRequest = decode(req)?;
                json(202, &h.core(move |c| c.request_received(&body)).await?)
            }
            Op::ListRequests => json(200, &h.core(|c| c.requests()).await?),
            Op::DenyRequest => {
                let id: RequestRef = param(params, "id")?;
                h.core(move |c| c.deny(&id)).await?;
                empty()
            }
            Op::ApproveRequest => {
                let id: RequestRef = param(params, "id")?;
                let approval: Approval = decode(req)?;
                json(201, &peer::approve(h, id, approval).await?)
            }
            Op::ListPeers => json(200, &h.core(move |c| c.peers(scope.as_ref())).await?),
            Op::AddPeer => {
                let input: PeerInput = decode(req)?;
                json(201, &peer::add(h, input).await?)
            }
            Op::GetPeer => {
                let name = param_name(params, "name")?;
                json(200, &h.core(move |c| c.peer(&name, scope.as_ref())).await?)
            }
            Op::RemovePeer => {
                let name = param_name(params, "name")?;
                h.core(move |c| c.remove_peer(&name)).await?;
                empty()
            }
            Op::SetPeerAliases => {
                let name = param_name(params, "name")?;
                let body: PeerAliases = decode(req)?;
                json(
                    200,
                    &h.core(move |c| c.set_peer_aliases(&name, &body.aliases))
                        .await?,
                )
            }
            Op::ListThreads => {
                let q: ListThreadsQuery = query(req)?;
                let threads = h.core(move |c| c.threads(&q, scope.as_ref())).await?;
                json(200, &ThreadList { threads })
            }
            Op::OpenThread => {
                let n: NewThread = decode(req)?;
                json(
                    201,
                    &h.core(move |c| c.open_thread(n, scope.as_ref())).await?,
                )
            }
            Op::GetThread => {
                let r: ThreadRef = param(params, "ref")?;
                let q: GetThreadQuery = query(req)?;
                json(
                    200,
                    &h.core(move |c| c.thread(&r, scope.as_ref(), &q)).await?,
                )
            }
            Op::ActOnThread => {
                let r: ThreadRef = param(params, "ref")?;
                let a: ThreadAction = decode(req)?;
                json(
                    200,
                    &h.core(move |c| c.act(&r, scope.as_ref(), a.action, a.body))
                        .await?,
                )
            }
            Op::StreamEvents => Reply::Stream(caller),
            Op::DeliverEvent => {
                let Caller::Peer { key } = caller else {
                    return Err(unauthorized());
                };
                let e: WireEvent = decode(req)?;
                json(200, &h.core(move |c| c.receive(&key, e)).await?)
            }
            Op::NotifyPeeringAccepted => {
                let Caller::Peer { key } = caller else {
                    return Err(unauthorized());
                };
                h.core(move |c| c.accepted(&key)).await?;
                empty()
            }
            Op::SetOwnWebhook | Op::DeleteOwnWebhook => {
                let Caller::Agent { name, peers, .. } = caller else {
                    return Err(unauthorized());
                };
                if op == Op::DeleteOwnWebhook {
                    h.core(move |c| c.delete_agent_webhook(&name)).await?;
                    return Ok(empty());
                }
                let w: cp_api::OwnWebhook = decode(req)?;
                json(
                    200,
                    &h.core(move |c| c.set_agent_webhook(&name, &peers, &w))
                        .await?,
                )
            }
            Op::ListAgentTokens => json(200, &h.core(|c| c.agent_tokens()).await?),
            Op::CreateAgentToken => {
                let input: AgentTokenInput = decode(req)?;
                json(201, &h.core(move |c| c.create_agent_token(input)).await?)
            }
            Op::DeleteAgentToken => {
                let name = param_name(params, "name")?;
                h.core(move |c| c.delete_agent_token(&name)).await?;
                empty()
            }
            Op::RevokeOwnToken => {
                let Caller::Agent { token, .. } = caller else {
                    return Err(unauthorized());
                };
                h.core(move |c| c.revoke_own_agent_token(&token)).await?;
                empty()
            }
            Op::ListWebhooks => {
                let webhooks = h.core(|c| c.webhooks()).await?;
                json(200, &cp_api::WebhookList { webhooks })
            }
            Op::CreateWebhook => {
                let w: cp_api::WebhookCreate = decode(req)?;
                json(201, &h.core(move |c| c.create_webhook(w)).await?)
            }
            Op::GetWebhook => {
                let name = param_name(params, "name")?;
                json(200, &h.core(move |c| c.webhook(&name)).await?)
            }
            Op::UpdateWebhook => {
                let name = param_name(params, "name")?;
                let u: cp_api::WebhookUpdate = decode(req)?;
                json(200, &h.core(move |c| c.update_webhook(&name, u)).await?)
            }
            Op::DeleteWebhook => {
                let name = param_name(params, "name")?;
                h.core(move |c| c.delete_webhook(&name)).await?;
                empty()
            }
            Op::ListWebhookDeliveries => {
                let name = param_name(params, "name")?;
                let q: cp_api::WebhookDeliveriesQuery = query(req)?;
                json(
                    200,
                    &h.core(move |c| {
                        c.webhook_deliveries(&name, q.cursor.as_ref(), q.limit, q.status)
                    })
                    .await?,
                )
            }
            Op::RetryWebhookDelivery => {
                let name = param_name(params, "name")?;
                let id: cp_thread::Id = param(params, "id")?;
                h.core(move |c| c.retry_webhook(&name, &id)).await?;
                empty()
            }
        })
    }
}
