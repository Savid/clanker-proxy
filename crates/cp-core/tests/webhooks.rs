//! Webhooks: settings, queueing, formats, signing and retries.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use common::{Clock, Daemon, kind, name, peer, settle};
use cp_api::{
    ErrorKind, WebhookCreate, WebhookEvents, WebhookHeader, WebhookOrigin, WebhookSecret,
    WebhookType, WebhookUpdate, WebhookUrl,
};
use cp_core::{Millis, WebhookResult, render, signature};
use cp_thread::Action;

fn create(n: &str, kind: WebhookType, url: &str) -> WebhookCreate {
    WebhookCreate {
        name: name(n),
        kind,
        url: WebhookUrl::new(url.into()).unwrap(),
        events: WebhookEvents::All,
        origin: WebhookOrigin::Both,
        enabled: true,
        peers: cp_api::Peers::default(),
        secret: None,
        headers: Vec::new(),
    }
}

fn key() -> String {
    "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE=".into() // 32 × 'a'
}

#[test]
fn signatures_match_standard_webhooks() {
    let got = signature(&key(), "event-id", "1700000000", br#"{"hello":"world"}"#).unwrap();
    assert_eq!(got, "v1,sTL4E9ah/5ZFL+CR/fTl252h2LiBHuo+k+zA5Nd1A40=");
}

#[test]
fn every_provider_gets_metadata_only() {
    let raw = r#"{"id":"00000000-0000-4000-8000-000000000001","type":"thread.open","origin":"incoming","at":"2026-10-07T10:00:00Z","subject":"00000000-0000-4000-8000-000000000002","peer":"bob","state":"open","myTurn":true}"#;
    for kind in WebhookType::ALL {
        let m = render(kind, raw).unwrap();
        let body = String::from_utf8(m.body).unwrap();
        if kind == WebhookType::Generic {
            assert_eq!(
                body, raw,
                "generic sends the stored bytes, which the signature covers"
            );
            continue;
        }
        assert!(
            body.contains("cpctl show 00000000-0000-4000-8000-000000000002")
                && body.contains("your turn"),
            "{kind}: {body}"
        );
        if kind == WebhookType::Ntfy {
            assert_eq!(m.content_type, "text/plain; charset=utf-8");
            assert_eq!(m.headers[0].0, "Title");
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        match kind {
            WebhookType::Discord => {
                assert_eq!(v["allowed_mentions"]["parse"], serde_json::json!([]));
            }
            WebhookType::Slack => assert_eq!(v["unfurl_links"], false),
            WebhookType::Teams => assert_eq!(v["type"], "message"),
            WebhookType::Gotify => assert_eq!(v["priority"], 5),
            WebhookType::Apprise => assert_eq!(v["format"], "text"),
            _ => assert!(v["text"].is_string(), "{kind}"),
        }
    }
}

#[test]
fn settings_are_checked() {
    let clock = Clock::new();
    let mut a = Daemon::new("alice", &clock, 1);
    for (url, ok) in [
        ("https://hooks.example.com/x?token=1", true),
        ("http://127.0.0.1:9000/hook", true),
        ("http://localhost:9000/hook", false),
        ("http://hooks.example.com/x", false),
        ("https://user:pw@hooks.example.com/x", false),
        ("https://hooks.example.com/x#frag", false),
    ] {
        let r = a.call(|c| c.create_webhook(create("w", WebhookType::Generic, url)));
        assert_eq!(r.is_ok(), ok, "{url}");
        if ok {
            a.call(|c| c.delete_webhook(&name("w"))).unwrap();
        }
    }
    let mut signed = create("chat", WebhookType::Slack, "https://hooks.slack.com/x");
    signed.secret = Some(WebhookSecret::new(key()).unwrap());
    assert_eq!(
        kind(&a.call(|c| c.create_webhook(signed))),
        ErrorKind::Invalid,
        "signing is for generic only"
    );
    let mut reserved = create("h", WebhookType::Generic, "https://x.test/");
    reserved.headers = vec![
        serde_json::from_str::<WebhookHeader>(r#"{"name":"Webhook-Id","value":"x"}"#).unwrap(),
    ];
    assert_eq!(
        kind(&a.call(|c| c.create_webhook(reserved))),
        ErrorKind::Invalid
    );
    let mut scoped = create("p", WebhookType::Generic, "https://x.test/");
    scoped.peers = cp_api::Peers::try_from(vec![name("bob")]).unwrap();
    assert_eq!(
        kind(&a.call(|c| c.create_webhook(scoped))),
        ErrorKind::Invalid,
        "unknown peer"
    );

    a.call(|c| c.create_webhook(create("w", WebhookType::Generic, "https://x.test/")))
        .unwrap();
    assert_eq!(
        kind(&a.call(|c| c.create_webhook(create("w", WebhookType::Generic, "https://x.test/")))),
        ErrorKind::Conflict
    );
    let view = a.call(|c| c.webhook(&name("w"))).unwrap();
    assert_eq!(view.destination, "https://x.test");
}

/// Runs every due notification through `answer`, returning what was sent.
fn drain(
    d: &mut Daemon,
    mut answer: impl FnMut(&cp_core::WebhookRequest) -> WebhookResult,
) -> Vec<cp_core::WebhookRequest> {
    let mut requests = Vec::new();
    for job in d.call(|c| c.webhook_due(10)).unwrap() {
        let Some(attempt) = d.call(|c| c.begin_webhook(&job)).unwrap() else {
            continue;
        };
        let result = answer(&attempt.request);
        d.call(|c| c.record_webhook(&attempt, &result)).unwrap();
        requests.push(attempt.request);
    }
    requests
}

fn ok() -> WebhookResult {
    WebhookResult::Status {
        code: 204,
        retry_after: None,
        body: Vec::new(),
    }
}

#[test]
fn thread_events_are_queued_filtered_and_signed() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    let mut hook = create("runner", WebhookType::Generic, "https://runner.test/hook");
    hook.origin = WebhookOrigin::Incoming;
    hook.secret = Some(WebhookSecret::new(key()).unwrap());
    bob.call(|c| c.create_webhook(hook)).unwrap();
    alice
        .call(|c| {
            c.create_webhook(create(
                "all",
                WebhookType::Slack,
                "https://hooks.slack.com/x",
            ))
        })
        .unwrap();

    let id = alice.send("bob", "secret plans").id.to_string();
    settle(&mut alice, &mut bob);
    let sent = drain(&mut bob, |_| ok());
    assert_eq!(sent.len(), 1);
    let r = &sent[0];
    let header = |n: &str| {
        r.headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    let ts = header("Webhook-Timestamp");
    assert_eq!(
        header("Webhook-Signature"),
        signature(&key(), &header("Webhook-Id"), &ts, &r.body).unwrap()
    );
    assert_eq!(header("Webhook-Id"), header("Idempotency-Key"));
    let p: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(
        (
            p["type"].as_str(),
            p["origin"].as_str(),
            p["subject"].as_str(),
            p["myTurn"].as_bool()
        ),
        (
            Some("thread.open"),
            Some("incoming"),
            Some(id.as_str()),
            Some(true)
        )
    );
    assert!(
        !String::from_utf8_lossy(&r.body).contains("secret plans"),
        "no titles in payloads"
    );

    // Bob's own move is outgoing, which this webhook does not want.
    bob.act(&id, Action::Ack, "").unwrap();
    assert!(drain(&mut bob, |_| ok()).is_empty());
    // Alice's chat webhook hears her own open, as text, with the URL's query kept.
    let chat = drain(&mut alice, |_| ok());
    assert_eq!(chat.len(), 1);
    assert!(String::from_utf8_lossy(&chat[0].body).contains("New thread"));
}

#[test]
fn pending_notifications_coalesce_and_peering_is_capped() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    alice
        .call(|c| c.create_webhook(create("w", WebhookType::Generic, "https://x.test/")))
        .unwrap();
    let id = alice.send("bob", "t").id.to_string();
    for i in 0..3 {
        alice
            .act(&id, Action::Comment, &format!("note {i}"))
            .unwrap();
    }
    let page = alice
        .call(|c| c.webhook_deliveries(&name("w"), None, None, None))
        .unwrap();
    let replies = page
        .deliveries
        .iter()
        .filter(|d| d.event == cp_api::WebhookEventType::ThreadReply)
        .count();
    assert_eq!(replies, 1, "unsent replies about the same thread coalesce");

    for n in 0..120u32 {
        clock.advance(Millis::MINUTE);
        let req = cp_api::PeeringRequest {
            name: name(&format!("p{n}")),
            url: cp_api::DaemonUrl::try_from(format!("https://p{n}.test")).unwrap(),
            key: cp_api::PublicKey::try_from(format!("{n:043}")).unwrap(),
            note: None,
        };
        alice.call(|c| c.request_received(&req)).unwrap();
    }
    let all = alice
        .call(|c| c.webhook_deliveries(&name("w"), None, Some(100), None))
        .unwrap();
    let peering = all
        .deliveries
        .iter()
        .filter(|d| d.event == cp_api::WebhookEventType::PeeringRequested)
        .count();
    assert!(
        peering <= 1,
        "a flood leaves one pending peering notice: {peering}"
    );
}

#[test]
fn failures_back_off_per_endpoint_and_expire() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    alice
        .call(|c| c.create_webhook(create("down", WebhookType::Generic, "https://down.test/")))
        .unwrap();
    alice
        .call(|c| c.create_webhook(create("up", WebhookType::Generic, "https://up.test/")))
        .unwrap();
    alice.send("bob", "t");

    let sent = drain(&mut alice, |r| {
        if r.url.contains("down") {
            WebhookResult::Status {
                code: 503,
                retry_after: Some("120".into()),
                body: Vec::new(),
            }
        } else {
            ok()
        }
    });
    assert_eq!(
        sent.len(),
        2,
        "one endpoint failing does not hold up the other"
    );
    let w = alice.call(|c| c.webhook(&name("down"))).unwrap();
    assert!(
        w.paused_until
            .is_some_and(|t| Millis::from_at(t) >= clock.now().plus(120 * Millis::SECOND)),
        "Retry-After pauses the endpoint: {w:?}"
    );
    assert!(drain(&mut alice, |_| ok()).is_empty(), "paused");

    // A change by the owner clears the endpoint's pause; the notification
    // keeps its own retry time.
    let w = alice
        .call(|c| {
            c.update_webhook(
                &name("down"),
                WebhookUpdate {
                    enabled: Some(true),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(w.paused_until, None);
    clock.advance(5 * Millis::MINUTE);
    assert_eq!(
        drain(&mut alice, |_| WebhookResult::Transport(
            "connection refused".into()
        ))
        .len(),
        1
    );

    // A week of failures ends the notification.
    for _ in 0..(8 * 24) {
        clock.advance(Millis::HOUR);
        alice.call(cp_core::Core::maintain_webhooks).unwrap();
        drain(&mut alice, |_| WebhookResult::Transport("timed out".into()));
    }
    let page = alice
        .call(|c| c.webhook_deliveries(&name("down"), None, None, None))
        .unwrap();
    let d = &page.deliveries[0];
    assert_eq!(d.status, cp_api::DeliveryStatus::Failed);
    assert!(
        d.last_error.contains("retry window expired"),
        "{}",
        d.last_error
    );

    // A failed delivery can be retried by hand.
    alice
        .call(|c| c.retry_webhook(&name("down"), &d.id))
        .unwrap();
    assert_eq!(drain(&mut alice, |_| ok()).len(), 1);
    assert_eq!(
        kind(&alice.call(|c| c.retry_webhook(&name("down"), &d.id))),
        ErrorKind::Conflict
    );
}

#[test]
fn final_statuses_fail_at_once() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    alice
        .call(|c| {
            c.create_webhook(create(
                "w",
                WebhookType::Discord,
                "https://discord.com/api/webhooks/1/x?thread_id=7",
            ))
        })
        .unwrap();
    alice.send("bob", "t");
    let sent = drain(&mut alice, |_| WebhookResult::Status {
        code: 404,
        retry_after: None,
        body: Vec::new(),
    });
    assert!(
        sent[0].url.ends_with("?thread_id=7&wait=true"),
        "provider query added, owner's kept: {}",
        sent[0].url
    );
    let page = alice
        .call(|c| c.webhook_deliveries(&name("w"), None, None, None))
        .unwrap();
    assert_eq!(
        (
            page.deliveries[0].status,
            page.deliveries[0].last_error.as_str()
        ),
        (cp_api::DeliveryStatus::Failed, "HTTP 404")
    );
}

#[test]
fn a_loser_of_a_race_hears_it() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    for d in [&mut alice, &mut bob] {
        let mut hook = create("w", WebhookType::Generic, "https://x.test/");
        hook.events = serde_json::from_str(r#"["thread.overruled"]"#).unwrap();
        d.call(|c| c.create_webhook(hook)).unwrap();
    }
    let id = alice.send("bob", "t").id.to_string();
    settle(&mut alice, &mut bob);
    alice.act(&id, Action::Withdraw, "never mind").unwrap();
    bob.act(&id, Action::Resolve, "done").unwrap();
    settle(&mut alice, &mut bob);
    let heard: Vec<_> = [&mut alice, &mut bob]
        .into_iter()
        .map(|d| drain(d, |_| ok()).len())
        .collect();
    assert_eq!(
        heard.iter().sum::<usize>(),
        1,
        "exactly the loser is told: {heard:?}"
    );
}

#[test]
fn a_send_lost_in_a_crash_still_expires() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    alice
        .call(|c| c.create_webhook(create("w", WebhookType::Generic, "https://x.test/")))
        .unwrap();
    alice.send("bob", "t");
    let job = alice.call(|c| c.webhook_due(10)).unwrap().remove(0);
    let lost = alice.call(|c| c.begin_webhook(&job)).unwrap().unwrap();
    clock.advance(8 * Millis::DAY);
    alice.call(cp_core::Core::maintain_webhooks).unwrap();
    assert_eq!(
        alice.call(|c| c.next_webhook_due()).unwrap(),
        None,
        "nothing stays due"
    );
    alice.call(|c| c.record_webhook(&lost, &ok())).unwrap();
    let page = alice
        .call(|c| c.webhook_deliveries(&name("w"), None, None, None))
        .unwrap();
    assert_eq!(
        page.deliveries[0].status,
        cp_api::DeliveryStatus::Failed,
        "a late answer changes nothing"
    );
}

#[test]
fn an_agent_token_registers_its_own_webhook() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    let scope = common::scope(&["bob"]);
    alice
        .call(|c| {
            c.create_agent_token(cp_api::AgentTokenInput {
                name: name("inbox"),
                peers: scope.clone(),
                expires_at: None,
            })
        })
        .unwrap();
    let own = |u: &str| cp_api::OwnWebhook {
        url: WebhookUrl::new(u.into()).unwrap(),
        secret: WebhookSecret::new(key()).unwrap(),
    };

    for private in [
        "http://127.0.0.1:9000/hook",
        "https://10.0.0.1/hook",
        "https://localhost/hook",
        "https://runner.internal/hook",
        "https://[::1]/hook",
    ] {
        assert_eq!(
            kind(&alice.call(|c| c.set_agent_webhook(&name("inbox"), &scope, &own(private)))),
            ErrorKind::Invalid,
            "{private}"
        );
    }
    let w = alice
        .call(|c| c.set_agent_webhook(&name("inbox"), &scope, &own("https://amp.test/a")))
        .unwrap();
    assert_eq!(w.agent, Some(name("inbox")));
    assert_eq!(
        (w.origin, w.events, w.peers),
        (WebhookOrigin::Incoming, WebhookEvents::All, scope.clone())
    );
    // Registering again replaces it.
    let w = alice
        .call(|c| c.set_agent_webhook(&name("inbox"), &scope, &own("https://amp.test/b")))
        .unwrap();
    assert_eq!(w.destination, "https://amp.test");
    assert_eq!(alice.call(|c| c.webhooks()).unwrap().len(), 1);

    // It hears the peer's thread.
    bob.send("alice", "for the agent");
    settle(&mut alice, &mut bob);
    let sent = drain(&mut alice, |_| ok());
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, "https://amp.test/b");

    // An owner's webhook keeps its name.
    alice
        .call(|c| c.create_webhook(create("mine", WebhookType::Generic, "https://x.test/")))
        .unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.set_agent_webhook(
            &name("mine"),
            &scope,
            &own("https://amp.test/c")
        ))),
        ErrorKind::Conflict
    );

    // Revoking the token deletes its webhook.
    alice
        .call(|c| c.delete_agent_token(&name("inbox")))
        .unwrap();
    let left: Vec<_> = alice
        .call(|c| c.webhooks())
        .unwrap()
        .into_iter()
        .map(|w| w.name.to_string())
        .collect();
    assert_eq!(left, ["mine"]);
}
