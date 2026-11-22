//! Threads: the owner opening and acting on them, peers' events arriving,
//! and reading and listing them.

use cp_api::{
    DEFAULT_LOG, DEFAULT_THREADS, Delivery, DeliveryStatus, Error, ErrorKind, GetThreadQuery,
    ListThreadsQuery, MAX_LOG, MAX_THREADS, NewThread, Peers, PublicKey, Receipt, Thread,
    ThreadEvent, ThreadRef, ThreadSummary, TurnFilter, WireEvent,
};
use cp_thread::{
    Action, At, Body, Event, Fold, Id, Kind, Labels, Move, Name, Opening, Refusal, Role, Step,
    Title, Usage,
};

use crate::core::{Core, scope_json};
use crate::runtime::{Millis, Runtime};
use crate::sql::{Row, Sql, Value, text};
use crate::webhooks::Notice;
use cp_api::{WebhookEventOrigin, WebhookEventType};

/// How far ahead of this daemon's clock a peer's event may be dated.
const MAX_FUTURE_SKEW: i64 = 10 * Millis::MINUTE;

/// A thread's stored row: what it lists as, and its folded state.
#[derive(Clone, Debug)]
pub(crate) struct ThreadRow {
    pub fold: Fold,
    pub title: Title,
    pub labels: Labels,
    pub peer: Name,
    pub events: u32,
    /// Who made the latest move to arrive that changed something.
    pub last_from: Name,
    pub undelivered: u32,
    pub failed: u32,
    pub overruled: u32,
}

const THREAD_COLUMNS: &str = "id, title, kind, labels, sender, recipient, peer, state, \
    opened_at, updated_at, events, last_from, undelivered, failed, overruled, last_clock, \
    last_id, max_clock, sender_events, sender_bytes, sender_clock, recipient_events, \
    recipient_bytes, recipient_clock";

fn seconds(s: i64) -> At {
    At::from_unix_seconds(s).unwrap_or(At::EPOCH)
}

fn internal(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Internal, detail)
}

fn thread_row(r: &Row) -> Result<ThreadRow, Error> {
    let usage = |e, b, c| -> Result<Usage, Error> {
        Ok(Usage {
            events: r.count(e)?,
            body_bytes: r.uint(b)?,
            max_clock: r.uint(c)?,
        })
    };
    let fold = Fold {
        id: r.parse(0)?,
        kind: r.name(2)?,
        sender: r.parse(4)?,
        recipient: r.parse(5)?,
        state: r.name(7)?,
        opened_at: seconds(r.int(8)?),
        updated_at: seconds(r.int(9)?),
        last_clock: r.uint(15)?,
        last_id: r.parse(16)?,
        max_clock: r.uint(17)?,
        sender_usage: usage(18, 19, 20)?,
        recipient_usage: usage(21, 22, 23)?,
    };
    Ok(ThreadRow {
        fold,
        title: r.parse(1)?,
        labels: serde_json::from_str(r.text(3)?).map_err(|_| internal("stored labels"))?,
        peer: r.parse(6)?,
        events: r.count(10)?,
        last_from: r.parse(11)?,
        undelivered: r.count(12)?,
        failed: r.count(13)?,
        overruled: r.count(14)?,
    })
}

/// An event as stored: the columns `event_from_row` reads, in order.
const EVENT_COLUMNS: &str =
    "id, thread, author, recipient, at, clock, action, title, kind, labels, body";

pub(crate) fn event_from_row(r: &Row) -> Result<Event, Error> {
    let action: Action = r.name(6)?;
    let opening = if action == Action::Open {
        Some(Opening {
            title: r.parse(7)?,
            kind: r.name(8)?,
            labels: serde_json::from_str(r.opt_text(9)?.unwrap_or("[]"))
                .map_err(|_| internal("stored labels"))?,
        })
    } else {
        None
    };
    Ok(Event {
        id: r.parse(0)?,
        thread: r.parse(1)?,
        from: r.parse(2)?,
        to: r.parse(3)?,
        at: seconds(r.int(4)?),
        clock: r.uint(5)?,
        action,
        opening,
        body: r.parse(10)?,
    })
}

fn labels_json(labels: &Labels) -> String {
    serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string())
}

/// The order both daemons replay a thread in, for SQL.
const REPLAY_ORDER: &str = "action = 'open' DESC, clock, id";

impl<S: Sql, R: Runtime> Core<S, R> {
    pub(crate) fn load_thread(&self, id: &Id) -> Result<Option<ThreadRow>, Error> {
        self.sql
            .query_one(
                &format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id = ?"),
                &[text(id)],
            )?
            .map(|r| thread_row(&r))
            .transpose()
    }

    pub(crate) fn save_thread(&mut self, row: &ThreadRow) -> Result<(), Error> {
        let f = &row.fold;
        let values: Vec<Value> = vec![
            text(&f.id),
            text(&row.title),
            f.kind.as_str().into(),
            labels_json(&row.labels).into(),
            text(&f.sender),
            text(&f.recipient),
            text(&row.peer),
            f.state.as_str().into(),
            f.turn().map(|t| t.as_str().to_string()).into(),
            f.opened_at.as_unix_seconds().into(),
            f.updated_at.as_unix_seconds().into(),
            row.events.into(),
            text(&row.last_from),
            row.undelivered.into(),
            row.failed.into(),
            row.overruled.into(),
            f.last_clock.into(),
            text(&f.last_id),
            f.max_clock.into(),
            f.sender_usage.events.into(),
            f.sender_usage.body_bytes.into(),
            f.sender_usage.max_clock.into(),
            f.recipient_usage.events.into(),
            f.recipient_usage.body_bytes.into(),
            f.recipient_usage.max_clock.into(),
        ];
        self.sql.execute(
            "INSERT OR REPLACE INTO threads (id, title, kind, labels, sender, recipient, peer, \
             state, turn, opened_at, updated_at, events, last_from, undelivered, failed, \
             overruled, last_clock, last_id, max_clock, sender_events, sender_bytes, \
             sender_clock, recipient_events, recipient_bytes, recipient_clock) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &values,
        )?;
        self.effects.changed(&f.id);
        Ok(())
    }

    fn insert_event(&self, e: &Event, ignored: Option<&Refusal>) -> Result<(), Error> {
        let (title, kind, labels) = match &e.opening {
            Some(o) => (
                Some(o.title.as_str().to_string()),
                Some(o.kind.as_str()),
                Some(labels_json(&o.labels)),
            ),
            None => (None, None, None),
        };
        self.sql.execute(
            "INSERT INTO events (id, thread, author, recipient, at, clock, action, title, kind, \
             labels, body, body_bytes, body_blank, ignored, stored_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                text(&e.id),
                text(&e.thread),
                text(&e.from),
                text(&e.to),
                e.at.as_unix_seconds().into(),
                e.clock.into(),
                e.action.as_str().into(),
                title.into(),
                kind.into(),
                labels.into(),
                text(&e.body),
                e.body.bytes().into(),
                e.body.is_blank().into(),
                ignored.map(ToString::to_string).into(),
                self.now().0.into(),
            ],
        )?;
        Ok(())
    }

    /// Queues the owner's event for delivery to its recipient.
    fn queue(&mut self, e: &Event) -> Result<(), Error> {
        self.sql.execute(
            "INSERT INTO outbox (event_id, thread, peer, status, next_attempt_at) \
             VALUES (?, ?, ?, 'pending', ?)",
            &[
                text(&e.id),
                text(&e.thread),
                text(&e.to),
                self.now().0.into(),
            ],
        )?;
        self.effects.deliver = true;
        Ok(())
    }

    pub(crate) fn peer_exists(&self, name: &Name) -> Result<bool, Error> {
        Ok(self
            .sql
            .query_one("SELECT 1 FROM peers WHERE name = ?", &[text(name)])?
            .is_some())
    }

    /// The summary the API shows for a stored thread.
    pub(crate) fn summary_of(&self, row: &ThreadRow) -> Result<ThreadSummary, Error> {
        let f = &row.fold;
        let role: Role = f
            .role_of(&self.me)
            .ok_or_else(|| internal(format!("thread {} is not the owner's", f.id)))?;
        let turn = f.turn().cloned();
        Ok(ThreadSummary {
            id: f.id.clone(),
            title: row.title.clone(),
            kind: f.kind,
            labels: row.labels.clone(),
            sender: f.sender.clone(),
            recipient: f.recipient.clone(),
            peer: row.peer.clone(),
            role,
            state: f.state,
            my_turn: turn.as_ref() == Some(&self.me),
            turn,
            last_from: row.last_from.clone(),
            actions: f.allowed(&self.me),
            opened_at: f.opened_at,
            updated_at: f.updated_at,
            events: row.events,
            undelivered: row.undelivered,
            failed: row.failed,
            overruled: row.overruled,
        })
    }

    /// Notifies webhooks of `e` on the thread as it now stands.
    fn notify_event(
        &mut self,
        row: &ThreadRow,
        e: &Event,
        origin: WebhookEventOrigin,
    ) -> Result<(), Error> {
        let n = Notice {
            kind: WebhookEventType::for_action(e.action),
            origin,
            subject: row.fold.id.to_string(),
            event_id: Some(e.id.clone()),
            peer: row.peer.clone(),
            state: Some(row.fold.state),
            my_turn: Some(row.fold.turn() == Some(&self.me)),
        };
        self.notify(&n)
    }

    /// The summary of the thread `id`, for streams.
    pub fn summary(&self, id: &Id) -> Result<Option<ThreadSummary>, Error> {
        self.load_thread(id)?
            .map(|row| self.summary_of(&row))
            .transpose()
    }

    /// The ID of the thread `r` names among threads with peers in `scope`.
    pub(crate) fn resolve(&self, r: &ThreadRef, scope: Option<&Peers>) -> Result<Id, Error> {
        let rows = self.sql.query(
            "SELECT id FROM threads WHERE id LIKE ?1 \
             AND (?2 IS NULL OR peer IN (SELECT value FROM json_each(?2))) ORDER BY id LIMIT 2",
            &[format!("{}%", r.as_str()).into(), scope_json(scope)],
        )?;
        match rows.as_slice() {
            [] => Err(Error::new(
                ErrorKind::NotFound,
                format!("no thread matches {:?}", r.as_str()),
            )),
            [one] => one.parse(0),
            _ => Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "{:?} matches more than one thread; give more of its ID",
                    r.as_str()
                ),
            )),
        }
    }

    /// Opens a thread with a peer in `scope`, named by its name or an
    /// alias, and queues it for delivery.
    pub fn open_thread(
        &mut self,
        n: NewThread,
        scope: Option<&Peers>,
    ) -> Result<ThreadSummary, Error> {
        let to = self.resolve_peer(&n.to, scope)?;
        if !scope.is_none_or(|s| s.reaches(&to)) || !self.peer_exists(&to)? {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("{} is not a peer", n.to),
            ));
        }
        let labels =
            Labels::sorted(n.labels).map_err(|e| Error::new(ErrorKind::Invalid, e.to_string()))?;
        let id = self.new_id();
        let e = Event {
            id: id.clone(),
            thread: id,
            from: self.me.clone(),
            to: to.clone(),
            at: self.now().at(),
            clock: 1,
            action: Action::Open,
            opening: Some(Opening {
                title: n.title.clone(),
                kind: n.kind.unwrap_or(Kind::Request),
                labels: labels.clone(),
            }),
            body: n.body,
        };
        e.validate()
            .map_err(|err| Error::new(ErrorKind::Invalid, err.to_string()))?;
        let fold = Fold::open(&e).ok_or_else(|| internal("open event did not open"))?;
        self.insert_event(&e, None)?;
        self.queue(&e)?;
        let row = ThreadRow {
            fold,
            title: n.title,
            labels,
            peer: to,
            events: 1,
            last_from: self.me.clone(),
            undelivered: 1,
            failed: 0,
            overruled: 0,
        };
        self.save_thread(&row)?;
        self.notify_event(&row, &e, WebhookEventOrigin::Outgoing)?;
        self.summary_of(&row)
    }

    /// Refuses a thread whose open its peer will never get: anything more
    /// sent on it would be refused as a thread they do not hold.
    fn check_delivered(&self, row: &ThreadRow) -> Result<(), Error> {
        let failed = self.sql.query_one(
            "SELECT last_error FROM outbox WHERE event_id = ? AND status = 'failed'",
            &[text(&row.fold.id)],
        )?;
        match failed {
            Some(r) => Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "{} never received this thread ({:?}); send a new one",
                    row.peer,
                    r.opt_text(0)?.unwrap_or("refused")
                ),
            )),
            None => Ok(()),
        }
    }

    /// The owner acts on the thread `r` names, among threads with peers in
    /// `scope`.
    pub fn act(
        &mut self,
        r: &ThreadRef,
        scope: Option<&Peers>,
        action: Action,
        body: Body,
    ) -> Result<ThreadSummary, Error> {
        if action == Action::Open {
            return Err(Error::new(
                ErrorKind::Invalid,
                "open a thread by opening it, not as an action",
            ));
        }
        let id = self.resolve(r, scope)?;
        let mut row = self
            .load_thread(&id)?
            .ok_or_else(|| internal("resolved thread is gone"))?;
        if !self.peer_exists(&row.peer)? {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("{} is not a peer", row.peer),
            ));
        }
        self.check_delivered(&row)?;

        let e = Event {
            id: self.new_id(),
            thread: id,
            from: self.me.clone(),
            to: row.peer.clone(),
            at: self.now().at(),
            clock: row.fold.next_clock(),
            action,
            opening: None,
            body,
        };
        e.validate()
            .map_err(|err| Error::new(ErrorKind::Invalid, err.to_string()))?;
        row.fold.room(&e).map_err(|err| {
            Error::new(
                ErrorKind::Conflict,
                format!("{err}; open a new thread to go on"),
            )
        })?;
        match row.fold.check(&e) {
            Ok(()) => {}
            Err(Refusal::NoReason) => {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    Refusal::NoReason.to_string(),
                ));
            }
            Err(err) => {
                let allowed: Vec<&str> = row
                    .fold
                    .allowed(&self.me)
                    .iter()
                    .map(|a| a.as_str())
                    .collect();
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!("{err}; you can now {}", allowed.join(", ")),
                ));
            }
        }
        row.fold.step(&e);
        self.insert_event(&e, None)?;
        self.queue(&e)?;
        row.events = row.events.saturating_add(1);
        row.last_from = self.me.clone();
        row.undelivered = row.undelivered.saturating_add(1);
        self.save_thread(&row)?;
        self.notify_event(&row, &e, WebhookEventOrigin::Outgoing)?;
        self.summary_of(&row)
    }

    /// Stores an event the peer with `key` delivered. The host has checked
    /// the call's signature; the key is looked up again here, in the same
    /// transaction, so a peer removed since the request began cannot get an
    /// event in.
    pub fn receive(&mut self, key: &PublicKey, wire: WireEvent) -> Result<Receipt, Error> {
        let peer = self
            .peer_with_key(key)?
            .ok_or_else(|| Error::new(ErrorKind::Unauthorized, "unknown peer key"))?;
        let e = wire
            .into_event(peer.clone(), self.me.clone())
            .map_err(|err| Error::new(ErrorKind::Invalid, err.to_string()))?;
        if Millis::from_at(e.at) > self.now().plus(MAX_FUTURE_SKEW) {
            return Err(Error::new(
                ErrorKind::Unavailable,
                format!(
                    "event time {} is ahead of this daemon's clock; retry later",
                    e.at
                ),
            )
            .retry_after(60));
        }
        let id = e.id.clone();
        if let Some(stored) = self.sql.query_one(
            &format!("SELECT {EVENT_COLUMNS} FROM events WHERE id = ?"),
            &[text(&id)],
        )? {
            return if event_from_row(&stored)? == e {
                Ok(Receipt {
                    id,
                    duplicate: true,
                })
            } else {
                Err(Error::new(
                    ErrorKind::Conflict,
                    format!("event {id} already exists with different content"),
                ))
            };
        }

        if e.action == Action::Open {
            self.receive_open(&e)?;
        } else {
            self.receive_move(&e)?;
        }
        self.peer_heard_from(&peer)?;
        Ok(Receipt {
            id,
            duplicate: false,
        })
    }

    fn receive_open(&mut self, e: &Event) -> Result<(), Error> {
        if self.load_thread(&e.thread)?.is_some() {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("thread {} already exists", e.thread),
            ));
        }
        let (Some(fold), Some(opening)) = (Fold::open(e), &e.opening) else {
            return Err(Error::new(
                ErrorKind::Invalid,
                "open needs a title and a kind",
            ));
        };
        self.insert_event(e, None)?;
        let row = ThreadRow {
            fold,
            title: opening.title.clone(),
            labels: opening.labels.clone(),
            peer: e.from.clone(),
            events: 1,
            last_from: e.from.clone(),
            undelivered: 0,
            failed: 0,
            overruled: 0,
        };
        self.save_thread(&row)?;
        self.notify_event(&row, e, WebhookEventOrigin::Incoming)
    }

    fn receive_move(&mut self, e: &Event) -> Result<(), Error> {
        let unprocessable = |detail: String| Error::new(ErrorKind::Unprocessable, detail);
        let mut row = self
            .load_thread(&e.thread)?
            .ok_or_else(|| unprocessable(format!("unknown thread {}", e.thread)))?;
        if row.fold.role_of(&e.from).is_none() || *row.fold.peer(&e.from) != e.to {
            return Err(unprocessable(format!(
                "thread {} is not between {} and {}",
                e.thread, e.from, e.to
            )));
        }
        row.fold
            .room(e)
            .map_err(|err| unprocessable(err.to_string()))?;
        row.fold
            .check_clock(e)
            .map_err(|err| unprocessable(err.to_string()))?;

        let mut overruled = Vec::new();
        let quiet = if row.fold.follows(e) {
            let refusal = row.fold.step(e);
            self.insert_event(e, refusal.as_ref())?;
            refusal.is_some()
        } else {
            // A concurrent move lands before events already held: replay,
            // since it can change what they did.
            self.insert_event(e, None)?;
            overruled = self.replay(&mut row)?;
            self.sql
                .query_one(
                    "SELECT ignored IS NOT NULL FROM events WHERE id = ?",
                    &[text(&e.id)],
                )?
                .map(|r| r.flag(0))
                .transpose()?
                .unwrap_or(false)
        };
        row.events = row.events.saturating_add(1);
        if !quiet {
            row.last_from = e.from.clone();
        }
        self.save_thread(&row)?;
        // A move that changed nothing answers nothing, so no webhook hears
        // of it.
        if !quiet {
            self.notify_event(&row, e, WebhookEventOrigin::Incoming)?;
        }
        for id in overruled {
            let n = Notice {
                kind: WebhookEventType::ThreadOverruled,
                origin: WebhookEventOrigin::Incoming,
                subject: row.fold.id.to_string(),
                event_id: Some(id),
                peer: row.peer.clone(),
                state: Some(row.fold.state),
                my_turn: Some(row.fold.turn() == Some(&self.me)),
            };
            self.notify(&n)?;
        }
        Ok(())
    }

    /// Replays a thread from its stored events, without their bodies, and
    /// records what each event did. Returns the owner's events that this
    /// replay overruled.
    pub(crate) fn replay(&mut self, row: &mut ThreadRow) -> Result<Vec<Id>, Error> {
        let rows = self.sql.query(
            &format!(
                "SELECT id, thread, author, recipient, at, clock, action, body_bytes, body_blank, \
                 ignored FROM events WHERE thread = ? ORDER BY {REPLAY_ORDER}"
            ),
            &[text(&row.fold.id)],
        )?;
        let mut steps = Vec::with_capacity(rows.len());
        for r in &rows {
            let step = Step {
                id: r.parse(0)?,
                thread: r.parse(1)?,
                from: r.parse(2)?,
                to: r.parse(3)?,
                at: seconds(r.int(4)?),
                clock: r.uint(5)?,
                action: r.name(6)?,
                body_bytes: r.uint(7)?,
                body_blank: r.flag(8)?,
            };
            steps.push((step, r.opt_text(9)?.map(str::to_string)));
        }
        let mut it = steps.into_iter();
        let (open, _) = it.next().ok_or_else(|| internal("thread has no events"))?;
        let mut fold = Fold::start(&open, row.fold.kind)
            .ok_or_else(|| internal("thread does not start with its open"))?;

        let mut overruled = Vec::new();
        let mut owner_ignored = 0u32;
        for (step, was) in it {
            let now = fold.step(&step).map(|r| r.to_string());
            if now != was {
                self.sql.execute(
                    "UPDATE events SET ignored = ? WHERE id = ?",
                    &[now.clone().into(), text(step.id())],
                )?;
                if *step.from() == self.me && was.is_none() {
                    overruled.push(step.id().clone());
                }
            }
            if *step.from() == self.me && now.is_some() {
                owner_ignored = owner_ignored.saturating_add(1);
            }
        }
        row.fold = fold;
        row.overruled = owner_ignored;
        Ok(overruled)
    }

    /// A thread and a page of its events, among threads with peers in
    /// `scope`.
    pub fn thread(
        &self,
        r: &ThreadRef,
        scope: Option<&Peers>,
        q: &GetThreadQuery,
    ) -> Result<Thread, Error> {
        let id = self.resolve(r, scope)?;
        let row = self
            .load_thread(&id)?
            .ok_or_else(|| internal("resolved thread is gone"))?;
        let before = q.before.unwrap_or(row.events).min(row.events);
        let limit = q.limit.unwrap_or(DEFAULT_LOG).clamp(1, MAX_LOG);
        let start = before.saturating_sub(limit);
        let rows = self.sql.query(
            &format!(
                "SELECT e.id, e.author, e.at, e.action, e.body, e.ignored, o.status, o.attempts, \
                 o.last_error, o.next_attempt_at, o.delivered_at FROM events e \
                 LEFT JOIN outbox o ON o.event_id = e.id WHERE e.thread = ? \
                 ORDER BY e.{REPLAY_ORDER} LIMIT ? OFFSET ?"
            )
            .replace(
                "e.action = 'open' DESC, clock, id",
                "e.action = 'open' DESC, e.clock, e.id",
            ),
            &[text(&id), (before - start).into(), start.into()],
        )?;
        let mut log = Vec::with_capacity(rows.len());
        for r in &rows {
            let delivery = match r.opt_text(6)? {
                None => None,
                Some(status) => Some(Delivery {
                    status: match status {
                        "delivered" => DeliveryStatus::Delivered,
                        "failed" => DeliveryStatus::Failed,
                        _ => DeliveryStatus::Pending,
                    },
                    attempts: r.count(7)?,
                    last_error: r.opt_text(8)?.map(str::to_string),
                    next_attempt_at: r
                        .opt_int(9)?
                        .filter(|_| status == "pending")
                        .map(|t| Millis(t).at()),
                    delivered_at: r.opt_int(10)?.map(|t| Millis(t).at()),
                }),
            };
            log.push(ThreadEvent {
                id: r.parse(0)?,
                from: r.parse(1)?,
                at: seconds(r.int(2)?),
                action: r.name(3)?,
                body: r.parse(4)?,
                ignored: r.opt_text(5)?.map(str::to_string),
                delivery,
            });
        }
        Ok(Thread {
            summary: self.summary_of(&row)?,
            log,
            log_start: start,
        })
    }

    /// Threads, newest activity first, among threads with peers in `scope`.
    /// The peer filter may be an alias.
    pub fn threads(
        &self,
        q: &ListThreadsQuery,
        scope: Option<&Peers>,
    ) -> Result<Vec<ThreadSummary>, Error> {
        let peer = q
            .peer
            .as_ref()
            .map(|p| self.resolve_peer(p, scope))
            .transpose()?;
        let turn = q.turn.map(|t| match t {
            TurnFilter::Mine => "mine",
            TurnFilter::Theirs => "theirs",
            TurnFilter::None => "none",
        });
        let rows = self.sql.query(
            &format!(
                "SELECT {THREAD_COLUMNS} FROM threads \
                 WHERE (?1 IS NULL OR peer IN (SELECT value FROM json_each(?1))) \
                 AND (?2 IS NULL OR state = ?2) \
                 AND (?3 IS NULL OR peer = ?3) \
                 AND (?4 IS NULL OR EXISTS (SELECT 1 FROM json_each(threads.labels) WHERE value = ?4)) \
                 AND (?5 IS NULL OR (?5 = 'mine' AND turn = ?6) \
                      OR (?5 = 'theirs' AND turn IS NOT NULL AND turn != ?6) \
                      OR (?5 = 'none' AND turn IS NULL)) \
                 ORDER BY updated_at DESC, id LIMIT ?7"
            ),
            &[
                scope_json(scope),
                q.state.map(cp_thread::State::as_str).into(),
                peer.as_ref().map(Name::as_str).into(),
                q.label.as_ref().map(|l| l.as_str().to_string()).into(),
                turn.into(),
                text(&self.me),
                q.limit.unwrap_or(DEFAULT_THREADS).clamp(1, MAX_THREADS).into(),
            ],
        )?;
        rows.iter()
            .map(|r| self.summary_of(&thread_row(r)?))
            .collect()
    }
}
