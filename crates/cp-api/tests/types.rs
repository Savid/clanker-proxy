//! The API's types accept what they should and refuse the rest.
// Test fixtures unwrap: a malformed fixture should fail loudly.
#![allow(clippy::unwrap_used)]

use std::collections::HashSet;

use cp_api::{
    DaemonUrl, ListThreadsQuery, Op, Peers, ThreadRef, WebhookEventType, WebhookEvents,
    WebhookUpdate, WireEvent,
};
use cp_thread::{Action, Name};

#[test]
fn daemon_urls() {
    for (url, want) in [
        ("https://cp.example.com", Some("https://cp.example.com")),
        ("https://cp.example.com/", Some("https://cp.example.com")),
        (
            "https://cp.example.com:8443/base/",
            Some("https://cp.example.com:8443/base"),
        ),
        ("http://127.0.0.1:18471", Some("http://127.0.0.1:18471")),
        ("http://127.8.9.10:1", Some("http://127.8.9.10:1")),
        ("http://[::1]:18471", Some("http://[::1]:18471")),
        ("http://localhost:18471", None),
        ("http://cp.example.com", None),
        ("http://192.168.1.2:18471", None),
        ("https://user:pass@cp.example.com", None),
        ("https://cp.example.com/?x=1", None),
        ("https://cp.example.com/#frag", None),
        ("ftp://cp.example.com", None),
        ("cp.example.com", None),
    ] {
        let got = DaemonUrl::try_from(url).ok();
        assert_eq!(got.as_ref().map(DaemonUrl::as_str), want, "{url}");
    }
    let long = format!("https://cp.example.com/{}", "a".repeat(cp_api::MAX_URL));
    assert!(DaemonUrl::try_from(long).is_err());
}

#[test]
fn wire_events() {
    let open = r#"{"id":"00000000-0000-4000-8000-000000000000","thread":"00000000-0000-4000-8000-000000000000","at":"2026-10-07T00:00:00Z","clock":1,"action":"open","title":"Review","kind":"request","labels":["infra"],"body":"Look?"}"#;
    let wire: WireEvent = serde_json::from_str(open).unwrap();
    let e = wire
        .clone()
        .into_event(
            Name::try_from("alice").unwrap(),
            Name::try_from("bob").unwrap(),
        )
        .unwrap();
    assert_eq!(e.action, Action::Open);
    assert_eq!(WireEvent::from(&e), wire);
    assert_eq!(serde_json::to_string(&wire).unwrap(), open);

    let (alice, bob) = (
        Name::try_from("alice").unwrap(),
        Name::try_from("bob").unwrap(),
    );
    for (case, json) in [
        (
            "open without title",
            open.replace(r#""title":"Review","#, ""),
        ),
        (
            "title on a comment",
            open.replace(r#""action":"open""#, r#""action":"comment""#)
                .replace(r#""clock":1"#, r#""clock":2"#),
        ),
        (
            "open at clock 2",
            open.replace(r#""clock":1"#, r#""clock":2"#),
        ),
    ] {
        let wire: WireEvent = serde_json::from_str(&json).unwrap();
        assert!(
            wire.into_event(alice.clone(), bob.clone()).is_err(),
            "{case}"
        );
    }
    for (case, json) in [
        (
            "unknown field",
            open.replace(r#""body""#, r#""from":"mallory","body""#),
        ),
        ("fractional time", open.replace("00:00:00Z", "00:00:00.5Z")),
        ("offset time", open.replace("00:00:00Z", "10:00:00+10:00")),
    ] {
        assert!(serde_json::from_str::<WireEvent>(&json).is_err(), "{case}");
    }
}

#[test]
fn webhook_events() {
    let all: WebhookEvents = serde_json::from_str(r#"["*"]"#).unwrap();
    assert!(all.includes(WebhookEventType::ThreadOverruled));
    let some: WebhookEvents = serde_json::from_str(r#"["thread.open","thread.reply"]"#).unwrap();
    assert!(
        some.includes(WebhookEventType::ThreadReply)
            && !some.includes(WebhookEventType::ThreadClose)
    );
    assert_eq!(
        serde_json::to_string(&some).unwrap(),
        r#"["thread.open","thread.reply"]"#
    );
    for bad in [
        r"[]",
        r#"["*","thread.open"]"#,
        r#"["thread.open","thread.open"]"#,
        r#"["thread.merge"]"#,
    ] {
        assert!(serde_json::from_str::<WebhookEvents>(bad).is_err(), "{bad}");
    }
    assert!(
        serde_json::from_str::<WebhookUpdate>(r#"{"type":"slack"}"#).is_err(),
        "type is immutable"
    );
}

#[test]
fn peers() {
    assert!(serde_json::from_str::<Peers>(r#"["bob","carol"]"#).is_ok());
    assert!(serde_json::from_str::<Peers>(r#"["bob","bob"]"#).is_err());
    let many: Vec<String> = (0..=cp_api::MAX_PEERS).map(|i| format!("p{i}")).collect();
    assert!(serde_json::from_value::<Peers>(serde_json::json!(many)).is_err());
}

#[test]
fn public_keys() {
    assert!(cp_api::PublicKey::try_from("A".repeat(43)).is_ok());
    assert!(cp_api::PublicKey::try_from("A".repeat(42)).is_err());
    assert!(cp_api::PublicKey::try_from(format!("{}=", "A".repeat(42))).is_err());
}

#[test]
fn thread_refs() {
    assert_eq!(
        ThreadRef::try_from("765A0B0C").unwrap().as_str(),
        "765a0b0c"
    );
    assert!(ThreadRef::try_from("abc").is_err());
    assert!(ThreadRef::try_from("zzzz").is_err());
}

#[test]
fn operations_are_unique() {
    let mut ids = HashSet::new();
    let mut routes = HashSet::new();
    for op in Op::ALL {
        assert!(ids.insert(op.id()), "{} twice", op.id());
        assert!(
            routes.insert((op.method(), op.path())),
            "{} {} twice",
            op.method(),
            op.path()
        );
    }
}

#[test]
fn queries_parse() {
    let q: ListThreadsQuery =
        serde_urlencoded::from_str("turn=mine&state=needs-input&limit=20").unwrap();
    assert_eq!(q.limit, Some(20));
    assert!(serde_urlencoded::from_str::<ListThreadsQuery>("bogus=1").is_err());
}

#[test]
fn webhook_type_names_match_serde() {
    for t in cp_api::WebhookType::ALL {
        assert_eq!(
            serde_json::to_value(t).unwrap(),
            serde_json::json!(t.as_str())
        );
        assert_eq!(t.provider().kind, t);
    }
    for t in WebhookEventType::ALL {
        assert_eq!(
            serde_json::to_value(t).unwrap(),
            serde_json::json!(t.as_str())
        );
    }
}
