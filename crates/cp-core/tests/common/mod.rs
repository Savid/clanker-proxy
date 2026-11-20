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
    Approval, DaemonUrl, Error, ErrorKind, NewThread, PeerInput, PeeringRequest, Peers, ThreadRef,
    ThreadSummary,
};
use cp_core::{Attempt, Config, Core, EVENTS_PATH, Millis, Outcome, Runtime, SignedCall};
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
        // Distinct seeds give distinct streams, and so distinct keys.
        state: (seed << 1) | 1,
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

/// `a` asks `b` to peer, through the real request: afterwards `a` has `b`
/// as a requested peer and `b` has the request.
pub fn ask(a: &mut Daemon, b: &mut Daemon) -> cp_api::Peer {
    let b_name = b.core.me().clone();
    let call = a
        .call(|c| {
            c.add_peer(PeerInput {
                name: b_name.clone(),
                url: url(&format!("https://{b_name}.test")),
                note: None,
            })
        })
        .unwrap();
    let request: PeeringRequest = serde_json::from_slice(&call.body).unwrap();
    let receipt = b.call(|c| c.request_received(&request)).unwrap();
    let asked = a
        .call(|c| c.finish_add_peer(&b_name, &call.url, &receipt))
        .unwrap();
    assert_eq!(receipt.code, asked.code, "codes differ");
    asked
}

/// Makes `a` and `b` peers: `a` asks, `b` approves after comparing codes.
pub fn peer(a: &mut Daemon, b: &mut Daemon) {
    let a_name = a.core.me().clone();
    ask(a, b);

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
    let accepted = signed(&pending.call.headers, pending.call.path, &pending.call.body);
    a.call(|c| {
        c.authenticate_peer(&accepted)?;
        c.accepted(&accepted.key)
    })
    .unwrap();
    b.call(|c| c.commit_approval(&pending)).unwrap();
}

/// The signed call a request with these headers makes, as the receiving
/// host reads it.
pub fn signed(headers: &[(String, String)], path: &str, body: &[u8]) -> SignedCall {
    let header = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.as_str())
    };
    SignedCall::new(header, "POST", path, body).unwrap()
}

/// What the receiving daemon's host does with a delivery: check the
/// signature, then store the event.
pub fn receive(to: &mut Daemon, a: &Attempt) -> Result<cp_api::Receipt, Error> {
    let call = signed(&a.headers, EVENTS_PATH, &a.body);
    let event = a.event.clone();
    to.call(|c| {
        c.authenticate_peer(&call)?;
        c.receive(&call.key, event)
    })
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
            let result = receive(to, &a);
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
