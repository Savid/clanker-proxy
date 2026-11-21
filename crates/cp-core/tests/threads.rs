//! Threads between two daemons: the workflow, refusals, and replay.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Clock, Daemon, deliver, kind, name, peer, scope, settle};
use cp_api::{
    ErrorKind, GetThreadQuery, ListThreadsQuery, NewThread, ThreadRef, TurnFilter, WireEvent,
};
use cp_core::Millis;
use cp_thread::{Action, Body, State, Title};

fn pair() -> (Clock, Daemon, Daemon) {
    let clock = Clock::new();
    let mut alice = Daemon::new("alice", &clock, 1);
    let mut bob = Daemon::new("bob", &clock, 2);
    peer(&mut alice, &mut bob);
    (clock, alice, bob)
}

#[test]
fn a_request_runs_its_course_on_both_sides() {
    let (_, mut alice, mut bob) = pair();
    let sent = alice.send("bob", "Bump the reth image");
    let id = sent.id.to_string();
    assert_eq!(sent.undelivered, 1);
    assert!(!sent.my_turn && sent.turn == Some(name("bob")));
    settle(&mut alice, &mut bob);
    assert_eq!(alice.summary(&id).undelivered, 0);

    let got = bob.summary(&id);
    assert!(got.my_turn && got.last_from == name("alice"));
    assert_eq!(
        got.actions,
        vec![
            Action::Comment,
            Action::Ack,
            Action::NeedsInput,
            Action::Resolve,
            Action::Close,
            Action::Decline
        ]
    );

    for (who, action, body, state) in [
        ("bob", Action::Ack, "", State::Acked),
        ("bob", Action::NeedsInput, "which tag?", State::NeedsInput),
        ("alice", Action::Comment, "v1.2", State::Acked),
        ("bob", Action::Resolve, "done in #4312", State::Resolved),
        ("alice", Action::Close, "", State::Closed),
    ] {
        let d = if who == "alice" { &mut alice } else { &mut bob };
        assert_eq!(
            d.act(&id, action, body).unwrap().state,
            state,
            "{who} {action}"
        );
        settle(&mut alice, &mut bob);
        assert_eq!(alice.summary(&id).state, state);
        assert_eq!(bob.summary(&id).state, state);
    }
    let (a, b) = (alice.thread(&id), bob.thread(&id));
    assert_eq!(a.summary.events, 6);
    let ids = |t: &cp_api::Thread| t.log.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&a), ids(&b), "both sides replay the same order");
}

#[test]
fn refusals_say_what_is_allowed() {
    let (_, mut alice, mut bob) = pair();
    let id = alice.send("bob", "t").id.to_string();
    settle(&mut alice, &mut bob);

    let e = alice.act(&id, Action::Ack, "").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Conflict);
    assert_eq!(
        e.detail,
        "only the recipient may ack; you can now comment, close, withdraw"
    );

    assert_eq!(kind(&alice.act(&id, Action::Open, "")), ErrorKind::Invalid);
    bob.act(&id, Action::Resolve, "done").unwrap();
    settle(&mut alice, &mut bob);
    assert_eq!(
        kind(&alice.act(&id, Action::Reopen, "  ")),
        ErrorKind::Invalid,
        "reopen needs a reason"
    );
    assert_eq!(
        kind(&alice.act("ffff", Action::Comment, "x")),
        ErrorKind::NotFound
    );
}

#[test]
fn an_agent_scope_hides_other_peers_and_their_threads() {
    let clock = Clock::new();
    let (mut alice, mut bob, mut carol) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
        Daemon::new("carol", &clock, 3),
    );
    peer(&mut alice, &mut bob);
    peer(&mut alice, &mut carol);
    let to_bob = alice.send("bob", "for bob").id;
    let to_carol = alice.send("carol", "for carol").id;

    let only_bob = scope(&["bob"]);
    let listed = alice
        .call(|c| c.threads(&ListThreadsQuery::default(), Some(&only_bob)))
        .unwrap();
    assert_eq!(
        listed.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        vec![to_bob]
    );
    let r = ThreadRef::try_from(to_carol.as_str()).unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.thread(&r, Some(&only_bob), &GetThreadQuery::default()))),
        ErrorKind::NotFound
    );
    let all = alice
        .call(|c| {
            c.threads(
                &ListThreadsQuery {
                    turn: Some(TurnFilter::Theirs),
                    ..Default::default()
                },
                None,
            )
        })
        .unwrap();
    assert_eq!(all.len(), 2);

    // It reads, and sends to, only its peers; the rest look like no peer.
    let peers = alice.call(|c| c.peers(Some(&only_bob))).unwrap().peers;
    assert_eq!(
        peers.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["bob"]
    );
    assert_eq!(alice.call(|c| c.peers(None)).unwrap().peers.len(), 2);
    assert!(
        alice
            .call(|c| c.peer(&name("bob"), Some(&only_bob)))
            .is_ok()
    );
    assert_eq!(
        kind(&alice.call(|c| c.peer(&name("carol"), Some(&only_bob)))),
        ErrorKind::NotFound
    );
    let open = |to: &str| NewThread {
        to: name(to),
        title: Title::try_from("from the agent").unwrap(),
        kind: None,
        labels: Vec::new(),
        body: Body::try_from("details").unwrap(),
    };
    let sent = alice
        .call(|c| c.open_thread(open("bob"), Some(&only_bob)))
        .unwrap();
    assert_eq!(sent.peer.as_str(), "bob");
    let refused = alice.call(|c| c.open_thread(open("carol"), Some(&only_bob)));
    assert_eq!(kind(&refused), ErrorKind::NotFound);
    assert_eq!(
        refused.unwrap_err().to_string(),
        "carol is not a peer",
        "an out-of-scope peer answers like no peer"
    );
}

#[test]
fn concurrent_moves_converge_and_the_loser_learns_it() {
    let (_, mut alice, mut bob) = pair();
    let id = alice.send("bob", "t").id.to_string();
    settle(&mut alice, &mut bob);

    // Both act before hearing from the other.
    alice.act(&id, Action::Withdraw, "not needed").unwrap();
    bob.act(&id, Action::Resolve, "done").unwrap();
    settle(&mut alice, &mut bob);

    let (a, b) = (alice.summary(&id), bob.summary(&id));
    assert_eq!(a.state, b.state, "both sides agree");
    assert_eq!(
        a.overruled + b.overruled,
        1,
        "exactly one move lost: {a:?} {b:?}"
    );
    let loser = if a.overruled == 1 {
        alice.thread(&id)
    } else {
        bob.thread(&id)
    };
    assert!(loser.log.iter().any(|e| e.ignored.is_some()));
}

#[test]
fn peer_events_are_checked() {
    let (clock, mut alice, mut bob) = pair();
    let id = alice.send("bob", "t").id.to_string();
    settle(&mut alice, &mut bob);
    // A delivery from bob, already authenticated by its signature.
    bob.act(&id, Action::Ack, "").unwrap();
    let wire = bob.call(|c| c.due(10)).unwrap().pop().unwrap().event;
    let key = bob.core.public_key().clone();

    // A delivery dated far ahead is refused for now.
    let mut future = wire.clone();
    future.at = (clock.now().plus(20 * Millis::MINUTE)).at();
    assert_eq!(
        kind(&alice.call(|c| c.receive(&key, future))),
        ErrorKind::Unavailable
    );

    assert!(
        !alice
            .call(|c| c.receive(&key, wire.clone()))
            .unwrap()
            .duplicate
    );
    assert!(
        alice
            .call(|c| c.receive(&key, wire.clone()))
            .unwrap()
            .duplicate,
        "redelivery is harmless"
    );
    let mut changed: WireEvent = wire.clone();
    changed.body = cp_thread::Body::try_from("different").unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.receive(&key, changed))),
        ErrorKind::Conflict
    );

    let mut stale = wire.clone();
    stale.id = cp_thread::Id::try_from("00000000-0000-4000-8000-0000000000aa").unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.receive(&key, stale))),
        ErrorKind::Unprocessable,
        "clock not after bob's own"
    );

    let mut stray = wire;
    stray.id = cp_thread::Id::try_from("00000000-0000-4000-8000-0000000000ab").unwrap();
    stray.thread = cp_thread::Id::try_from("00000000-0000-4000-8000-0000000000ac").unwrap();
    stray.clock = 9;
    assert_eq!(
        kind(&alice.call(|c| c.receive(&key, stray))),
        ErrorKind::Unprocessable,
        "unknown thread"
    );
    assert_eq!(
        kind(&alice.call(|c| c.receive(
            &cp_api::PublicKey::try_from("A".repeat(43)).unwrap(),
            cp_api_wire()
        ))),
        ErrorKind::Unauthorized
    );
}

fn cp_api_wire() -> WireEvent {
    serde_json::from_str(
        r#"{"id":"00000000-0000-4000-8000-000000000001","thread":"00000000-0000-4000-8000-000000000001","at":"2026-10-07T00:00:00Z","clock":1,"action":"open","title":"t","kind":"request"}"#,
    )
    .unwrap()
}

#[test]
fn pages_of_a_thread() {
    let (_, mut alice, mut bob) = pair();
    let id = alice.send("bob", "t").id.to_string();
    for i in 0..5 {
        alice
            .act(&id, Action::Comment, &format!("note {i}"))
            .unwrap();
    }
    settle(&mut alice, &mut bob);
    let r = ThreadRef::try_from(id.as_str()).unwrap();
    let last = bob
        .call(|c| {
            c.thread(
                &r,
                None,
                &GetThreadQuery {
                    limit: Some(2),
                    before: None,
                },
            )
        })
        .unwrap();
    assert_eq!((last.log.len(), last.log_start), (2, 4));
    let before = bob
        .call(|c| {
            c.thread(
                &r,
                None,
                &GetThreadQuery {
                    limit: Some(10),
                    before: Some(last.log_start),
                },
            )
        })
        .unwrap();
    assert_eq!((before.log.len(), before.log_start), (4, 0));
    assert_eq!(before.log.first().unwrap().action, Action::Open);
}

#[test]
fn a_thread_the_peer_never_received_refuses_more() {
    let (_, mut alice, mut bob) = pair();
    let id = alice.send("bob", "t").id.to_string();
    // Bob's daemon refuses the open for good.
    let a = alice.call(|c| c.due(10)).unwrap().pop().unwrap();
    alice
        .call(|c| c.record(&a, cp_core::Outcome::Refused("422 bad".into())))
        .unwrap();
    assert_eq!(alice.summary(&id).failed, 1);
    assert_eq!(
        kind(&alice.act(&id, Action::Comment, "hello?")),
        ErrorKind::Conflict
    );
    assert_eq!(deliver(&mut alice, &mut bob), vec![]);
}
