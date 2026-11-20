//! Two in-process daemons with a shared fake clock, and a network between
//! them that the test controls.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use std::cell::Cell;
use std::rc::Rc;

use cp_api::{
    Approval, DaemonUrl, Error, ErrorKind, NewThread, PeerInput, Peers, ThreadRef, ThreadSummary,
};
use cp_core::{Attempt, Config, Core, Millis, Outcome, Runtime};
use cp_sqlite::{Sqlite, run};
use cp_thread::{Action, Body, Name, Title};

/// A clock both daemons read; tests move it.
#[derive(Clone)]
pub struct Clock(pub Rc<Cell<i64>>);

impl Clock {
    pub fn new() -> Self {
        Self(Rc::new(Cell::new(1_791_331_200_000)))
    }

    pub fn now(&self) -> Millis {
        Millis(self.0.get())
    }

    pub fn advance(&self, ms: i64) {
        self.0.set(self.0.get() + ms);
    }
}

pub struct FakeRuntime {
    clock: Clock,
    state: u64,
}

impl Runtime for FakeRuntime {
    fn now(&self) -> Millis {
        self.clock.now()
    }

    fn random(&mut self, buf: &mut [u8]) {
        for b in buf {
            // xorshift64*: deterministic, so a failing case reproduces.
            self.state ^= self.state >> 12;
            self.state ^= self.state << 25;
            self.state ^= self.state >> 27;
            *b = (self.state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 56) as u8;
        }
    }
}

pub fn runtime(clock: &Clock, seed: u64) -> FakeRuntime {
    FakeRuntime {
        clock: clock.clone(),
        state: seed | 1,
    }
}

pub type C = Core<Sqlite, FakeRuntime>;

pub struct Daemon {
    pub core: C,
}

pub fn name(s: &str) -> Name {
    Name::try_from(s).unwrap()
}

pub fn url(s: &str) -> DaemonUrl {
    DaemonUrl::try_from(s).unwrap()
}

impl Daemon {
    pub fn new(who: &str, clock: &Clock, seed: u64) -> Self {
        let rt = runtime(clock, seed);
        let core = Core::open(
            Sqlite::memory().unwrap(),
            rt,
            Config {
                name: Some(name(who)),
                url: Some(url(&format!("https://{who}.test"))),
            },
        )
        .unwrap();
        Self { core }
    }

    pub fn call<T>(&mut self, f: impl FnOnce(&mut C) -> Result<T, Error>) -> Result<T, Error> {
        let r = run(&mut self.core, f);
        self.core.take_effects();
        r
    }

    pub fn send(&mut self, to: &str, title: &str) -> ThreadSummary {
        self.call(|c| {
            c.open_thread(NewThread {
                to: name(to),
                title: Title::try_from(title).unwrap(),
                kind: None,
                labels: Vec::new(),
                body: Body::try_from("details").unwrap(),
            })
        })
        .unwrap()
    }

    pub fn act(
        &mut self,
        thread: &str,
        action: Action,
        body: &str,
    ) -> Result<ThreadSummary, Error> {
        let r = ThreadRef::try_from(thread).unwrap();
        let body = Body::try_from(body).unwrap();
        self.call(|c| c.act(&r, None, action, body))
    }

    pub fn summary(&mut self, thread: &str) -> ThreadSummary {
        let r = ThreadRef::try_from(thread).unwrap();
        self.call(|c| c.thread(&r, None, &cp_api::GetThreadQuery::default()))
            .unwrap()
            .summary
    }

    pub fn thread(&mut self, thread: &str) -> cp_api::Thread {
        let r = ThreadRef::try_from(thread).unwrap();
        self.call(|c| {
            c.thread(
                &r,
                None,
                &cp_api::GetThreadQuery {
                    limit: Some(1000),
                    before: None,
                },
            )
        })
        .unwrap()
    }
}

/// The status a peer's daemon answers an error with.
pub fn outcome<T>(r: &Result<T, Error>) -> Outcome {
    match r {
        Ok(_) => Outcome::Delivered,
        Err(e) => Outcome::from_status(e.kind.status(), e.detail.clone()),
    }
}

/// Makes `a` and `b` peers: `a` asks, `b` approves after comparing codes.
pub fn peer(a: &mut Daemon, b: &mut Daemon) {
    let a_name = a.core.me().clone();
    let b_name = b.core.me().clone();
    let out = a
        .call(|c| {
            c.add_peer(PeerInput {
                name: b_name.clone(),
                url: url(&format!("https://{b_name}.test")),
                note: None,
            })
        })
        .unwrap();
    let receipt = b
        .call(|c| c.request_received(out.request.as_ref().unwrap()))
        .unwrap();
    assert_eq!(
        receipt.code,
        a.call(|c| c.peer(&b_name)).unwrap().code,
        "codes differ"
    );

    let id = b
        .call(|c| c.requests())
        .unwrap()
        .requests
        .first()
        .unwrap()
        .id
        .clone();
    let pending = b
        .call(|c| c.begin_approval(&id, Approval::default()))
        .unwrap();
    assert_eq!(pending.name, a_name);
    a.call(|c| c.accepted(pending.call.secret.expose()))
        .unwrap();
    b.call(|c| c.commit_approval(&pending)).unwrap();
}

/// Attempts every delivery `from` has due to `to`, until none is left or
/// `to` refuses one for now. Returns what each attempt did.
pub fn deliver(from: &mut Daemon, to: &mut Daemon) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    loop {
        let due = from.call(|c| c.due(40)).unwrap();
        let due: Vec<Attempt> = due
            .into_iter()
            .filter(|a| a.peer == *to.core.me())
            .collect();
        if due.is_empty() {
            return outcomes;
        }
        let mut progressed = false;
        for a in due {
            if !from.call(|c| c.still_pending(&a)).unwrap() {
                continue;
            }
            let secret = a.secret.expose().to_string();
            let event = a.event.clone();
            let result = to.call(|c| c.receive(&secret, event));
            let o = outcome(&result);
            progressed |= matches!(o, Outcome::Delivered | Outcome::Refused(_));
            from.call(|c| c.record(&a, o.clone())).unwrap();
            outcomes.push(o);
        }
        if !progressed {
            return outcomes;
        }
    }
}

/// Delivers in both directions until nothing is left to send.
pub fn settle(a: &mut Daemon, b: &mut Daemon) {
    for _ in 0..100 {
        let x = deliver(a, b);
        let y = deliver(b, a);
        if x.is_empty() && y.is_empty() {
            return;
        }
    }
    panic!("delivery did not settle");
}

pub fn kind<T: std::fmt::Debug>(r: &Result<T, Error>) -> ErrorKind {
    r.as_ref().expect_err("wanted an error").kind
}

pub fn scope(peers: &[&str]) -> Peers {
    Peers::try_from(peers.iter().map(|p| name(p)).collect::<Vec<_>>()).unwrap()
}
