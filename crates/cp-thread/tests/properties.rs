//! Properties replay must hold for any events, not just the tabled ones.
// Test helpers unwrap: a malformed fixture should fail loudly.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use cp_thread::{Action, At, Body, Event, Id, Kind, Labels, Name, Opening, Thread, Title};
use proptest::prelude::*;

const THREAD: &str = "00000000-0000-4000-8000-000000000000";
const MOVES: [Action; 8] = [
    Action::Comment,
    Action::Ack,
    Action::NeedsInput,
    Action::Resolve,
    Action::Close,
    Action::Reopen,
    Action::Decline,
    Action::Withdraw,
];

fn name(s: &str) -> Name {
    Name::try_from(s).unwrap()
}

fn open(kind: Kind) -> Event {
    Event {
        id: Id::try_from(THREAD).unwrap(),
        thread: Id::try_from(THREAD).unwrap(),
        from: name("alice"),
        to: name("bob"),
        at: At::from_unix_seconds(1_791_331_200).unwrap(),
        clock: 1,
        action: Action::Open,
        opening: Some(Opening {
            title: Title::try_from("t").unwrap(),
            kind,
            labels: Labels::default(),
        }),
        body: Body::default(),
    }
}

/// A move: who made it, what it was, the clock it carried, and whether it
/// gave a reason.
type Move = (bool, usize, u64, bool);

fn events(kind: Kind, moves: &[Move]) -> Vec<Event> {
    let mut out = vec![open(kind)];
    for (i, &(by_bob, action, clock, reason)) in moves.iter().enumerate() {
        let (from, to) = if by_bob {
            ("bob", "alice")
        } else {
            ("alice", "bob")
        };
        let n = i64::try_from(i).unwrap();
        out.push(Event {
            id: Id::try_from(format!("00000000-0000-4000-8000-{:012}", i + 1)).unwrap(),
            thread: Id::try_from(THREAD).unwrap(),
            from: name(from),
            to: name(to),
            // Wall clocks disagree with replay order on purpose.
            at: At::from_unix_seconds(1_791_331_200 + 60 * ((n * 7) % 11)).unwrap(),
            clock,
            action: MOVES[action],
            opening: None,
            body: Body::try_from(if reason { "because" } else { "" }).unwrap(),
        });
    }
    out
}

fn kind() -> impl Strategy<Value = Kind> {
    prop_oneof![Just(Kind::Request), Just(Kind::Fyi)]
}

fn moves() -> impl Strategy<Value = Vec<Move>> {
    prop::collection::vec(
        (any::<bool>(), 0..MOVES.len(), 2u64..14, any::<bool>()),
        0..14,
    )
}

proptest! {
    /// Two peers holding the same events reach the same thread, whatever
    /// order the events arrived in.
    #[test]
    fn arrival_order_does_not_matter(kind in kind(), moves in moves(), seed in any::<u64>()) {
        let all = events(kind, &moves);
        let mut shuffled = all.clone();
        shuffle(&mut shuffled, seed);
        prop_assert_eq!(Thread::replay(shuffled).unwrap(), Thread::replay(all).unwrap());
    }

    /// Adding events one at a time, folding in those that sort last and
    /// replaying for those that do not, gives what a full replay gives.
    #[test]
    fn insert_matches_replay(kind in kind(), moves in moves(), seed in any::<u64>()) {
        let all = events(kind, &moves);
        let mut rest = all.get(1..).unwrap_or_default().to_vec();
        shuffle(&mut rest, seed);
        let mut thread = Thread::replay([open(kind)]).unwrap();
        for e in rest {
            thread = thread.insert(e).unwrap();
        }
        prop_assert_eq!(thread, Thread::replay(all).unwrap());
    }

    /// What a participant is told they may do is exactly what the workflow
    /// accepts from them, in every state replay can reach.
    #[test]
    fn allowed_is_what_check_accepts(kind in kind(), moves in moves()) {
        let thread = Thread::replay(events(kind, &moves)).unwrap();
        let fold = &thread.fold;
        for user in [name("alice"), name("bob")] {
            let allowed = fold.allowed(&user);
            for action in MOVES {
                let mut e = events(kind, &[(user == name("bob"), 0, fold.next_clock(), true)])
                    .pop()
                    .unwrap();
                e.action = action;
                prop_assert_eq!(fold.check(&e).is_ok(), allowed.contains(&action), "{} {}", user, action);
            }
        }
    }

    #[test]
    fn times_round_trip(seconds in 0i64..253_402_300_800) {
        let at = At::from_unix_seconds(seconds).unwrap();
        prop_assert_eq!(at.to_string().parse::<At>().unwrap(), at);
    }

    #[test]
    fn random_ids_are_valid(bytes in any::<[u8; 16]>()) {
        let id = Id::from_random(bytes);
        prop_assert_eq!(Id::try_from(id.as_str()).unwrap(), id);
    }
}

/// A deterministic Fisher–Yates shuffle, so a failing case reproduces.
fn shuffle<T>(items: &mut [T], mut seed: u64) {
    for i in (1..items.len()).rev() {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = usize::try_from(seed >> 33).unwrap() % (i + 1);
        items.swap(i, j);
    }
}
