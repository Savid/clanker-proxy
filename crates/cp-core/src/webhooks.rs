//! Webhooks: owner-configured notifications of thread and peering events.
//!
//! A notification is queued in the same transaction as the event it reports,
//! carries metadata only (receivers read the thread themselves), and is
//! retried with backoff for a week. Each endpoint has at most one request in
//! flight and its own pause after failures, so one bad endpoint never holds
//! up the others.

use std::fmt::Write as _;
use std::net::IpAddr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use cp_api::{
    Cursor, DeliveryStatus, Error, ErrorKind, MAX_DELIVERIES, OwnWebhook, Peers, Webhook,
    WebhookCreate, WebhookDelivery, WebhookDeliveryList, WebhookEventOrigin, WebhookEventType,
    WebhookEvents, WebhookHeader, WebhookHeaderName, WebhookOrigin, WebhookPayload, WebhookType,
    WebhookUpdate,
};
use cp_thread::{Id, Name, State};
use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::{Digest as _, Sha256};

use crate::core::Core;
use crate::delivery::{MAX_BACKOFF, backoff};
use crate::runtime::{Millis, Runtime};
use crate::sql::{Row, Sql, text};

/// How long a notification is retried.
pub const WEBHOOK_TTL: i64 = 7 * Millis::DAY;
/// How long finished deliveries stay in the history.
const HISTORY: i64 = 7 * Millis::DAY;
/// Most peering notifications kept per endpoint: requests are public, so a
/// flood must not fill the queue.
pub const MAX_PEERING_NOTICES: u32 = 100;
/// Most custom headers on a webhook.
const MAX_HEADERS: usize = 16;
/// How long a webhook request may take.
pub const WEBHOOK_TIMEOUT_SECS: u64 = 10;

fn invalid(detail: &str) -> Error {
    Error::new(ErrorKind::Invalid, detail)
}

/// Checks a destination. Errors never echo the URL, which can hold
/// credentials.
fn check_url(raw: &str) -> Result<url::Url, Error> {
    let u = url::Url::parse(raw).map_err(|_| {
        invalid("webhook URL must be an absolute URL without credentials or fragment")
    })?;
    if u.host_str().is_none_or(str::is_empty)
        || !u.username().is_empty()
        || u.password().is_some()
        || raw.contains('#')
        || u.cannot_be_a_base()
    {
        return Err(invalid(
            "webhook URL must be an absolute URL without credentials or fragment",
        ));
    }
    let loopback = match u.host() {
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        _ => false,
    };
    if u.scheme() != "https" && !(u.scheme() == "http" && loopback) {
        return Err(invalid(
            "webhook URL requires HTTPS, or HTTP on a literal loopback address",
        ));
    }
    Ok(u)
}

/// Checks an agent token's own webhook goes somewhere public. An agent
/// token lives where peers' text runs, so it must not be able to make the
/// daemon post into the network it sits in.
fn check_public(raw: &str) -> Result<(), Error> {
    let u = check_url(raw)?;
    let private = match u.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            let tld = d.rsplit('.').next().unwrap_or_default();
            matches!(tld, "localhost" | "local" | "internal")
        }
        Some(url::Host::Ipv4(ip)) => {
            let [a, b, ..] = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                || (a == 100 && (64..128).contains(&b))
        }
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.to_ipv4_mapped().is_some()
        }
        None => true,
    };
    if u.scheme() != "https" || private {
        return Err(invalid("an agent token's webhook needs a public https URL"));
    }
    Ok(())
}

fn reserved(name: &str) -> bool {
    name.starts_with("webhook-")
        || name.starts_with("proxy-")
        || matches!(
            name,
            "host"
                | "content-length"
                | "content-type"
                | "connection"
                | "transfer-encoding"
                | "trailer"
                | "te"
                | "upgrade"
                | "expect"
                | "user-agent"
                | "idempotency-key"
        )
}

/// A webhook as stored, secrets included.
#[derive(Clone)]
struct Config {
    name: Name,
    kind: WebhookType,
    url: String,
    events: WebhookEvents,
    origin: WebhookOrigin,
    peers: Peers,
    enabled: bool,
    secret: String,
    headers: Vec<(String, String)>,
    paused_until: Millis,
    /// The agent token that registered it for itself, if one did.
    agent: Option<Name>,
}

impl Config {
    fn validate(&self) -> Result<(), Error> {
        check_url(&self.url)?;
        if !self.peers.is_empty()
            && matches!(&self.events, WebhookEvents::Only(t) if t.contains(&WebhookEventType::PeeringRequested))
        {
            return Err(invalid(
                "peering.requested goes only to webhooks without peers; leave it out or drop --peers",
            ));
        }
        if !self.secret.is_empty() {
            if self.kind != WebhookType::Generic {
                return Err(invalid(
                    "signing keys apply only to generic webhooks; provider destinations use their URL or authentication headers",
                ));
            }
            if STANDARD
                .decode(&self.secret)
                .map_or(true, |k| k.len() != 32)
            {
                return Err(invalid(
                    "webhook secret must be the base64 encoding of exactly 32 bytes",
                ));
            }
        }
        if self.headers.len() > MAX_HEADERS {
            return Err(invalid("at most 16 webhook headers are allowed"));
        }
        let mut seen: Vec<String> = Vec::new();
        for (n, _) in &self.headers {
            let lower = n.to_ascii_lowercase();
            if seen.contains(&lower) {
                return Err(invalid("webhook header names must be unique ignoring case"));
            }
            if reserved(&lower) {
                return Err(invalid(
                    "transport, Content-Type, User-Agent, Idempotency-Key, Proxy-* and Webhook-* headers are managed by the daemon",
                ));
            }
            seen.push(lower);
        }
        Ok(())
    }

    /// What identifies where and how notifications go: a failure's pause
    /// applies only while it is unchanged, so a failure seen before the owner
    /// fixed the endpoint cannot pause the fixed one.
    fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.url.as_bytes());
        h.update([0]);
        h.update(self.secret.as_bytes());
        for (n, v) in &self.headers {
            h.update([0]);
            h.update(n.as_bytes());
            h.update([0]);
            h.update(v.as_bytes());
        }
        h.finalize().iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    fn view(&self, now: Millis) -> Webhook {
        let destination = url::Url::parse(&self.url).map_or_else(
            |_| String::new(),
            |u| match u.port() {
                Some(p) => format!("{}://{}:{p}", u.scheme(), u.host_str().unwrap_or_default()),
                None => format!("{}://{}", u.scheme(), u.host_str().unwrap_or_default()),
            },
        );
        let mut header_names: Vec<WebhookHeaderName> = self
            .headers
            .iter()
            .filter_map(|(n, _)| WebhookHeaderName::try_from(canonical(n)).ok())
            .collect();
        header_names.sort();
        Webhook {
            name: self.name.clone(),
            kind: self.kind,
            destination,
            events: self.events.clone(),
            origin: self.origin,
            peers: self.peers.clone(),
            enabled: self.enabled,
            signing: !self.secret.is_empty(),
            header_names,
            paused_until: (self.paused_until > now).then(|| self.paused_until.at()),
            agent: self.agent.clone(),
        }
    }
}

/// A header name as HTTP writes it: `x-api-key` → `X-Api-Key`.
fn canonical(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut c = part.chars();
            c.next().map_or_else(String::new, |f| {
                f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase()
            })
        })
        .collect::<Vec<_>>()
        .join("-")
}

const CONFIG_COLUMNS: &str =
    "name, type, url, events, origin, peers, enabled, secret, headers, retry_after, agent";

fn json_err(_: serde_json::Error) -> Error {
    Error::new(ErrorKind::Internal, "stored webhook settings")
}

fn config_from(r: &Row) -> Result<Config, Error> {
    let headers: Vec<(String, String)> = serde_json::from_str(r.text(8)?).map_err(json_err)?;
    Ok(Config {
        name: r.parse(0)?,
        kind: serde_json::from_value(serde_json::Value::String(r.text(1)?.into()))
            .map_err(json_err)?,
        url: r.text(2)?.into(),
        events: serde_json::from_str(r.text(3)?).map_err(json_err)?,
        origin: serde_json::from_value(serde_json::Value::String(r.text(4)?.into()))
            .map_err(json_err)?,
        peers: serde_json::from_str(r.text(5)?).map_err(json_err)?,
        enabled: r.flag(6)?,
        secret: r.text(7)?.into(),
        headers,
        paused_until: Millis(r.int(9)?),
        agent: r.opt_parse(10)?,
    })
}

fn headers_of(list: &[WebhookHeader]) -> Vec<(String, String)> {
    list.iter()
        .map(|h| (h.name.as_str().to_string(), h.value.expose().to_string()))
        .collect()
}

/// What a notification reports, before it is matched to webhooks.
pub(crate) struct Notice {
    pub kind: WebhookEventType,
    pub origin: WebhookEventOrigin,
    pub subject: String,
    pub event_id: Option<Id>,
    pub peer: Name,
    pub state: Option<State>,
    pub my_turn: Option<bool>,
}

/// A notification due to be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebhookJob {
    pub id: Id,
    pub webhook: Name,
    pub attempts: u32,
}

/// A request to an endpoint. Its URL and headers can hold credentials:
/// never log them.
#[derive(Clone)]
pub struct WebhookRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for WebhookRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebhookRequest([redacted])")
    }
}

/// A notification being sent: the request, and what the outcome needs.
#[derive(Clone, Debug)]
pub struct WebhookSend {
    pub job: WebhookJob,
    pub request: WebhookRequest,
    fingerprint: String,
    retry_until: Millis,
    kind: WebhookType,
}

/// What an endpoint answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebhookResult {
    Status {
        code: u16,
        /// The `Retry-After` header, if any.
        retry_after: Option<String>,
        /// The start of the body, for Discord's `retry_after`.
        body: Vec<u8>,
    },
    /// The endpoint answered with a redirect, which is never followed.
    Redirect,
    /// No answer, by category (never the URL).
    Transport(String),
}

/// The wait after `n` failures of `key`, with up to a quarter more, derived
/// from the key so it is stable across restarts and spreads retries out.
fn jittered(key: &str, n: u32) -> i64 {
    let base = backoff(n);
    let byte = Sha256::digest(format!("{key}:{n}").as_bytes())
        .first()
        .copied()
        .unwrap_or(0);
    base + base * i64::from(byte) / (4 * 255)
}

/// When a receiver's requested retry falls, capped at an hour and the
/// notification's window.
fn requested_retry(
    r: &WebhookResult,
    kind: WebhookType,
    now: Millis,
    until: Millis,
) -> Option<Millis> {
    let WebhookResult::Status {
        code,
        retry_after,
        body,
    } = r
    else {
        return None;
    };
    let mut at = None;
    if matches!(code, 429 | 503)
        && let Some(h) = retry_after.as_deref().map(str::trim)
    {
        at = if let Ok(s) = h.parse::<i64>() {
            (s >= 0).then(|| now.plus(s.saturating_mul(Millis::SECOND)))
        } else {
            jiff::fmt::rfc2822::DateTimeParser::new()
                .parse_timestamp(h)
                .ok()
                .map(|t| Millis(t.as_millisecond()))
        };
    }
    if *code == 429 && kind == WebhookType::Discord {
        let seconds = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("retry_after")?.as_f64())
            .filter(|s| *s > 0.0);
        if let Some(s) = seconds {
            // Clamped before converting, so a huge value cannot overflow.
            let whole = s.ceil().min(3600.0);
            at = Some(
                now.plus(Millis::SECOND * format!("{whole:.0}").parse::<i64>().unwrap_or(3600)),
            );
        }
    }
    at.map(|t| t.min(now.plus(MAX_BACKOFF)).min(until))
}

impl<S: Sql, R: Runtime> Core<S, R> {
    fn load_webhook(&self, name: &Name) -> Result<Config, Error> {
        self.sql
            .query_one(
                &format!("SELECT {CONFIG_COLUMNS} FROM webhooks WHERE name = ?"),
                &[text(name)],
            )?
            .map(|r| config_from(&r))
            .transpose()?
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "webhook not found"))
    }

    fn save_webhook(&self, c: &Config, insert: bool) -> Result<(), Error> {
        let values = [
            text(&c.name),
            c.kind.as_str().into(),
            c.url.clone().into(),
            serde_json::to_string(&c.events).map_err(json_err)?.into(),
            serde_json::to_value(c.origin)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
                .into(),
            serde_json::to_string(&c.peers).map_err(json_err)?.into(),
            c.enabled.into(),
            c.secret.clone().into(),
            serde_json::to_string(&c.headers).map_err(json_err)?.into(),
        ];
        let sql = if insert {
            "INSERT INTO webhooks (name, type, url, events, origin, peers, enabled, secret, headers) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        } else {
            // An owner's change resets the endpoint's backoff.
            "UPDATE webhooks SET type = ?2, url = ?3, events = ?4, origin = ?5, peers = ?6, enabled = ?7, secret = ?8, headers = ?9, retry_after = 0, failures = 0 WHERE name = ?1"
        };
        self.sql.execute(sql, &values)?;
        Ok(())
    }

    /// Adds a webhook.
    pub fn create_webhook(&mut self, w: WebhookCreate) -> Result<Webhook, Error> {
        let c = Config {
            name: w.name,
            kind: w.kind,
            url: w.url.expose().to_string(),
            events: w.events,
            origin: w.origin,
            peers: self.known_peers(&w.peers)?,
            enabled: w.enabled,
            secret: w
                .secret
                .as_ref()
                .map_or_else(String::new, |s| s.expose().to_string()),
            headers: headers_of(&w.headers),
            paused_until: Millis::default(),
            agent: None,
        };
        c.validate()?;
        if self
            .sql
            .query_one("SELECT 1 FROM webhooks WHERE name = ?", &[text(&c.name)])?
            .is_some()
        {
            return Err(Error::new(
                ErrorKind::Conflict,
                "webhook name already exists",
            ));
        }
        self.save_webhook(&c, true)?;
        Ok(c.view(self.now()))
    }

    /// Every webhook, by name.
    pub fn webhooks(&self) -> Result<Vec<Webhook>, Error> {
        let now = self.now();
        self.sql
            .query(
                &format!("SELECT {CONFIG_COLUMNS} FROM webhooks ORDER BY name"),
                &[],
            )?
            .iter()
            .map(|r| config_from(r).map(|c| c.view(now)))
            .collect()
    }

    pub fn webhook(&self, name: &Name) -> Result<Webhook, Error> {
        Ok(self.load_webhook(name)?.view(self.now()))
    }

    /// Changes a webhook: omitted settings are kept, and an invalid result
    /// changes nothing. Narrowing its peers drops queued notifications about
    /// the others.
    pub fn update_webhook(&mut self, name: &Name, u: WebhookUpdate) -> Result<Webhook, Error> {
        let mut c = self.load_webhook(name)?;
        if let Some(url) = u.url {
            c.url = url.expose().to_string();
        }
        if let Some(e) = u.events {
            c.events = e;
        }
        if let Some(o) = u.origin {
            c.origin = o;
        }
        if let Some(p) = u.peers {
            c.peers = self.known_peers(&p)?;
        }
        if let Some(e) = u.enabled {
            c.enabled = e;
        }
        if let Some(s) = u.secret {
            c.secret = s.expose().to_string();
        }
        if let Some(h) = u.headers {
            c.headers = headers_of(&h);
        }
        c.validate()?;
        self.save_webhook(&c, false)?;
        if !c.peers.is_empty() {
            self.sql.execute(
                "DELETE FROM webhook_deliveries WHERE webhook = ?1 AND status = 'pending' AND sending = 0 \
                 AND (event = 'peering.requested' OR peer NOT IN (SELECT value FROM json_each(?2)))",
                &[text(name), serde_json::to_string(&c.peers).map_err(json_err)?.into()],
            )?;
        }
        self.webhook(name)
    }

    /// Registers, or replaces, the webhook the agent token `agent` has for
    /// itself: generic, signed, for the peers' moves only, and only for the
    /// token's `peers`. Lets an agent runner be woken without the owner.
    pub fn set_agent_webhook(
        &mut self,
        agent: &Name,
        peers: &Peers,
        w: &OwnWebhook,
    ) -> Result<Webhook, Error> {
        let url = w.url.expose().to_string();
        check_public(&url)?;
        if w.secret.expose().is_empty() {
            return Err(invalid("an agent token's webhook must be signed"));
        }
        if let Ok(existing) = self.load_webhook(agent)
            && existing.agent.as_ref() != Some(agent)
        {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("the owner has a webhook named {agent}; they can remove or rename it"),
            ));
        }
        let c = Config {
            name: agent.clone(),
            kind: WebhookType::Generic,
            url,
            events: WebhookEvents::All,
            origin: WebhookOrigin::Incoming,
            peers: peers.clone(),
            enabled: true,
            secret: w.secret.expose().to_string(),
            headers: Vec::new(),
            paused_until: Millis::default(),
            agent: Some(agent.clone()),
        };
        c.validate()?;
        let exists = self.load_webhook(agent).is_ok();
        self.save_webhook(&c, !exists)?;
        self.sql.execute(
            "UPDATE webhooks SET agent = ? WHERE name = ?",
            &[text(agent), text(agent)],
        )?;
        self.webhook(agent)
    }

    /// Removes the webhook the agent token `agent` registered, and its
    /// deliveries.
    pub fn delete_agent_webhook(&mut self, agent: &Name) -> Result<(), Error> {
        self.sql.execute(
            "DELETE FROM webhook_deliveries WHERE webhook IN (SELECT name FROM webhooks WHERE agent = ?)",
            &[text(agent)],
        )?;
        if self
            .sql
            .execute("DELETE FROM webhooks WHERE agent = ?", &[text(agent)])?
            == 0
        {
            return Err(Error::new(ErrorKind::NotFound, "this token has no webhook"));
        }
        Ok(())
    }

    /// Deletes a webhook and its deliveries.
    pub fn delete_webhook(&mut self, name: &Name) -> Result<(), Error> {
        self.sql.execute(
            "DELETE FROM webhook_deliveries WHERE webhook = ?",
            &[text(name)],
        )?;
        if self
            .sql
            .execute("DELETE FROM webhooks WHERE name = ?", &[text(name)])?
            == 0
        {
            return Err(Error::new(ErrorKind::NotFound, "webhook not found"));
        }
        Ok(())
    }

    /// A page of a webhook's deliveries, newest first.
    pub fn webhook_deliveries(
        &self,
        name: &Name,
        cursor: Option<&Cursor>,
        limit: Option<u32>,
        status: Option<DeliveryStatus>,
    ) -> Result<WebhookDeliveryList, Error> {
        self.load_webhook(name)?;
        let limit = limit.unwrap_or(MAX_DELIVERIES).clamp(1, MAX_DELIVERIES);
        let before: i64 = match cursor {
            Some(c) => c
                .as_str()
                .parse()
                .map_err(|_| invalid("invalid delivery cursor"))?,
            None => i64::MAX,
        };
        let status = status.map(|s| match s {
            DeliveryStatus::Pending => "pending",
            DeliveryStatus::Delivered => "delivered",
            DeliveryStatus::Failed => "failed",
        });
        let rows = self.sql.query(
            "SELECT seq, id, event, origin, subject, status, attempts, created_at, next_attempt_at, last_error \
             FROM webhook_deliveries WHERE webhook = ?1 AND seq < ?2 AND (?3 IS NULL OR status = ?3) \
             ORDER BY seq DESC LIMIT ?4",
            &[text(name), before.into(), status.into(), (limit + 1).into()],
        )?;
        let mut deliveries = Vec::with_capacity(rows.len());
        let mut next_cursor = None;
        for (i, r) in rows.iter().enumerate() {
            if i == usize::try_from(limit).unwrap_or(usize::MAX) {
                break;
            }
            let status = match r.text(5)? {
                "delivered" => DeliveryStatus::Delivered,
                "failed" => DeliveryStatus::Failed,
                _ => DeliveryStatus::Pending,
            };
            deliveries.push(WebhookDelivery {
                id: r.parse(1)?,
                event: serde_json::from_value(serde_json::Value::String(r.text(2)?.into()))
                    .map_err(json_err)?,
                origin: serde_json::from_value(serde_json::Value::String(r.text(3)?.into()))
                    .map_err(json_err)?,
                subject: r.text(4)?.into(),
                status,
                attempts: r.count(6)?,
                created_at: Millis(r.int(7)?).at(),
                next_attempt_at: (status == DeliveryStatus::Pending)
                    .then(|| Millis(r.int(8).unwrap_or(0)).at()),
                last_error: r.text(9)?.into(),
            });
            next_cursor = Some(r.int(0)?);
        }
        let more = rows.len() > deliveries.len();
        Ok(WebhookDeliveryList {
            deliveries,
            next_cursor: if more {
                next_cursor.and_then(|c| Cursor::try_from(c.to_string()).ok())
            } else {
                None
            },
        })
    }

    /// Queues a failed delivery again, with a fresh window, and clears the
    /// endpoint's pause.
    pub fn retry_webhook(&mut self, name: &Name, id: &Id) -> Result<(), Error> {
        let c = self.load_webhook(name)?;
        let now = self.now();
        let row = self
            .sql
            .query_one(
                "SELECT status FROM webhook_deliveries WHERE webhook = ? AND id = ?",
                &[text(name), text(id)],
            )?
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("webhook {name} has no delivery {id}"),
                )
            })?;
        if !c.enabled || row.text(0)? != "failed" {
            return Err(Error::new(
                ErrorKind::Conflict,
                "only failed deliveries of enabled webhooks can be retried",
            ));
        }
        self.sql.execute(
            "UPDATE webhook_deliveries SET status = 'pending', attempts = 0, last_error = '', next_attempt_at = ?1, \
             retry_until = ?2, finished_at = NULL, sending = 0 WHERE id = ?3",
            &[now.0.into(), now.plus(WEBHOOK_TTL).0.into(), text(id)],
        )?;
        self.sql.execute(
            "UPDATE webhooks SET retry_after = 0, failures = 0 WHERE name = ?",
            &[text(name)],
        )?;
        self.effects.deliver = true;
        Ok(())
    }

    /// Queues `n` for every webhook that wants it. Runs in the transaction
    /// that stores the event, so both commit or neither does.
    pub(crate) fn notify(&mut self, n: &Notice) -> Result<(), Error> {
        let now = self.now();
        let hooks: Vec<Config> = self
            .sql
            .query(
                &format!("SELECT {CONFIG_COLUMNS} FROM webhooks WHERE enabled = 1"),
                &[],
            )?
            .iter()
            .map(config_from)
            .collect::<Result<_, _>>()?;
        let peering = n.kind == WebhookEventType::PeeringRequested;
        let origin = match n.origin {
            WebhookEventOrigin::Incoming => "incoming",
            WebhookEventOrigin::Outgoing => "outgoing",
        };
        for h in hooks {
            let wanted = h.origin.includes(n.origin)
                && h.events.includes(n.kind)
                && (h.peers.is_empty() || (!peering && h.peers.as_slice().contains(&n.peer)));
            if !wanted {
                continue;
            }
            // Receivers read current state, so a newer notification about the
            // same thing replaces one not yet sent.
            if peering {
                self.sql.execute(
                    "DELETE FROM webhook_deliveries WHERE webhook = ? AND event = 'peering.requested' AND status = 'pending' AND sending = 0",
                    &[text(&h.name)],
                )?;
            } else {
                self.sql.execute(
                    "DELETE FROM webhook_deliveries WHERE webhook = ? AND event = ? AND origin = ? AND subject = ? AND status = 'pending' AND sending = 0",
                    &[text(&h.name), n.kind.as_str().into(), origin.into(), n.subject.clone().into()],
                )?;
            }
            let id = self.new_id();
            let payload = WebhookPayload {
                id: id.clone(),
                kind: n.kind,
                origin: n.origin,
                at: now.at(),
                subject: n.subject.clone(),
                event_id: n.event_id.clone(),
                peer: n.peer.clone(),
                state: n.state,
                my_turn: n.my_turn,
            };
            self.sql.execute(
                "INSERT INTO webhook_deliveries (id, webhook, event, origin, subject, peer, payload, status, created_at, next_attempt_at, retry_until) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?)",
                &[
                    text(&id),
                    text(&h.name),
                    n.kind.as_str().into(),
                    origin.into(),
                    n.subject.clone().into(),
                    text(&n.peer),
                    serde_json::to_string(&payload).map_err(json_err)?.into(),
                    now.0.into(),
                    now.0.into(),
                    now.plus(WEBHOOK_TTL).0.into(),
                ],
            )?;
            if peering {
                self.sql.execute(
                    "DELETE FROM webhook_deliveries WHERE webhook = ?1 AND event = 'peering.requested' AND seq NOT IN \
                     (SELECT seq FROM webhook_deliveries WHERE webhook = ?1 AND event = 'peering.requested' ORDER BY seq DESC LIMIT ?2)",
                    &[text(&h.name), MAX_PEERING_NOTICES.into()],
                )?;
            }
            self.effects.deliver = true;
        }
        Ok(())
    }

    /// Notifications due, at most one per endpoint, so a slow endpoint only
    /// delays itself.
    pub fn webhook_due(&self, budget: u32) -> Result<Vec<WebhookJob>, Error> {
        let now = self.now().0;
        let rows = self.sql.query(
            "SELECT d.id, d.webhook, d.attempts FROM webhook_deliveries d JOIN webhooks w ON w.name = d.webhook \
             WHERE w.enabled = 1 AND w.retry_after <= ?1 AND d.status = 'pending' AND d.next_attempt_at <= ?1 AND d.retry_until > ?1 \
             AND d.seq = (SELECT x.seq FROM webhook_deliveries x WHERE x.webhook = d.webhook AND x.status = 'pending' \
                          AND x.next_attempt_at <= ?1 AND x.retry_until > ?1 ORDER BY x.next_attempt_at, x.seq LIMIT 1) \
             ORDER BY d.next_attempt_at, d.seq LIMIT ?2",
            &[now.into(), budget.into()],
        )?;
        rows.iter()
            .map(|r| {
                Ok(WebhookJob {
                    id: r.parse(0)?,
                    webhook: r.parse(1)?,
                    attempts: r.count(2)?,
                })
            })
            .collect()
    }

    /// Marks `job` in flight and renders its request, if it is still due as
    /// it was.
    pub fn begin_webhook(&mut self, job: &WebhookJob) -> Result<Option<WebhookSend>, Error> {
        let Ok(c) = self.load_webhook(&job.webhook) else {
            return Ok(None);
        };
        if !c.enabled {
            return Ok(None);
        }
        let Some(r) = self.sql.query_one(
            "SELECT payload, retry_until FROM webhook_deliveries WHERE id = ? AND status = 'pending' AND attempts = ?",
            &[text(&job.id), job.attempts.into()],
        )?
        else {
            return Ok(None);
        };
        let payload = r.text(0)?.to_string();
        let retry_until = Millis(r.int(1)?);
        self.sql.execute(
            "UPDATE webhook_deliveries SET sending = 1 WHERE id = ?",
            &[text(&job.id)],
        )?;
        let request = render_request(&c, &job.id, &payload, self.now())?;
        Ok(Some(WebhookSend {
            job: job.clone(),
            request,
            fingerprint: c.fingerprint(),
            retry_until,
            kind: c.kind,
        }))
    }

    /// Records what an endpoint answered.
    pub fn record_webhook(
        &mut self,
        send: &WebhookSend,
        result: &WebhookResult,
    ) -> Result<(), Error> {
        let now = self.now();
        let job = &send.job;
        let (retryable, message) = match result {
            WebhookResult::Status { code, .. } if (200..300).contains(code) => {
                (false, String::new())
            }
            WebhookResult::Status { code, .. } if *code == 408 || *code == 429 || *code >= 500 => {
                (true, format!("HTTP {code}"))
            }
            WebhookResult::Status { code, .. } => (false, format!("HTTP {code}")),
            WebhookResult::Redirect => (false, "redirected; redirects are not followed".into()),
            WebhookResult::Transport(why) => (true, why.clone()),
        };
        let delivered = !retryable && message.is_empty();
        let mut status = if delivered {
            "delivered"
        } else if retryable {
            "pending"
        } else {
            "failed"
        };
        let mut error = message.clone();
        let mut next = None;
        if retryable {
            let asked = requested_retry(result, send.kind, now, send.retry_until);
            let mut at = now
                .plus(jittered(job.id.as_str(), job.attempts))
                .max(asked.unwrap_or_default());
            // The endpoint as a whole backs off too, unless the owner changed
            // it since this request went out.
            let failures = self
                .sql
                .query_one(
                    "SELECT failures FROM webhooks WHERE name = ?",
                    &[text(&job.webhook)],
                )?
                .map(|r| r.count(0))
                .transpose()?
                .unwrap_or(0);
            let pause = now
                .plus(jittered(job.webhook.as_str(), failures))
                .max(asked.unwrap_or_default())
                .min(send.retry_until);
            if self
                .load_webhook(&job.webhook)
                .is_ok_and(|c| c.fingerprint() == send.fingerprint)
            {
                self.sql.execute(
                    "UPDATE webhooks SET retry_after = max(retry_after, ?), failures = failures + 1 WHERE name = ?",
                    &[pause.0.into(), text(&job.webhook)],
                )?;
            }
            at = at.max(pause);
            if at >= send.retry_until {
                status = "failed";
                error = format!("retry window expired after {message}");
            } else {
                next = Some(at);
            }
        }
        let terminal = status != "pending";
        let changed = self.sql.execute(
            "UPDATE webhook_deliveries SET status = ?1, attempts = attempts + 1, last_error = ?2, \
             next_attempt_at = coalesce(?3, next_attempt_at), finished_at = ?4, sending = 0 \
             WHERE id = ?5 AND status = 'pending' AND attempts = ?6",
            &[
                status.into(),
                error.into(),
                next.map(|t| t.0).into(),
                terminal.then_some(now.0).into(),
                text(&job.id),
                job.attempts.into(),
            ],
        )?;
        if changed > 0 && delivered {
            self.sql.execute(
                "UPDATE webhooks SET failures = 0 WHERE name = ?",
                &[text(&job.webhook)],
            )?;
        }
        Ok(())
    }

    /// Fails notifications whose window has passed and prunes old history.
    /// One left marked in flight by a crash expires too, or it would stay
    /// due forever; a late answer to it then changes nothing.
    pub fn maintain_webhooks(&mut self) -> Result<(), Error> {
        let now = self.now();
        self.sql.execute(
            "UPDATE webhook_deliveries SET status = 'failed', last_error = 'retry window expired', finished_at = ?1 \
             WHERE status = 'pending' AND retry_until <= ?1",
            &[now.0.into()],
        )?;
        self.sql.execute(
            "DELETE FROM webhook_deliveries WHERE status != 'pending' AND finished_at < ?",
            &[now.plus(-HISTORY).0.into()],
        )?;
        Ok(())
    }

    /// When the next notification is due, if any is pending.
    pub fn next_webhook_due(&self) -> Result<Option<Millis>, Error> {
        Ok(self
            .sql
            .query_one(
                "SELECT min(max(d.next_attempt_at, w.retry_after)) FROM webhook_deliveries d \
                 JOIN webhooks w ON w.name = d.webhook WHERE d.status = 'pending' AND w.enabled = 1",
                &[],
            )?
            .and_then(|r| r.opt_int(0).ok().flatten())
            .map(Millis))
    }
}

/// The request for a notification, as its provider wants it.
fn render_request(
    c: &Config,
    id: &Id,
    payload: &str,
    now: Millis,
) -> Result<WebhookRequest, Error> {
    let message = render(c.kind, payload)?;
    let mut url = c.url.clone();
    if c.kind == WebhookType::Discord {
        url = set_query(&url, "wait", "true");
    }
    let mut headers: Vec<(String, String)> = message.headers;
    // The owner's headers come after the formatter's, so a configured header
    // such as ntfy's Title replaces the generated one.
    for (n, v) in &c.headers {
        headers.retain(|(h, _)| !h.eq_ignore_ascii_case(n));
        headers.push((n.clone(), v.clone()));
    }
    headers.push(("Content-Type".into(), message.content_type.into()));
    headers.push(("User-Agent".into(), "cpd".into()));
    if c.kind == WebhookType::Generic {
        let ts = now.0.div_euclid(Millis::SECOND).to_string();
        headers.push(("Webhook-Id".into(), id.to_string()));
        headers.push(("Idempotency-Key".into(), id.to_string()));
        headers.push(("Webhook-Timestamp".into(), ts.clone()));
        if !c.secret.is_empty() {
            headers.push((
                "Webhook-Signature".into(),
                signature(&c.secret, id.as_str(), &ts, &message.body)?,
            ));
        }
    }
    Ok(WebhookRequest {
        url,
        headers,
        body: message.body,
    })
}

/// Sets a query parameter, keeping every other pair of the owner's query
/// exactly as written, since it can carry credentials.
fn set_query(url: &str, key: &str, value: &str) -> String {
    let (base, query) = url.split_once('?').unwrap_or((url, ""));
    let mut pairs: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty() && p.split('=').next() != Some(key))
        .collect();
    let set = format!("{key}={value}");
    pairs.push(&set);
    format!("{base}?{}", pairs.join("&"))
}

/// The Standard Webhooks signature: `v1,` and the base64 HMAC-SHA256 of
/// `id.timestamp.body` under the decoded key.
pub fn signature(secret: &str, id: &str, timestamp: &str, body: &[u8]) -> Result<String, Error> {
    let key = STANDARD
        .decode(secret)
        .ok()
        .filter(|k| k.len() == 32)
        .ok_or_else(|| Error::new(ErrorKind::Internal, "invalid webhook signing key"))?;
    let mut mac = <Hmac<Sha256>>::new_from_slice(&key)
        .map_err(|_| Error::new(ErrorKind::Internal, "invalid webhook signing key"))?;
    mac.update(format!("{id}.{timestamp}.").as_bytes());
    mac.update(body);
    Ok(format!(
        "v1,{}",
        STANDARD.encode(mac.finalize().into_bytes())
    ))
}

/// A provider's message.
pub struct Message {
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub headers: Vec<(String, String)>,
}

fn label(kind: WebhookEventType) -> &'static str {
    match kind {
        WebhookEventType::ThreadOpen => "New thread",
        WebhookEventType::ThreadReply => "New reply",
        WebhookEventType::ThreadAck => "Thread acknowledged",
        WebhookEventType::ThreadNeedsInput => "Input requested",
        WebhookEventType::ThreadResolve => "Result ready",
        WebhookEventType::ThreadDecline => "Thread declined",
        WebhookEventType::ThreadClose => "Thread closed",
        WebhookEventType::ThreadReopen => "Thread reopened",
        WebhookEventType::ThreadWithdraw => "Thread withdrawn",
        WebhookEventType::ThreadOverruled => "Your move did not take effect",
        WebhookEventType::PeeringRequested => "Peering request",
    }
}

fn color(kind: WebhookEventType) -> u32 {
    match kind {
        WebhookEventType::ThreadClose | WebhookEventType::ThreadResolve => 0x16_a3_4a,
        WebhookEventType::ThreadNeedsInput
        | WebhookEventType::PeeringRequested
        | WebhookEventType::ThreadOverruled => 0xd9_77_06,
        WebhookEventType::ThreadDecline | WebhookEventType::ThreadWithdraw => 0x64_74_8b,
        _ => 0x63_66_f1,
    }
}

/// Peer names are bounded, but neutralising mention and markup delimiters
/// also keeps an event from pinging a channel.
fn neutralize(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '@' => vec!['@', '\u{200b}'],
            '<' => vec!['‹'],
            '>' => vec!['›'],
            '&' => vec!['＆'],
            '\r' | '\n' => vec![' '],
            c => vec![c],
        })
        .collect()
}

/// The text chat destinations show: metadata and the command to read the
/// thread, never its title or bodies.
fn summary(p: &WebhookPayload) -> (String, String) {
    let title = format!("clanker-proxy: {}", label(p.kind));
    let origin = match p.origin {
        WebhookEventOrigin::Incoming => "incoming",
        WebhookEventOrigin::Outgoing => "outgoing",
    };
    let mut body = format!(
        "Event: {} ({origin})\nPeer: {}",
        p.kind,
        neutralize(p.peer.as_str())
    );
    if p.kind == WebhookEventType::PeeringRequested {
        let _ = write!(
            body,
            " (unverified)\nRequest: {}\nRead: cpctl requests",
            p.subject
        );
    } else {
        let _ = write!(body, "\nState: {}", p.state.map_or("", State::as_str));
        if p.my_turn == Some(true) {
            body.push_str(" — your turn");
        }
        let _ = write!(
            body,
            "\nThread: {}\nRead: cpctl show {}",
            p.subject, p.subject
        );
    }
    let _ = write!(body, "\nNotification: {}", p.id);
    (title, body)
}

/// Formats a stored payload for a destination. Generic receivers get the
/// payload bytes as stored, which is what the signature covers.
pub fn render(kind: WebhookType, payload: &str) -> Result<Message, Error> {
    if kind == WebhookType::Generic {
        return Ok(Message {
            body: payload.as_bytes().to_vec(),
            content_type: "application/json",
            headers: Vec::new(),
        });
    }
    let p: WebhookPayload = serde_json::from_str(payload)
        .map_err(|_| Error::new(ErrorKind::Internal, "cannot format notification"))?;
    let (title, body) = summary(&p);
    if kind == WebhookType::Ntfy {
        return Ok(Message {
            body: body.into_bytes(),
            content_type: "text/plain; charset=utf-8",
            headers: vec![("Title".into(), title)],
        });
    }
    let text = format!("{title}\n{body}");
    let v = match kind {
        WebhookType::Discord => serde_json::json!({
            "username": "clanker-proxy",
            "allowed_mentions": {"parse": []},
            "embeds": [{"title": title, "description": body, "color": color(p.kind), "timestamp": p.at.to_string(), "footer": {"text": "clanker-proxy"}}],
        }),
        WebhookType::Slack => serde_json::json!({
            "text": text,
            "blocks": [{"type": "section", "text": {"type": "plain_text", "text": text, "emoji": false}}],
            "unfurl_links": false,
            "unfurl_media": false,
        }),
        WebhookType::Teams => serde_json::json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "contentUrl": null,
                "content": {
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "type": "AdaptiveCard",
                    "version": "1.2",
                    "body": [
                        {"type": "TextBlock", "text": title, "weight": "Bolder", "wrap": true},
                        {"type": "TextBlock", "text": body, "wrap": true},
                    ],
                },
            }],
        }),
        WebhookType::Gotify => serde_json::json!({"title": title, "message": body, "priority": 5}),
        WebhookType::Apprise => {
            serde_json::json!({"title": title, "body": body, "type": "info", "format": "text"})
        }
        WebhookType::GoogleChat
        | WebhookType::Mattermost
        | WebhookType::Rocketchat
        | WebhookType::Generic
        | WebhookType::Ntfy => {
            serde_json::json!({"text": text})
        }
    };
    Ok(Message {
        body: serde_json::to_vec(&v)
            .map_err(|_| Error::new(ErrorKind::Internal, "cannot format notification"))?,
        content_type: "application/json",
        headers: Vec::new(),
    })
}
