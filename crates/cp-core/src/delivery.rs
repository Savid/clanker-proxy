//! Delivering the owner's events to peers: which are due, and what each
//! attempt's outcome does. The host makes the requests.
//!
//! Each peer receives its events in order: only a peer's oldest pending
//! event is ever due, and a later one waits behind it. A peer that is down
//! is retried until it is back, however long that takes, so the two sides
//! of a thread never silently diverge; only a peer that no longer knows
//! this daemon, or never approved it, is given up on.

use cp_api::{DaemonUrl, Error, ErrorKind, PeerSecret, WireEvent};
use cp_thread::{Id, Name};

use crate::core::Core;
use crate::peering::REQUEST_TTL;
use crate::runtime::{Millis, Runtime};
use crate::sql::{Row, Sql, text};

/// The first retry's wait; it doubles from there.
pub const MIN_BACKOFF: i64 = 15 * Millis::SECOND;
/// The longest wait between retries.
pub const MAX_BACKOFF: i64 = Millis::HOUR;
/// How long a peer may answer 401 (it no longer knows this daemon) before
/// its events are given up on.
pub const GIVE_UP_401: i64 = 7 * Millis::DAY;

/// The wait after a failed attempt, given the attempts before it.
pub fn backoff(attempts: u32) -> i64 {
    let mut wait = MIN_BACKOFF;
    for _ in 0..attempts {
        if wait >= MAX_BACKOFF / 2 {
            return MAX_BACKOFF;
        }
        wait *= 2;
    }
    wait
}

/// One event to send to a peer.
#[derive(Clone, Debug)]
pub struct Attempt {
    pub event: WireEvent,
    pub peer: Name,
    pub url: DaemonUrl,
    pub secret: PeerSecret,
    /// Attempts made before this one; [`Core::record`] applies only while
    /// this still matches, so a stale attempt changes nothing.
    pub attempts: u32,
}

/// What became of an attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The peer stored it.
    Delivered,
    /// The peer refused it for good (400, 409, 413, 422).
    Refused(String),
    /// The peer does not know this daemon's secret (401).
    Unrecognized(String),
    /// Anything else: the network, the peer's or a proxy's error, a
    /// redirect. Retried.
    Failed(String),
}

impl Outcome {
    /// The outcome an HTTP status reports.
    pub fn from_status(status: u16, detail: String) -> Self {
        match status {
            200..=299 => Outcome::Delivered,
            400 | 409 | 413 | 422 => Outcome::Refused(detail),
            401 => Outcome::Unrecognized(detail),
            _ => Outcome::Failed(detail),
        }
    }
}

/// What becomes of a queued event after an attempt.
enum Verdict {
    Delivered,
    Failed(String),
    Retry(String, Millis),
}

/// Longest stored failure text: it comes from the peer and reaches the
/// owner's agent.
const MAX_ERROR: usize = 300;

/// Keeps a peer-influenced message short and printable.
fn clip(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_control() || cp_thread::is_hidden(c) {
                ' '
            } else {
                c
            }
        })
        .take(MAX_ERROR)
        .collect();
    if s.chars().count() > MAX_ERROR {
        out.push('…');
    }
    out
}

impl<S: Sql, R: Runtime> Core<S, R> {
    /// Each peer's oldest pending event, if it is due, at most `budget` of
    /// them.
    pub fn due(&self, budget: u32) -> Result<Vec<Attempt>, Error> {
        let rows = self.sql.query(
            "SELECT o.event_id, o.peer, p.url, p.secret, o.attempts FROM outbox o \
             JOIN peers p ON p.name = o.peer \
             WHERE o.status = 'pending' AND o.next_attempt_at <= ?1 \
             AND o.seq = (SELECT min(seq) FROM outbox h WHERE h.peer = o.peer AND h.status = 'pending') \
             ORDER BY o.seq LIMIT ?2",
            &[self.now().0.into(), budget.into()],
        )?;
        rows.iter().map(|r| self.attempt(r)).collect()
    }

    fn attempt(&self, r: &Row) -> Result<Attempt, Error> {
        let id: Id = r.parse(0)?;
        let stored = self
            .sql
            .query_one(
                "SELECT id, thread, author, recipient, at, clock, action, title, kind, labels, body FROM events WHERE id = ?",
                &[text(&id)],
            )?
            .ok_or_else(|| Error::new(ErrorKind::Internal, format!("queued event {id} is missing")))?;
        let event = crate::threads::event_from_row(&stored)?;
        Ok(Attempt {
            event: WireEvent::from(&event),
            peer: r.parse(1)?,
            url: r.parse(2)?,
            secret: PeerSecret::new(r.text(3)?.to_string())
                .map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))?,
            attempts: r.count(4)?,
        })
    }

    /// Whether `a` should still be sent: its event is pending with the same
    /// attempts, to the same peer at the same URL and secret. The host asks
    /// just before sending, since anything may have changed since `due`.
    pub fn still_pending(&self, a: &Attempt) -> Result<bool, Error> {
        Ok(self
            .sql
            .query_one(
                "SELECT 1 FROM outbox o JOIN peers p ON p.name = o.peer \
                 WHERE o.event_id = ? AND o.status = 'pending' AND o.attempts = ? AND p.name = ? AND p.url = ? AND p.secret = ?",
                &[text(&a.event.id), a.attempts.into(), text(&a.peer), text(a.url.as_str()), a.secret.expose().into()],
            )?
            .is_some())
    }

    /// What an attempt's outcome means for its event, given its peer.
    fn verdict(&mut self, a: &Attempt, outcome: Outcome) -> Result<Option<Verdict>, Error> {
        let now = self.now();
        let Some(peer) = self.sql.query_one(
            "SELECT status, added_at, first_401_at FROM peers WHERE name = ?",
            &[text(&a.peer)],
        )?
        else {
            return Ok(None); // removed meanwhile, which failed its queue
        };
        let requested = peer.text(0)? == "requested";
        let never_approved =
            || Verdict::Failed(format!("gave up: {} never approved this daemon", a.peer));
        let retry = |e: &str| Verdict::Retry(clip(e), now.plus(backoff(a.attempts)));

        Ok(Some(match outcome {
            Outcome::Delivered => {
                self.sql.execute(
                    "UPDATE peers SET first_401_at = NULL WHERE name = ?",
                    &[text(&a.peer)],
                )?;
                Verdict::Delivered
            }
            Outcome::Refused(e) => Verdict::Failed(clip(&e)),
            _ if requested && now.0 - peer.int(1)? >= REQUEST_TTL => never_approved(),
            Outcome::Unrecognized(e) => {
                let first = peer.opt_int(2)?.map_or(now, Millis);
                if first == now {
                    self.sql.execute(
                        "UPDATE peers SET first_401_at = ? WHERE name = ?",
                        &[now.0.into(), text(&a.peer)],
                    )?;
                }
                if !requested && now.0 - first.0 >= GIVE_UP_401 {
                    Verdict::Failed(format!(
                        "gave up: {} has not recognised this daemon for 7 days: {}",
                        a.peer,
                        clip(&e)
                    ))
                } else {
                    retry(&e)
                }
            }
            Outcome::Failed(e) => retry(&e),
        }))
    }

    /// Records what became of an attempt. Does nothing if the event moved on
    /// meanwhile (delivered, failed, or retried by another attempt).
    pub fn record(&mut self, a: &Attempt, outcome: Outcome) -> Result<(), Error> {
        let Some(verdict) = self.verdict(a, outcome)? else {
            return Ok(());
        };
        let now = self.now();
        let (status, error, next, counters) = match &verdict {
            Verdict::Delivered => (
                "delivered",
                None,
                None,
                "undelivered = max(undelivered - 1, 0)",
            ),
            Verdict::Failed(e) => (
                "failed",
                Some(e.clone()),
                None,
                "undelivered = max(undelivered - 1, 0), failed = failed + 1",
            ),
            Verdict::Retry(e, at) => ("pending", Some(e.clone()), Some(at.0), ""),
        };
        let changed = self.sql.execute(
            "UPDATE outbox SET status = ?1, attempts = attempts + 1, last_error = ?2, \
             next_attempt_at = ?3, \
             delivered_at = CASE WHEN ?1 = 'delivered' THEN ?4 ELSE delivered_at END \
             WHERE event_id = ?5 AND status = 'pending' AND attempts = ?6",
            &[
                status.into(),
                error.into(),
                next.into(),
                now.0.into(),
                text(&a.event.id),
                a.attempts.into(),
            ],
        )?;
        if changed == 0 {
            return Ok(());
        }
        if !counters.is_empty() {
            self.sql.execute(
                &format!("UPDATE threads SET {counters} WHERE id = ?"),
                &[text(&a.event.thread)],
            )?;
            // A delivery or a final failure lets the peer's next event go.
            self.effects.deliver = true;
        }
        self.effects.changed(&a.event.thread);
        Ok(())
    }

    /// When the next delivery is due, if any is pending: the earliest of
    /// each peer's oldest pending event.
    pub fn next_due(&self) -> Result<Option<Millis>, Error> {
        Ok(self
            .sql
            .query_one(
                "SELECT min(o.next_attempt_at) FROM outbox o JOIN peers p ON p.name = o.peer \
                 WHERE o.status = 'pending' \
                 AND o.seq = (SELECT min(seq) FROM outbox h WHERE h.peer = o.peer AND h.status = 'pending')",
                &[],
            )?
            .and_then(|r| r.opt_int(0).ok().flatten())
            .map(Millis))
    }
}
