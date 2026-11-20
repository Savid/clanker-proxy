//! Peering: asking, approving, refusing, and removing.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Clock, Daemon, kind, name, peer, settle, url};
use cp_api::{Approval, ErrorKind, PeerInput, PeerSecret, PeerStatus, PeeringRequest};
use cp_core::{Config, Core, Millis, REQUESTS_PER_MINUTE};
use cp_sqlite::Sqlite;
use cp_thread::Action;

fn request(n: u32) -> PeeringRequest {
    PeeringRequest {
        name: name(&format!("p{n}")),
        url: url(&format!("https://p{n}.test")),
        secret: PeerSecret::new(format!("cpp_{n:043}")).unwrap(),
        note: None,
    }
}

#[test]
fn peering_makes_both_sides_active() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    for (d, other) in [(&mut alice, "bob"), (&mut bob, "alice")] {
        let p = d.call(|c| c.peer(&name(other))).unwrap();
        assert_eq!(p.status, PeerStatus::Active, "{other}");
        assert_eq!(d.call(|c| c.requests()).unwrap().requests, vec![]);
    }
    let (a, b) = (
        alice.call(|c| c.peer(&name("bob"))).unwrap(),
        bob.call(|c| c.peer(&name("alice"))).unwrap(),
    );
    assert_eq!(a.code, b.code, "both owners see the same code");
}

#[test]
fn add_peer_refusals() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    let ask = |n: &str, u: &str| PeerInput {
        name: name(n),
        url: url(u),
        note: None,
    };
    assert_eq!(
        kind(&alice.call(|c| c.add_peer(ask("alice", "https://x.test")))),
        ErrorKind::Unprocessable,
        "own name"
    );
    alice
        .call(|c| c.add_peer(ask("bob", "https://bob.test")))
        .unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.add_peer(ask("bob", "https://bob2.test")))),
        ErrorKind::Conflict,
        "name taken"
    );
    assert_eq!(
        kind(&alice.call(|c| c.add_peer(ask("robert", "https://bob.test")))),
        ErrorKind::Conflict,
        "URL taken"
    );

    // Carol already asked bob: bob should approve, not ask back.
    let mut carol = Daemon::new("carol", &clock, 3);
    let out = carol
        .call(|c| c.add_peer(ask("bob", "https://bob.test")))
        .unwrap();
    bob.call(|c| c.request_received(out.request.as_ref().unwrap()))
        .unwrap();
    let e = bob
        .call(|c| c.add_peer(ask("carol", "https://carol.test")))
        .unwrap_err();
    assert!(
        e.kind == ErrorKind::Conflict && e.detail.contains("approve that request"),
        "{e:?}"
    );

    // Without its own URL a daemon has nothing to give a peer.
    let mut nourl = Daemon {
        core: Core::open(
            Sqlite::memory().unwrap(),
            common::runtime(&clock, 4),
            Config {
                name: Some(name("dan")),
                url: None,
            },
        )
        .unwrap(),
    };
    assert_eq!(
        kind(&nourl.call(|c| c.add_peer(ask("bob", "https://bob.test")))),
        ErrorKind::Unprocessable
    );
}

#[test]
fn an_aborted_send_saves_nothing() {
    let clock = Clock::new();
    let mut alice = Daemon::new("alice", &clock, 1);
    let out = alice
        .call(|c| {
            c.add_peer(PeerInput {
                name: name("bob"),
                url: url("https://bob.test"),
                note: None,
            })
        })
        .unwrap();
    alice
        .call(|c| c.abort_add_peer(&name("bob"), &out.secret))
        .unwrap();
    assert_eq!(
        kind(&alice.call(|c| c.peer(&name("bob")))),
        ErrorKind::NotFound
    );
}

#[test]
fn one_approval_confirms_at_a_time() {
    let clock = Clock::new();
    let mut bob = Daemon::new("bob", &clock, 2);
    bob.call(|c| c.request_received(&request(1))).unwrap();
    bob.call(|c| c.request_received(&request(2))).unwrap();
    let ids: Vec<_> = bob
        .call(|c| c.requests())
        .unwrap()
        .requests
        .into_iter()
        .map(|r| r.id)
        .collect();

    let first = bob
        .call(|c| c.begin_approval(&ids[0], Approval::default()))
        .unwrap();
    assert_eq!(
        kind(&bob.call(|c| c.begin_approval(&ids[1], Approval::default()))),
        ErrorKind::Conflict
    );
    assert_eq!(
        kind(&bob.call(|c| c.deny(&ids[0]))),
        ErrorKind::Conflict,
        "cannot deny mid-approval"
    );
    // Its secret answers 503 until the approval ends: the requester may act
    // on its acceptance before this side has saved the peer.
    assert_eq!(
        kind(&bob.call(|c| c.authenticate_peer(first.call.secret.expose()))),
        ErrorKind::Unavailable
    );

    // An approval whose host stopped midway frees the slot when it expires.
    clock.advance(31 * Millis::SECOND);
    let second = bob
        .call(|c| c.begin_approval(&ids[1], Approval::default()))
        .unwrap();
    assert_eq!(
        kind(&bob.call(|c| c.commit_approval(&first))),
        ErrorKind::Conflict,
        "expired approval"
    );
    bob.call(|c| c.abort_approval(&second)).unwrap();
    assert_eq!(
        bob.call(|c| c.requests()).unwrap().requests.len(),
        2,
        "aborting leaves the requests"
    );
    bob.call(|c| {
        c.begin_approval(
            &ids[0],
            Approval {
                name: Some(name("pat")),
            },
        )
    })
    .and_then(|p| bob.call(|c| c.commit_approval(&p)))
    .unwrap();
    assert_eq!(
        bob.call(|c| c.peer(&name("pat"))).unwrap().status,
        PeerStatus::Active
    );
}

#[test]
fn public_requests_are_limited() {
    let clock = Clock::new();
    let mut bob = Daemon::new("bob", &clock, 2);
    for n in 0..REQUESTS_PER_MINUTE {
        bob.call(|c| c.request_received(&request(n))).unwrap();
    }
    assert_eq!(
        kind(&bob.call(|c| c.request_received(&request(99)))),
        ErrorKind::Unavailable
    );
    for n in REQUESTS_PER_MINUTE..30 {
        clock.advance(Millis::MINUTE);
        bob.call(|c| c.request_received(&request(n))).unwrap();
    }
    let reqs = bob.call(|c| c.requests()).unwrap().requests;
    assert_eq!(reqs.len(), 20, "at most 20 wait");
    assert!(
        reqs.iter().all(|r| r.name != name("p0")),
        "the oldest are dropped"
    );
    clock.advance(8 * Millis::DAY);
    assert!(
        bob.call(|c| c.requests()).unwrap().requests.is_empty(),
        "requests expire"
    );
}

#[test]
fn removing_a_peer_fails_their_queue_and_keeps_the_name() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    let id = alice.send("bob", "t").id.to_string();
    settle(&mut alice, &mut bob);
    alice.act(&id, Action::Comment, "one more").unwrap();
    alice.call(|c| c.remove_peer(&name("bob"))).unwrap();
    let t = alice.thread(&id);
    assert_eq!((t.summary.undelivered, t.summary.failed), (0, 1));
    assert_eq!(
        t.log[1].delivery.as_ref().unwrap().last_error.as_deref(),
        Some("peer removed")
    );
    assert_eq!(
        kind(&alice.act(&id, Action::Comment, "x")),
        ErrorKind::NotFound
    );
    // Threads name bob, so the name stays with bob's URL.
    let e = alice
        .call(|c| {
            c.add_peer(PeerInput {
                name: name("bob"),
                url: url("https://imposter.test"),
                note: None,
            })
        })
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Conflict);
    // Bob's events no longer get in.
    bob.act(&id, Action::Ack, "").unwrap();
    let outcomes = common::deliver(&mut bob, &mut alice);
    assert!(
        matches!(outcomes.as_slice(), [cp_core::Outcome::Unrecognized(_)]),
        "{outcomes:?}"
    );
}
