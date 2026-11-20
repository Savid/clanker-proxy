//! Delivery to peers: order, retries, and when to give up.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Clock, Daemon, deliver, peer, settle};
use cp_core::{GIVE_UP_401, MAX_BACKOFF, Millis, Outcome, backoff};
use cp_thread::{Action, State};

#[test]
fn backoff_doubles_to_an_hour() {
    let want = [(0, 15), (1, 30), (2, 60), (6, 960), (8, 3600), (100, 3600)];
    for (attempts, seconds) in want {
        assert_eq!(
            backoff(attempts),
            seconds * Millis::SECOND,
            "after {attempts}"
        );
    }
}

/// Fails every due attempt as if the peer were down.
fn fail_all(d: &mut Daemon, why: &Outcome) -> usize {
    let due = d.call(|c| c.due(40)).unwrap();
    let n = due.len();
    for a in due {
        d.call(|c| c.record(&a, why.clone())).unwrap();
    }
    n
}

#[test]
fn a_peer_down_for_weeks_still_converges() {
    let (clock, mut alice, mut bob) = {
        let clock = Clock::new();
        let mut a = Daemon::new("alice", &clock, 1);
        let mut b = Daemon::new("bob", &clock, 2);
        peer(&mut a, &mut b);
        (clock, a, b)
    };
    let id = alice.send("bob", "t").id.to_string();
    settle(&mut alice, &mut bob);
    bob.act(&id, Action::NeedsInput, "which tag?").unwrap();
    settle(&mut alice, &mut bob);

    // Alice answers while bob's daemon is off for three weeks.
    alice.act(&id, Action::Comment, "v1.2").unwrap();
    for _ in 0..(21 * 24) {
        clock.advance(MAX_BACKOFF);
        fail_all(&mut alice, &Outcome::Failed("connection refused".into()));
    }
    let pending = alice
        .thread(&id)
        .log
        .last()
        .unwrap()
        .delivery
        .clone()
        .unwrap();
    assert_eq!(
        pending.status,
        cp_api::DeliveryStatus::Pending,
        "never given up: {pending:?}"
    );

    clock.advance(MAX_BACKOFF);
    settle(&mut alice, &mut bob);
    assert_eq!(bob.summary(&id).state, State::Acked, "the answer arrived");
    assert_eq!(alice.summary(&id).state, bob.summary(&id).state);
}

#[test]
fn a_peer_that_forgot_this_daemon_is_given_up_after_a_week() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    let id = alice.send("bob", "t").id.to_string();

    fail_all(&mut alice, &Outcome::Unrecognized("401".into()));
    for _ in 0..(6 * 24) {
        clock.advance(Millis::HOUR);
        fail_all(&mut alice, &Outcome::Unrecognized("401".into()));
    }
    assert_eq!(
        alice.summary(&id).undelivered,
        1,
        "six days of 401 is not enough"
    );

    // Any success resets the clock.
    clock.advance(Millis::HOUR);
    let a = alice.call(|c| c.due(40)).unwrap().pop().unwrap();
    alice.call(|c| c.record(&a, Outcome::Delivered)).unwrap();
    alice.act(&id, Action::Comment, "more").unwrap();
    for _ in 0..(6 * 24) {
        clock.advance(Millis::HOUR);
        fail_all(&mut alice, &Outcome::Unrecognized("401".into()));
    }
    assert_eq!(alice.summary(&id).failed, 0);
    for _ in 0..(2 * 24) {
        clock.advance(Millis::HOUR);
        fail_all(&mut alice, &Outcome::Unrecognized("401".into()));
    }
    let s = alice.summary(&id);
    assert_eq!(
        (s.undelivered, s.failed),
        (0, 1),
        "given up after {GIVE_UP_401} ms of 401"
    );
    let _ = &mut bob;
}

#[test]
fn a_peer_that_never_approved_is_given_up() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    common::ask(&mut alice, &mut bob);
    let id = alice.send("bob", "t").id.to_string();
    for _ in 0..(8 * 24) {
        clock.advance(Millis::HOUR);
        fail_all(&mut alice, &Outcome::Unrecognized("401".into()));
    }
    let s = alice.summary(&id);
    assert_eq!((s.undelivered, s.failed), (0, 1));
}

#[test]
fn a_peers_events_go_in_order() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    let id = alice.send("bob", "t").id.to_string();
    alice.act(&id, Action::Comment, "one").unwrap();
    alice.act(&id, Action::Comment, "two").unwrap();
    // Only the oldest is ever due; the rest wait behind it.
    assert_eq!(alice.call(|c| c.due(40)).unwrap().len(), 1);
    assert_eq!(fail_all(&mut alice, &Outcome::Failed("503".into())), 1);
    assert!(
        alice.call(|c| c.due(40)).unwrap().is_empty(),
        "the queue waits for its head"
    );
    clock.advance(backoff(0));
    let outcomes = deliver(&mut alice, &mut bob);
    assert_eq!(outcomes, vec![Outcome::Delivered; 3]);
    assert_eq!(bob.summary(&id).events, 3);
}

#[test]
fn a_stale_attempt_changes_nothing() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    alice.send("bob", "t");
    let a = alice.call(|c| c.due(40)).unwrap().pop().unwrap();
    alice
        .call(|c| c.record(&a, Outcome::Failed("timeout".into())))
        .unwrap();
    assert!(!alice.call(|c| c.still_pending(&a)).unwrap());
    alice.call(|c| c.record(&a, Outcome::Delivered)).unwrap();
    let t = alice.call(|c| c.due(40)).unwrap();
    assert!(t.is_empty(), "still waiting out its backoff");
    let _ = &mut bob;
}
