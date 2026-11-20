//! The workflow, replay and validation, table by table.
// Test helpers unwrap: a malformed fixture should fail loudly.
#![allow(clippy::unwrap_used, clippy::unused_self)]

use cp_thread::{
    Action, At, Body, ENDING_SLACK, Event, Fold, Id, Kind, Labels, MAX_BODY, MAX_EVENTS, MAX_TITLE,
    Name, Opening, State, Thread, Title,
};

const THREAD: &str = "00000000-0000-4000-8000-000000000000";

fn name(s: &str) -> Name {
    Name::try_from(s).unwrap()
}

fn alice() -> Name {
    name("alice")
}

fn bob() -> Name {
    name("bob")
}

fn t0() -> At {
    "2026-10-07T00:00:00Z".parse().unwrap()
}

/// Mints events for one thread, one minute apart.
#[derive(Default)]
struct Builder {
    n: u64,
}

impl Builder {
    fn open(&self, kind: Kind) -> Event {
        Event {
            id: Id::try_from(THREAD).unwrap(),
            thread: Id::try_from(THREAD).unwrap(),
            from: alice(),
            to: bob(),
            at: t0(),
            clock: 1,
            action: Action::Open,
            opening: Some(Opening {
                title: Title::try_from("Review the plan").unwrap(),
                kind,
                labels: Labels::default(),
            }),
            body: Body::try_from("Can you look?").unwrap(),
        }
    }

    fn ev(&mut self, from: &Name, action: Action) -> Event {
        self.n += 1;
        let to = if *from == bob() { alice() } else { bob() };
        let body = if action == Action::Reopen {
            "There is more to do."
        } else {
            ""
        };
        Event {
            id: Id::try_from(format!("00000000-0000-4000-8000-{:012}", self.n)).unwrap(),
            thread: Id::try_from(THREAD).unwrap(),
            from: from.clone(),
            to,
            at: At::from_unix_seconds(t0().as_unix_seconds() + 60 * i64::try_from(self.n).unwrap())
                .unwrap(),
            clock: self.n + 1,
            action,
            opening: None,
            body: Body::try_from(body).unwrap(),
        }
    }
}

fn with(mut e: Event, body: &str) -> Event {
    e.body = Body::try_from(body).unwrap();
    e
}

fn last_ignored(t: &Thread) -> String {
    t.events
        .last()
        .and_then(|a| a.ignored.as_ref())
        .map_or_else(String::new, ToString::to_string)
}

/// A move and what should follow: the thread's state, and why the move was
/// ignored, if it was.
type Step<'a> = (&'a Name, Action, State, &'static str);

#[test]
#[allow(clippy::too_many_lines)]
fn workflow() {
    let (a, b) = (alice(), bob());
    let cases: Vec<(&str, Kind, Vec<Step>)> = vec![
        (
            "request to closed",
            Kind::Request,
            vec![
                (&b, Action::Ack, State::Acked, ""),
                (&b, Action::NeedsInput, State::NeedsInput, ""),
                (&a, Action::Comment, State::Acked, ""),
                (&b, Action::Comment, State::Acked, ""),
                (&b, Action::Resolve, State::Resolved, ""),
                (&a, Action::Reopen, State::Acked, ""),
                (&b, Action::Resolve, State::Resolved, ""),
                (&a, Action::Close, State::Closed, ""),
                (&a, Action::Comment, State::Closed, ""),
                (&a, Action::Reopen, State::Acked, ""),
            ],
        ),
        (
            "fyi closes on ack",
            Kind::Fyi,
            vec![(&b, Action::Ack, State::Closed, "")],
        ),
        (
            "recipient-only actions",
            Kind::Request,
            vec![
                (&a, Action::Ack, State::Open, "only the recipient may ack"),
                (
                    &a,
                    Action::Resolve,
                    State::Open,
                    "only the recipient may resolve",
                ),
                (&b, Action::Resolve, State::Resolved, ""),
                (&b, Action::Close, State::Closed, ""),
                (&b, Action::Reopen, State::Acked, ""),
            ],
        ),
        (
            "terminal states hold",
            Kind::Request,
            vec![
                (&b, Action::Decline, State::Declined, ""),
                (
                    &b,
                    Action::Ack,
                    State::Declined,
                    "cannot ack a thread that is declined",
                ),
                (
                    &a,
                    Action::Reopen,
                    State::Declined,
                    "cannot reopen a thread that is declined",
                ),
                (
                    &a,
                    Action::Withdraw,
                    State::Declined,
                    "cannot withdraw a thread that is declined",
                ),
            ],
        ),
        (
            "sender withdraws",
            Kind::Request,
            vec![
                (&b, Action::Ack, State::Acked, ""),
                (&a, Action::Withdraw, State::Withdrawn, ""),
            ],
        ),
        (
            "either participant closes without resolution",
            Kind::Request,
            vec![
                (&b, Action::Close, State::Closed, ""),
                (&a, Action::Reopen, State::Acked, ""),
                (&a, Action::Close, State::Closed, ""),
                (&b, Action::Reopen, State::Acked, ""),
                (&b, Action::NeedsInput, State::NeedsInput, ""),
                (&b, Action::Close, State::Closed, ""),
            ],
        ),
        (
            "no second open",
            Kind::Request,
            vec![(&a, Action::Open, State::Open, "thread is already open")],
        ),
    ];

    for (case, kind, steps) in cases {
        let mut builder = Builder::default();
        let mut events = vec![builder.open(kind)];
        for (from, action, state, ignored) in steps {
            events.push(builder.ev(from, action));
            let got = Thread::replay(events.clone()).unwrap();
            assert_eq!(
                (got.fold.state, last_ignored(&got).as_str()),
                (state, ignored),
                "{case}: after {from} {action}"
            );
        }
    }
}

/// Two peers acting at once see the same result once both events arrive,
/// whichever arrived first.
#[test]
fn replay_is_order_independent() {
    let mut b = Builder::default();
    let open = b.open(Kind::Request);
    let resolve = b.ev(&bob(), Action::Resolve);
    let mut withdraw = b.ev(&alice(), Action::Withdraw);
    // Both acted having seen only the open: their clocks are equal, and the
    // IDs break the tie the same way on both sides.
    withdraw.clock = resolve.clock;

    let first = Thread::replay([withdraw.clone(), resolve.clone(), open.clone()]).unwrap();
    let second = Thread::replay([open.clone(), resolve, withdraw]).unwrap();
    assert_eq!(first, second);
    assert_eq!(second.fold.state, State::Resolved);
    assert!(
        second.events[2]
            .ignored
            .as_ref()
            .is_some_and(|r| r.to_string().contains("cannot withdraw"))
    );
    assert_eq!(second.overruled(&alice()).count(), 1);
    assert_eq!(second.overruled(&bob()).count(), 0);

    // An event follows what its author had seen, even with an earlier time.
    let mut ack = b.ev(&bob(), Action::Ack);
    ack.at = At::from_unix_seconds(open.at.as_unix_seconds() - 3600).unwrap();
    let got = Thread::replay([ack, open]).unwrap();
    assert_eq!(got.fold.state, State::Acked);
}

#[test]
fn replay_concurrent_close_and_reopen() {
    let (a, b) = (alice(), bob());
    let cases = [
        (
            "both close",
            false,
            &b,
            Action::Close,
            Action::Close,
            State::Closed,
            true,
        ),
        (
            "resolve then close",
            false,
            &b,
            Action::Resolve,
            Action::Close,
            State::Closed,
            false,
        ),
        (
            "close then resolve",
            false,
            &a,
            Action::Close,
            Action::Resolve,
            State::Closed,
            true,
        ),
        (
            "both reopen",
            true,
            &b,
            Action::Reopen,
            Action::Reopen,
            State::Acked,
            true,
        ),
        (
            "close then reopen",
            true,
            &b,
            Action::Close,
            Action::Reopen,
            State::Acked,
            false,
        ),
        (
            "reopen then close",
            true,
            &b,
            Action::Reopen,
            Action::Close,
            State::Closed,
            false,
        ),
    ];
    for (case, resolved, first_from, first, second, state, last_ignored) in cases {
        let mut builder = Builder::default();
        let mut events = vec![builder.open(Kind::Request)];
        if resolved {
            events.push(builder.ev(&b, Action::Resolve));
        }
        let first = builder.ev(first_from, first);
        let mut second = builder.ev(&first.to, second);
        second.clock = first.clock;
        events.extend([first, second]);

        let forward = Thread::replay(events.clone()).unwrap();
        events.reverse();
        let backward = Thread::replay(events).unwrap();
        assert_eq!(
            forward, backward,
            "{case}: arrival order changed the replay"
        );

        let last = forward.events.last().unwrap();
        assert_eq!(
            (forward.fold.state, last.ignored.is_some()),
            (state, last_ignored),
            "{case}"
        );
    }
}

#[test]
fn reopen_requires_reason() {
    for state in [State::Resolved, State::Closed] {
        for from in [alice(), bob()] {
            for body in ["", " \t\r\n\u{2003}", "Still failing"] {
                let mut b = Builder::default();
                let mut events = vec![b.open(Kind::Request), b.ev(&bob(), Action::Resolve)];
                if state == State::Closed {
                    events.push(b.ev(&alice(), Action::Close));
                }
                let thread = Thread::replay(events.clone()).unwrap();
                let e = with(b.ev(&from, Action::Reopen), body);
                let valid = body == "Still failing";
                assert_eq!(
                    thread.fold.check(&e).is_ok(),
                    valid,
                    "{from} reopening {state} with {body:?}"
                );

                events.push(e);
                let got = Thread::replay(events).unwrap();
                let want = if valid { State::Acked } else { state };
                assert_eq!(got.fold.state, want);
                assert_eq!(got.events.last().unwrap().ignored.is_none(), valid);
            }
        }
    }
}

#[test]
fn replay_errors() {
    let mut b = Builder::default();
    assert!(
        Thread::replay([b.ev(&bob(), Action::Ack)]).is_err(),
        "replay without open"
    );

    let mut stray = b.ev(&bob(), Action::Ack);
    stray.thread = Id::try_from("00000000-0000-4000-8000-ffffffffffff").unwrap();
    assert!(
        Thread::replay([b.open(Kind::Request), stray]).is_err(),
        "another thread's event"
    );

    let mut outsider = b.ev(&bob(), Action::Comment);
    outsider.from = name("mallory");
    let got = Thread::replay([b.open(Kind::Request), outsider]).unwrap();
    assert!(got.events[1].ignored.is_some(), "outsider comment applied");
}

#[test]
fn check_clock() {
    let mut b = Builder::default();
    let resolve = b.ev(&bob(), Action::Resolve); // clock 2
    let closed = b.ev(&alice(), Action::Close); // clock 3
    let thread = Thread::replay([b.open(Kind::Request), resolve, closed]).unwrap();
    let next = thread.fold.next_clock();

    for (case, from, clock, ok) in [
        ("after everything", bob(), 4, true),
        ("concurrent with alice", bob(), 3, true),
        ("at bob's own earlier clock", bob(), 2, false),
        ("before bob's own clock", bob(), 1, false),
        ("at alice's own clock", alice(), 3, false),
        ("at the lead limit", bob(), next + 1000, true),
        ("past the lead limit", bob(), next + 1001, false),
    ] {
        let mut e = b.ev(&from, Action::Decline);
        e.clock = clock;
        assert_eq!(thread.fold.check_clock(&e).is_ok(), ok, "{case}");
    }
}

#[test]
fn lengths_count_characters() {
    assert!(Title::try_from("界".repeat(MAX_TITLE)).is_ok());
    assert!(Title::try_from("界".repeat(MAX_TITLE + 1)).is_err());
    assert!(Body::try_from("界".repeat(MAX_BODY)).is_ok());
    assert!(Body::try_from("界".repeat(MAX_BODY + 1)).is_err());
}

#[test]
fn control_characters() {
    for (title, body, ok) in [
        ("Plain", "line one\n\tindented\r\nline three", true),
        ("Tab\there", "", false),
        ("Bell\x07", "", false),
        ("Clean", "escape \x1b[8mhidden", false),
        ("Clean", "c1 \u{9b}31m", false),
        ("Right\u{202e}to left", "", false),
        ("Clean", "a\u{2028}line separator", false),
        ("Clean", "isolate \u{2066}x\u{2069}", false),
        ("Clean", "emoji 👨\u{200d}👩\u{200d}👧 joins", true),
    ] {
        let got = Title::try_from(title).is_ok() && Body::try_from(body).is_ok();
        assert_eq!(got, ok, "title {title:?} body {body:?}");
    }
}

#[test]
fn allowed_matches_check() {
    use Action::{Ack, Close, Comment, Decline, NeedsInput, Reopen, Resolve, Withdraw};

    let b = Builder::default();
    let open = b.open(Kind::Request);
    for (state, sender, recipient) in [
        (
            State::Open,
            vec![Comment, Close, Withdraw],
            vec![Comment, Ack, NeedsInput, Resolve, Close, Decline],
        ),
        (
            State::Acked,
            vec![Comment, Close, Withdraw],
            vec![Comment, NeedsInput, Resolve, Close, Decline],
        ),
        (
            State::NeedsInput,
            vec![Comment, Close, Withdraw],
            vec![Comment, Resolve, Close, Decline],
        ),
        (
            State::Resolved,
            vec![Comment, Close, Reopen],
            vec![Comment, Close, Reopen],
        ),
        (State::Closed, vec![Comment, Reopen], vec![Comment, Reopen]),
        (State::Declined, vec![Comment], vec![Comment]),
        (State::Withdrawn, vec![Comment], vec![Comment]),
    ] {
        let mut fold = Fold::open(&open).unwrap();
        fold.state = state;
        assert_eq!(fold.allowed(&alice()), sender, "{state}: sender");
        assert_eq!(fold.allowed(&bob()), recipient, "{state}: recipient");
        assert!(
            fold.allowed(&name("mallory")).is_empty(),
            "{state}: outsider"
        );

        for (user, allowed) in [
            (alice(), &sender),
            (bob(), &recipient),
            (name("mallory"), &vec![]),
        ] {
            for action in Action::ALL {
                let mut b = Builder::default();
                let mut e = with(b.ev(&user, action), "There is more to do.");
                e.to = fold.peer(&user).clone();
                assert_eq!(
                    fold.check(&e).is_ok(),
                    allowed.contains(&action),
                    "{state}: {user} checking {action}"
                );
            }
        }
    }
}

#[test]
fn turn() {
    let open = Builder::default().open(Kind::Request);
    for state in State::ALL {
        let mut fold = Fold::open(&open).unwrap();
        fold.state = state;
        let want = match state {
            State::Open | State::Acked => Some(bob()),
            State::NeedsInput | State::Resolved => Some(alice()),
            State::Closed | State::Declined | State::Withdrawn => None,
        };
        assert_eq!(fold.turn().cloned(), want, "turn in {state}");
    }
}

/// Every non-open, non-comment action has a rule, and every rule is usable.
#[test]
fn rules_cover_actions() {
    for action in Action::ALL {
        match (action, action.rule()) {
            (Action::Open | Action::Comment, rule) => {
                assert!(rule.is_none(), "{action} has a rule");
            }
            (_, Some(rule)) => assert!(!rule.from.is_empty() && !rule.by.is_empty(), "{action}"),
            (_, None) => panic!("{action} has no rule"),
        }
    }
}

#[test]
fn event_round_trip() {
    let mut open = Builder::default().open(Kind::Request);
    if let Some(o) = open.opening.as_mut() {
        o.labels = Labels::sorted(vec![
            "repo/x".try_into().unwrap(),
            "infra".try_into().unwrap(),
        ])
        .unwrap();
    }
    let json = serde_json::to_string(&open).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(back, open);
    assert!(json.contains(r#""at":"2026-10-07T00:00:00Z""#), "{json}");
}

#[test]
fn parse_rejects() {
    let valid = serde_json::to_string(&Builder::default().open(Kind::Request)).unwrap();
    let parse = |s: &str| {
        serde_json::from_str::<Event>(s)
            .map_err(|e| e.to_string())?
            .validate()
            .map_err(|e| e.to_string())
    };
    assert!(parse(&valid).is_ok(), "{valid}");

    for (case, payload) in [
        (
            "unknown field",
            valid.replacen(r#"{"id""#, r#"{"extra":true,"id""#, 1),
        ),
        ("trailing data", format!("{valid}{{}}")),
        (
            "open clock",
            valid.replacen(r#""clock":1"#, r#""clock":2"#, 1),
        ),
        (
            "local time",
            valid.replacen(r#"00:00Z""#, r#"00:00+10:00""#, 1),
        ),
        ("fraction", valid.replacen(r#"00:00Z""#, r#"00:00.5Z""#, 1)),
        (
            "upper user",
            valid.replacen(r#""from":"alice""#, r#""from":"Alice""#, 1),
        ),
        (
            "self",
            valid.replacen(r#""to":"bob""#, r#""to":"alice""#, 1),
        ),
        ("bad action", valid.replacen(r#""open""#, r#""merge""#, 1)),
        ("bad kind", valid.replacen(r#""request""#, r#""task""#, 1)),
        (
            "title on ack",
            valid
                .replacen(r#""action":"open""#, r#""action":"ack""#, 1)
                .replacen(r#""clock":1"#, r#""clock":2"#, 1),
        ),
        (
            "blank title",
            valid.replacen(r#""Review the plan""#, r#"" ""#, 1),
        ),
        (
            "open thread",
            valid.replacen(
                &format!(r#""thread":"{THREAD}""#),
                r#""thread":"00000000-0000-4000-8000-000000000001""#,
                1,
            ),
        ),
    ] {
        assert!(parse(&payload).is_err(), "{case}: {payload}");
    }
}

#[test]
fn normalize_name() {
    for (input, ok) in [
        ("Savid".to_string(), true),
        ("a-b".to_string(), true),
        ("-a".to_string(), false),
        ("a--b".to_string(), false),
        ("a-".to_string(), false),
        (String::new(), false),
        ("a".repeat(39), true),
        ("a".repeat(40), false),
        ("a_b".to_string(), false),
        (format!("a{}", "-b".repeat(19)), true),
        (format!("a{}", "-b".repeat(20)), false),
    ] {
        assert_eq!(Name::normalize(&input).is_ok(), ok, "{input:?}");
    }
}

#[test]
fn room() {
    /// A thread in which bob has added `n` comments, each `size` bytes of
    /// body, and alice only the open.
    fn spent(n: u32, size: usize) -> Thread {
        let mut b = Builder::default();
        let mut events = vec![b.open(Kind::Request)];
        for _ in 0..n {
            events.push(with(b.ev(&bob(), Action::Comment), &"x".repeat(size)));
        }
        Thread::replay(events).unwrap()
    }

    let mut b = Builder::default();
    let full_bodies = u32::try_from(cp_thread::MAX_THREAD_BODY / MAX_BODY as u64).unwrap();
    for (case, thread, e, ok, bob_all) in [
        (
            "room left",
            spent(10, 0),
            with(b.ev(&bob(), Action::Comment), "more"),
            true,
            true,
        ),
        (
            "events used",
            spent(MAX_EVENTS, 0),
            with(b.ev(&bob(), Action::Comment), "more"),
            false,
            false,
        ),
        (
            "events used, ending",
            spent(MAX_EVENTS, 0),
            b.ev(&bob(), Action::Decline),
            true,
            false,
        ),
        (
            "events used, ending with a body",
            spent(MAX_EVENTS, 0),
            with(b.ev(&bob(), Action::Decline), "why"),
            false,
            false,
        ),
        (
            "slack used",
            spent(MAX_EVENTS + ENDING_SLACK, 0),
            b.ev(&bob(), Action::Decline),
            false,
            false,
        ),
        (
            "bytes used",
            spent(full_bodies, MAX_BODY),
            with(b.ev(&bob(), Action::Comment), "x"),
            false,
            false,
        ),
        (
            "the other side's room is its own",
            spent(MAX_EVENTS, 0),
            with(b.ev(&alice(), Action::Comment), "hi"),
            true,
            false,
        ),
    ] {
        assert_eq!(thread.fold.room(&e).is_ok(), ok, "{case}");

        let mut fresh = thread.fold.clone();
        fresh.recipient_usage = cp_thread::Usage::default();
        assert_eq!(
            thread.fold.allowed(&bob()) == fresh.allowed(&bob()),
            bob_all,
            "{case}"
        );
    }
}

#[test]
fn ids() {
    let id = Id::from_random([0xff; 16]);
    assert_eq!(id.as_str(), "ffffffff-ffff-4fff-bfff-ffffffffffff");
    assert!(Id::try_from("FFFFFFFF-FFFF-4FFF-BFFF-FFFFFFFFFFFF").is_err());
}
