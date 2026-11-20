//! `cpctl webhook …`: the owner's notification destinations.

use std::fmt::Write as _;
use std::io::Read;

use cp_api::{
    DeliveryStatus, Webhook, WebhookCreate, WebhookDeliveryList, WebhookEventType, WebhookEvents,
    WebhookHeader, WebhookHeaderName, WebhookHeaderValue, WebhookList, WebhookOrigin,
    WebhookSecret, WebhookType, WebhookUpdate, WebhookUrl,
};
use cp_thread::Name;

use crate::cli::Parsed;
use crate::client::segment;
use crate::output::{Step, next, step, table};
use crate::{App, Failure, name, peers_flag};

/// Most deliveries one page holds.
const PAGE: u32 = 100;

/// A wire enum from its name, as the API spells it.
fn named<T: serde::de::DeserializeOwned>(raw: &str, what: &str, hint: &str) -> Result<T, Failure> {
    serde_json::from_value(serde_json::Value::String(raw.trim().to_string()))
        .map_err(|_| Failure::usage(format!("unknown {what} {raw:?}")).hint(hint))
}

fn events(raw: &str) -> Result<WebhookEvents, Failure> {
    let list: Vec<&str> = raw.split(',').map(str::trim).collect();
    serde_json::from_value(serde_json::json!(list)).map_err(|e| {
        Failure::usage(format!("--events: {e}")).hint("cpctl webhook events lists the types")
    })
}

fn origin(raw: &str) -> Result<WebhookOrigin, Failure> {
    named(raw, "origin", "use incoming, outgoing or both")
}

/// Refuses reading several files from stdin: the first read would leave
/// the others empty.
fn one_stdin(p: &Parsed) -> Result<(), Failure> {
    let stdin = ["url-file", "secret-file", "headers-file"]
        .iter()
        .filter(|f| p.value(f) == Some("-"))
        .count();
    if stdin > 1 {
        return Err(Failure::usage(
            "only one of --url-file, --secret-file and --headers-file can read stdin (-)",
        ));
    }
    Ok(())
}

/// A small private file, or stdin for `-`.
fn read(path: &str, label: &str, limit: usize, stdin: &mut dyn Read) -> Result<String, Failure> {
    let mut buf = Vec::new();
    let max = u64::try_from(limit + 1).unwrap_or(u64::MAX);
    let read = if path == "-" {
        stdin.take(max).read_to_end(&mut buf)
    } else {
        let f = std::fs::File::open(path).map_err(|e| {
            let why = match e.kind() {
                std::io::ErrorKind::NotFound => "does not exist",
                std::io::ErrorKind::PermissionDenied => "cannot be read: permission denied",
                _ => "cannot be opened",
            };
            Failure::usage(format!("{label} file {path} {why}"))
        })?;
        f.take(max).read_to_end(&mut buf)
    };
    if read.is_err() || buf.len() > limit {
        return Err(Failure::usage(format!(
            "cannot read the {label} file (at most {limit} bytes)"
        )));
    }
    String::from_utf8(buf).map_err(|_| Failure::usage(format!("the {label} file is not UTF-8")))
}

fn url(arg: Option<&str>, file: Option<&str>, stdin: &mut dyn Read) -> Result<WebhookUrl, Failure> {
    let raw = match (arg, file) {
        (Some(u), None) => u.to_string(),
        (None, Some(f)) => read(f, "URL", cp_api::MAX_WEBHOOK_URL + 2, stdin)?
            .trim()
            .to_string(),
        _ => {
            return Err(Failure::usage(
                "give the destination URL as an argument or with --url-file, not both",
            ));
        }
    };
    if raw.is_empty() {
        return Err(Failure::usage("the destination URL is empty"));
    }
    // The error never repeats the URL: it can hold credentials.
    WebhookUrl::new(raw).map_err(|e| Failure::usage(e.to_string()))
}

fn secret(path: &str, stdin: &mut dyn Read) -> Result<WebhookSecret, Failure> {
    let raw = read(path, "signing key", 128, stdin)?.trim().to_string();
    if raw.is_empty() {
        return Err(Failure::usage(
            "the signing-key file is empty; use --signing=false to remove signing",
        ));
    }
    WebhookSecret::new(raw).map_err(|_| {
        Failure::usage("the signing-key file must hold the base64 encoding of 32 bytes")
            .hint("make one with: umask 077; openssl rand -base64 32 > webhook.key")
    })
}

fn headers(path: &str, stdin: &mut dyn Read) -> Result<Vec<WebhookHeader>, Failure> {
    let raw = read(path, "headers", 128 * 1024, stdin)?;
    let bad = || {
        Failure::usage("the headers file must hold a JSON object of header names and string values")
    };
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).map_err(|_| bad())?;
    map.into_iter()
        .map(|(n, v)| {
            let v = v.as_str().ok_or_else(bad)?.to_string();
            // Neither message repeats the value: it can be a credential.
            Ok(WebhookHeader {
                name: WebhookHeaderName::try_from(n.clone())
                    .map_err(|e| Failure::usage(format!("header {n:?}: {e}")))?,
                value: WebhookHeaderValue::new(v)
                    .map_err(|e| Failure::usage(format!("header {n}: {e}")))?,
            })
        })
        .collect()
}

fn for_peers(w: &Webhook) -> String {
    if w.peers.is_empty() {
        "every peer".into()
    } else {
        w.peers
            .as_slice()
            .iter()
            .map(Name::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn joined_events(e: &WebhookEvents) -> String {
    match e {
        WebhookEvents::All => "*".into(),
        WebhookEvents::Only(types) => types
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(","),
    }
}

fn origin_str(o: WebhookOrigin) -> &'static str {
    match o {
        WebhookOrigin::Incoming => "incoming",
        WebhookOrigin::Outgoing => "outgoing",
        WebhookOrigin::Both => "both",
    }
}

fn status_str(s: DeliveryStatus) -> &'static str {
    match s {
        DeliveryStatus::Pending => "pending",
        DeliveryStatus::Delivered => "delivered",
        DeliveryStatus::Failed => "failed",
    }
}

fn describe(out: &mut String, h: &Webhook) {
    let _ = writeln!(
        out,
        "{} → {} ({}, enabled: {}, origin: {}, signing: {})\nevents: {}\npeers: {}",
        h.name,
        h.destination,
        h.kind,
        h.enabled,
        origin_str(h.origin),
        h.signing,
        joined_events(&h.events),
        for_peers(h),
    );
    if let Some(agent) = &h.agent {
        let _ = writeln!(
            out,
            "registered by agent token {agent}; revoking the token deletes it"
        );
    }
    if !h.header_names.is_empty() {
        let names: Vec<&str> = h
            .header_names
            .iter()
            .map(WebhookHeaderName::as_str)
            .collect();
        let _ = writeln!(out, "headers: {} (values hidden)", names.join(", "));
    }
    let mut steps = vec![step(
        format!("cpctl webhook deliveries {}", h.name),
        "check delivery status",
    )];
    if !h.enabled {
        out.push_str("paused: no new notifications are queued, and queued ones wait\n");
        steps.push(step(
            format!("cpctl webhook set {} --enabled=true", h.name),
            "enable it again",
        ));
    } else if let Some(until) = h.paused_until {
        let _ = writeln!(out, "backing off until {until} after failed attempts");
        steps.push(step(
            format!("cpctl webhook set {} --enabled=true", h.name),
            "retry now, once the receiver is fixed",
        ));
    }
    next(out, &steps);
}

impl App {
    pub(crate) async fn webhook(
        &mut self,
        command: &str,
        p: &Parsed,
        stdin: &mut dyn Read,
    ) -> Result<(), Failure> {
        let arg = |i: usize| p.args.get(i).cloned().unwrap_or_default();
        match command {
            "webhook add" => self.webhook_add(p, stdin).await,
            "webhook ls" => self.webhook_ls().await,
            "webhook show" => {
                let n = name(&arg(0))?;
                let (h, raw): (Webhook, _) =
                    self.client.get(&format!("/api/v1/webhooks/{n}")).await?;
                self.print(&raw, |w| describe(w, &h));
                Ok(())
            }
            "webhook set" => self.webhook_set(p, stdin).await,
            "webhook rm" => {
                let n = name(&arg(0))?;
                self.client.delete(&format!("/api/v1/webhooks/{n}")).await?;
                self.done(
                    &format!("deleted webhook {n} and its delivery history"),
                    &[step("cpctl webhook ls", "see the remaining webhooks")],
                );
                Ok(())
            }
            "webhook deliveries" => self.webhook_deliveries(p).await,
            "webhook retry" => {
                let n = name(&arg(0))?;
                let id = arg(1);
                self.client
                    .post_empty(&format!(
                        "/api/v1/webhooks/{n}/deliveries/{}/retry",
                        segment(&id)?
                    ))
                    .await?;
                self.done(
                    &format!("delivery {id} queued"),
                    &[step(
                        format!("cpctl webhook deliveries {n}"),
                        "check the next attempt",
                    )],
                );
                Ok(())
            }
            "webhook types" => {
                let providers: Vec<_> = WebhookType::ALL.iter().map(|t| t.provider()).collect();
                let raw = serde_json::json!({ "types": providers }).to_string();
                self.print(&raw, |w| {
                    for p in &providers {
                        let _ = writeln!(
                            w,
                            "{}: {}\n  auth: {}\n  URL: {}",
                            p.kind, p.description, p.auth, p.example_url
                        );
                    }
                    next(
                        w,
                        &[
                            step("cpctl help webhook add", "configure a destination"),
                            step("cpctl webhook events", "choose event subscriptions"),
                        ],
                    );
                });
                Ok(())
            }
            "webhook events" => {
                let names: Vec<&str> = WebhookEventType::ALL.iter().map(|t| t.as_str()).collect();
                let raw = serde_json::json!({ "events": names }).to_string();
                self.print(&raw, |w| {
                    for n in &names {
                        let _ = writeln!(w, "{n}");
                    }
                    next(w, &[step("cpctl help webhook add", "subscribe a receiver")]);
                });
                Ok(())
            }
            _ => Err(Failure::usage(format!("no command {command:?}"))),
        }
    }

    async fn webhook_add(&mut self, p: &Parsed, stdin: &mut dyn Read) -> Result<(), Failure> {
        one_stdin(p)?;
        let n = name(p.args.first().map_or("", String::as_str))?;
        let kind: WebhookType = named(
            p.value("type").unwrap_or("generic"),
            "webhook type",
            "cpctl webhook types lists them",
        )?;
        let input = WebhookCreate {
            name: n,
            kind,
            url: url(
                p.args.get(1).map(String::as_str),
                p.value("url-file"),
                stdin,
            )?,
            events: events(p.value("events").unwrap_or("*"))?,
            origin: origin(p.value("origin").unwrap_or("incoming"))?,
            enabled: true,
            peers: peers_flag(p)?,
            secret: p
                .value("secret-file")
                .map(|f| secret(f, stdin))
                .transpose()?,
            headers: p
                .value("headers-file")
                .map(|f| headers(f, stdin))
                .transpose()?
                .unwrap_or_default(),
        };
        let (h, raw): (Webhook, _) = self.client.post("/api/v1/webhooks", &input).await?;
        self.print(&raw, |w| describe(w, &h));
        Ok(())
    }

    async fn webhook_set(&mut self, p: &Parsed, stdin: &mut dyn Read) -> Result<(), Failure> {
        one_stdin(p)?;
        let n = name(p.args.first().map_or("", String::as_str))?;
        let mut u = WebhookUpdate::default();
        let mut any = false;
        if let Some(f) = p.value("url-file") {
            u.url = Some(url(None, Some(f), stdin)?);
            any = true;
        }
        if let Some(e) = p.value("events") {
            u.events = Some(events(e)?);
            any = true;
        }
        if let Some(o) = p.value("origin") {
            u.origin = Some(origin(o)?);
            any = true;
        }
        if p.value("peers").is_some() {
            u.peers = Some(peers_flag(p)?);
            any = true;
        }
        if let Some(e) = p.value("enabled") {
            u.enabled = Some(match e {
                "true" => true,
                "false" => false,
                _ => return Err(Failure::usage("--enabled must be true or false")),
            });
            any = true;
        }
        match (p.value("signing"), p.value("secret-file")) {
            (Some(_), Some(_)) | (Some("true"), None) => {
                return Err(Failure::usage(
                    "use --signing=false to remove signing, or --secret-file to enable it",
                ));
            }
            (Some("false"), None) => {
                u.secret = WebhookSecret::new(String::new()).ok();
                any = true;
            }
            (Some(_), None) => return Err(Failure::usage("--signing takes only false")),
            (None, Some(f)) => {
                u.secret = Some(secret(f, stdin)?);
                any = true;
            }
            (None, None) => {}
        }
        if let Some(f) = p.value("headers-file") {
            u.headers = Some(headers(f, stdin)?);
            any = true;
        }
        if !any {
            return Err(Failure::usage("give at least one setting to change"));
        }
        let (h, raw): (Webhook, _) = self
            .client
            .patch(&format!("/api/v1/webhooks/{n}"), &u)
            .await?;
        self.print(&raw, |w| describe(w, &h));
        Ok(())
    }

    async fn webhook_ls(&mut self) -> Result<(), Failure> {
        let (l, raw): (WebhookList, _) = self.client.get("/api/v1/webhooks").await?;
        self.print(&raw, |w| {
            if l.webhooks.is_empty() {
                w.push_str("no webhooks\n");
            } else {
                let mut rows = vec![vec![
                    "NAME".into(),
                    "TYPE".into(),
                    "ENABLED".into(),
                    "ORIGIN".into(),
                    "EVENTS".into(),
                    "PEERS".into(),
                    "DESTINATION".into(),
                ]];
                for h in &l.webhooks {
                    rows.push(vec![
                        h.agent.as_ref().map_or_else(
                            || h.name.to_string(),
                            |a| format!("{} (agent {a})", h.name),
                        ),
                        h.kind.to_string(),
                        h.enabled.to_string(),
                        origin_str(h.origin).into(),
                        joined_events(&h.events),
                        for_peers(h),
                        h.destination.clone(),
                    ]);
                }
                table(w, &rows);
            }
            next(
                w,
                &[
                    step("cpctl help webhook add", "configure a receiver"),
                    step("cpctl webhook events", "see the event types"),
                ],
            );
        });
        Ok(())
    }

    async fn webhook_deliveries(&mut self, p: &Parsed) -> Result<(), Failure> {
        let n = name(p.args.first().map_or("", String::as_str))?;
        let limit: u32 = match p.value("limit") {
            None => PAGE,
            Some(v) => v
                .parse()
                .ok()
                .filter(|l| (1..=PAGE).contains(l))
                .ok_or_else(|| Failure::usage("--limit must be between 1 and 100"))?,
        };
        let status: Option<DeliveryStatus> = p
            .value("status")
            .map(|s| named(s, "status", "use pending, delivered or failed"))
            .transpose()?;
        let mut query = format!("?limit={limit}");
        if let Some(c) = p.value("cursor") {
            let _ = write!(query, "&cursor={}", segment(c)?);
        }
        if let Some(s) = status {
            let _ = write!(query, "&status={}", status_str(s));
        }
        let (l, raw): (WebhookDeliveryList, _) = self
            .client
            .get(&format!("/api/v1/webhooks/{n}/deliveries{query}"))
            .await?;
        self.print(&raw, |w| {
            if l.deliveries.is_empty() {
                w.push_str("no deliveries\n");
            } else {
                let mut rows = vec![vec![
                    "ID".into(),
                    "CREATED".into(),
                    "EVENT".into(),
                    "SUBJECT".into(),
                    "STATUS".into(),
                    "ATTEMPTS".into(),
                    "DETAIL".into(),
                ]];
                for d in &l.deliveries {
                    let mut detail = d
                        .next_attempt_at
                        .map_or_else(String::new, |at| format!("next attempt {at}"));
                    if !d.last_error.is_empty() {
                        if !detail.is_empty() {
                            detail.push_str("  ");
                        }
                        detail.push_str(&crate::output::printable(&d.last_error, false));
                    }
                    rows.push(vec![
                        d.id.to_string(),
                        d.created_at.to_string(),
                        d.event.to_string(),
                        d.subject.clone(),
                        status_str(d.status).into(),
                        d.attempts.to_string(),
                        detail,
                    ]);
                }
                table(w, &rows);
            }
            let mut steps: Vec<Step> = Vec::new();
            if let Some(c) = &l.next_cursor {
                let mut cmd = format!(
                    "cpctl webhook deliveries {n} --cursor {} -n {limit}",
                    c.as_str()
                );
                if let Some(s) = status {
                    let _ = write!(cmd, " --status {}", status_str(s));
                }
                steps.push(step(cmd, "read older deliveries"));
            }
            if l.deliveries
                .iter()
                .any(|d| d.status == DeliveryStatus::Failed)
            {
                steps.push(step(
                    format!("cpctl webhook retry {n} <delivery-id>"),
                    "retry a failed delivery once its receiver is fixed",
                ));
            } else if status.is_none() {
                steps.push(step(
                    format!("cpctl webhook deliveries {n} --status failed"),
                    "list only failures",
                ));
            }
            steps.push(step(
                format!("cpctl webhook show {n}"),
                "check whether the endpoint is paused",
            ));
            next(w, &steps);
        });
        Ok(())
    }
}
