//! Two daemons acting concurrently over an unreliable network converge once
//! the network heals: same state, same events, same effect for each event.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use common::{Clock, Daemon, peer, settle};
use cp_api::DeliveryStatus;
use cp_core::{Millis, Outcome};
use cp_thread::Action;
use proptest::prelude::*;

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

#[derive(Clone, Debug)]
enum Step {
    /// An owner moves, whether or not the workflow lets them now.
    Act { bob: bool, action: usize },
    /// Some of one side's due deliveries go through.
    Deliver { to_bob: bool, n: usize },
    /// The next delivery fails on the way.
    Drop { to_bob: bool },
    /// The next delivery arrives but its answer is lost, so it is sent again.
    LoseAnswer { to_bob: bool },
    /// Time passes.
    Wait { minutes: i64 },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (any::<bool>(), 0..MOVES.len()).prop_map(|(bob, action)| Step::Act { bob, action }),
        3 => (any::<bool>(), 1..4usize).prop_map(|(to_bob, n)| Step::Deliver { to_bob, n }),
        1 => any::<bool>().prop_map(|to_bob| Step::Drop { to_bob }),
        1 => any::<bool>().prop_map(|to_bob| Step::LoseAnswer { to_bob }),
        1 => (1..120i64).prop_map(|minutes| Step::Wait { minutes }),
    ]
}

fn sides<'a>(
    alice: &'a mut Daemon,
    bob: &'a mut Daemon,
    to_bob: bool,
) -> (&'a mut Daemon, &'a mut Daemon) {
    if to_bob { (alice, bob) } else { (bob, alice) }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn concurrent_owners_converge(script in prop::collection::vec(step(), 1..60)) {
        let clock = Clock::new();
        let (mut alice, mut bob) = (Daemon::new("alice", &clock, 11), Daemon::new("bob", &clock, 22));
        peer(&mut alice, &mut bob);
        let id = alice.send("bob", "simulated").id.to_string();
        settle(&mut alice, &mut bob);

        for s in &script {
            match s.clone() {
                Step::Act { bob: by_bob, action } => {
                    let d = if by_bob { &mut bob } else { &mut alice };
                    let _ = d.act(&id, MOVES[action], "because");
                }
                Step::Deliver { to_bob, n } => {
                    let (from, to) = sides(&mut alice, &mut bob, to_bob);
                    for _ in 0..n {
                        let Some(a) = from.call(|c| c.due(1)).unwrap().pop() else { break };
                        let r = common::receive(to, &a);
                        from.call(|c| c.record(&a, common::outcome(&r))).unwrap();
                    }
                }
                Step::Drop { to_bob } => {
                    let (from, _) = sides(&mut alice, &mut bob, to_bob);
                    if let Some(a) = from.call(|c| c.due(1)).unwrap().pop() {
                        from.call(|c| c.record(&a, Outcome::Failed("connection reset".into()))).unwrap();
                    }
                }
                Step::LoseAnswer { to_bob } => {
                    let (from, to) = sides(&mut alice, &mut bob, to_bob);
                    if let Some(a) = from.call(|c| c.due(1)).unwrap().pop() {
                        let _ = common::receive(to, &a);
                        from.call(|c| c.record(&a, Outcome::Failed("timed out".into()))).unwrap();
                    }
                }
                Step::Wait { minutes } => clock.advance(minutes * Millis::MINUTE),
            }
        }

        // The network heals.
        for _ in 0..20 {
            clock.advance(Millis::HOUR);
            settle(&mut alice, &mut bob);
        }

        let (a, b) = (alice.thread(&id), bob.thread(&id));
        prop_assert_eq!(a.summary.state, b.summary.state);
        prop_assert_eq!(a.summary.events, b.summary.events);
        let effects = |t: &cp_api::Thread| t.log.iter().map(|e| (e.id.clone(), e.action, e.ignored.clone())).collect::<Vec<_>>();
        prop_assert_eq!(effects(&a), effects(&b));
        for (t, me) in [(&a, "alice"), (&b, "bob")] {
            prop_assert_eq!((t.summary.undelivered, t.summary.failed), (0, 0));
            let mine_ignored = t.log.iter().filter(|e| e.from.as_str() == me && e.ignored.is_some()).count();
            prop_assert_eq!(t.summary.overruled as usize, mine_ignored, "{} overruled count", me);
            prop_assert!(t.log.iter().all(|e| e.delivery.as_ref().is_none_or(|d| d.status == DeliveryStatus::Delivered)));
        }
    }
}
