//! A thread's state folded from its events, without the events themselves.

use crate::event::{Event, Move};
use crate::text::{Id, Name};
use crate::time::At;
use crate::workflow::{Action, Kind, Role, State};

/// Each author's room in a thread: at most this many events…
pub const MAX_EVENTS: u32 = 1000;
/// …and this many bytes of bodies. Counting per author keeps one side from
/// filling a thread for the other.
pub const MAX_THREAD_BODY: u64 = 4 << 20;
/// Past its room an author may still end the thread, without a body, up to
/// this many more times.
pub const ENDING_SLACK: u32 = 8;
/// How far past [`Fold::next_clock`] a peer's event may be. An honest peer's
/// clock is one past the events it holds, which can run ahead of this
/// daemon's only by the peer's own events that this daemon refused.
pub const MAX_LEAD: u64 = 1000;

/// Why an event cannot apply to a thread as it stands. The event is kept,
/// marked with this, and its body still reads as a comment.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("event belongs to thread {0}")]
    OtherThread(Id),
    #[error("{from} to {to} is not between this thread's participants")]
    NotParticipants { from: Name, to: Name },
    #[error("thread is already open")]
    AlreadyOpen,
    #[error("only the {role} may {action}")]
    Role { role: Role, action: Action },
    #[error("cannot {action} a thread that is {state}")]
    State { action: Action, state: State },
    #[error("reopen requires a reason")]
    NoReason,
}

/// Why an author has no room for an event.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RoomError {
    #[error("{author} has added the most events allowed to thread {thread}")]
    Exhausted { author: Name, thread: Id },
    #[error(
        "{author} has used its room in thread {thread}: only close, decline or withdraw, without a body, can follow"
    )]
    Spent { author: Name, thread: Id },
}

/// Why a peer's event has a clock the thread cannot take.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClockError {
    /// Peers deliver in order, so each author's clocks rise; a lower one
    /// would replay ahead of what its author already said.
    #[error("clock {clock} is not after {author}'s clock {seen} in this thread")]
    NotAfter { clock: u64, author: Name, seen: u64 },
    /// A clock far ahead would use up the room below the maximum.
    #[error("clock {clock} is past {limit}, this thread's limit")]
    TooFar { clock: u64, limit: u64 },
}

/// What one participant has added to a thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub events: u32,
    pub body_bytes: u64,
    pub max_clock: u64,
}

impl Usage {
    fn add<M: Move>(&mut self, e: &M) {
        self.events = self.events.saturating_add(1);
        self.body_bytes = self.body_bytes.saturating_add(e.body_bytes());
        self.max_clock = self.max_clock.max(e.clock());
    }
}

/// A thread's state, folded from its events in replay order. It is
/// everything deciding the next event needs, so a daemon can keep it and
/// apply a new event without reading the thread's history, as long as the
/// event sorts after the last one folded in ([`Fold::follows`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fold {
    pub id: Id,
    pub kind: Kind,
    pub sender: Name,
    pub recipient: Name,
    pub state: State,
    pub opened_at: At,
    pub updated_at: At,
    /// Replay position of the last event folded in.
    pub last_clock: u64,
    pub last_id: Id,
    /// Highest clock among all events folded in.
    pub max_clock: u64,
    pub sender_usage: Usage,
    pub recipient_usage: Usage,
}

impl Fold {
    /// The thread an open event starts, or `None` if `e` is not an open.
    pub fn open(e: &Event) -> Option<Self> {
        let opening = e.opening.as_ref()?;
        Self::start(e, opening.kind)
    }

    /// The thread an open move of `kind` starts, or `None` if `m` is not an
    /// open. For replaying stored moves, whose kind is kept apart.
    pub fn start<M: Move>(m: &M, kind: Kind) -> Option<Self> {
        if m.action() != Action::Open {
            return None;
        }
        let mut sender_usage = Usage::default();
        sender_usage.add(m);
        Some(Self {
            id: m.thread().clone(),
            kind,
            sender: m.from().clone(),
            recipient: m.to().clone(),
            state: State::Open,
            opened_at: m.at(),
            updated_at: m.at(),
            last_clock: m.clock(),
            last_id: m.id().clone(),
            max_clock: m.clock(),
            sender_usage,
            recipient_usage: Usage::default(),
        })
    }

    /// Whether `e` replays after everything folded in, so [`Fold::step`]
    /// gives the same result as replaying the whole thread.
    pub fn follows<M: Move>(&self, e: &M) -> bool {
        e.action() != Action::Open && (e.clock(), e.id()) > (self.last_clock, &self.last_id)
    }

    /// Folds in `e`, which must replay after everything already folded in.
    /// Returns why it had no effect, if it had none.
    pub fn step<M: Move>(&mut self, e: &M) -> Option<Refusal> {
        if let Some(usage) = self.usage_mut(e.from()) {
            usage.add(e);
        }
        self.max_clock = self.max_clock.max(e.clock());
        if (e.clock(), e.id()) > (self.last_clock, &self.last_id) {
            self.last_clock = e.clock();
            self.last_id = e.id().clone();
        }

        if let Err(refusal) = self.check(e) {
            return Some(refusal);
        }
        self.updated_at = self.updated_at.max(e.at());
        self.state = self.after(e);
        None
    }

    /// Why `e` may not apply to the thread as it stands, if it may not.
    pub fn check<M: Move>(&self, e: &M) -> Result<(), Refusal> {
        if *e.thread() != self.id {
            return Err(Refusal::OtherThread(e.thread().clone()));
        }
        let not_participants = || Refusal::NotParticipants {
            from: e.from().clone(),
            to: e.to().clone(),
        };
        let Some(role) = self.role_of(e.from()) else {
            return Err(not_participants());
        };
        if e.to() != self.peer(e.from()) {
            return Err(not_participants());
        }
        let action = e.action();
        if action == Action::Open {
            return Err(Refusal::AlreadyOpen);
        }
        let Some(allowed) = action.rule() else {
            return Ok(()); // a comment: either participant, any state
        };
        if !allowed.by.contains(&role) {
            let role = allowed.by.first().copied().unwrap_or(role);
            return Err(Refusal::Role { role, action });
        }
        if !allowed.from.contains(&self.state) {
            return Err(Refusal::State {
                action,
                state: self.state,
            });
        }
        if action == Action::Reopen && e.body_blank() {
            return Err(Refusal::NoReason);
        }
        Ok(())
    }

    /// The state once a checked event applies.
    fn after<M: Move>(&self, e: &M) -> State {
        match e.action() {
            // The sender answering a question hands the thread back.
            Action::Comment if self.state == State::NeedsInput && *e.from() == self.sender => {
                State::Acked
            }
            Action::Comment | Action::Open => self.state,
            Action::Ack if self.kind == Kind::Fyi => State::Closed,
            action => action.rule().map_or(self.state, |r| r.to),
        }
    }

    /// What `user` may do now, in [`Action::ALL`] order. A participant may
    /// always comment, unless they have used their room.
    pub fn allowed(&self, user: &Name) -> Vec<Action> {
        let Some(role) = self.role_of(user) else {
            return Vec::new();
        };
        let all = Action::ALL.into_iter().filter(|a| match a.rule() {
            None => *a == Action::Comment,
            Some(r) => r.by.contains(&role) && r.from.contains(&self.state),
        });
        if self.spent(user) {
            all.filter(|a| a.is_ending()).collect()
        } else {
            all.collect()
        }
    }

    /// Whether `user` has used their room, so only ending the thread without
    /// a body is left.
    pub fn spent(&self, user: &Name) -> bool {
        self.usage(user)
            .is_some_and(|u| u.events >= MAX_EVENTS || u.body_bytes >= MAX_THREAD_BODY)
    }

    /// Refuses `e` when its author has no room for it in the thread.
    pub fn room<M: Move>(&self, e: &M) -> Result<(), RoomError> {
        let usage = self.usage(e.from()).copied().unwrap_or_default();
        if usage.events >= MAX_EVENTS.saturating_add(ENDING_SLACK) {
            return Err(RoomError::Exhausted {
                author: e.from().clone(),
                thread: self.id.clone(),
            });
        }
        let over = usage.events >= MAX_EVENTS
            || usage.body_bytes.saturating_add(e.body_bytes()) > MAX_THREAD_BODY;
        let bare_ending = e.body_empty() && e.action().is_ending();
        if over && !bare_ending {
            return Err(RoomError::Spent {
                author: e.from().clone(),
                thread: self.id.clone(),
            });
        }
        Ok(())
    }

    /// The clock for a new event: one past every event folded in, so it
    /// replays after everything its author has seen.
    pub fn next_clock(&self) -> u64 {
        self.max_clock.saturating_add(1)
    }

    /// Why a peer's event has a clock the thread cannot take, if it cannot.
    pub fn check_clock<M: Move>(&self, e: &M) -> Result<(), ClockError> {
        if let Some(usage) = self.usage(e.from())
            && usage.max_clock >= e.clock()
        {
            return Err(ClockError::NotAfter {
                clock: e.clock(),
                author: e.from().clone(),
                seen: usage.max_clock,
            });
        }
        let limit = self.next_clock().saturating_add(MAX_LEAD);
        if e.clock() > limit {
            return Err(ClockError::TooFar {
                clock: e.clock(),
                limit,
            });
        }
        Ok(())
    }

    /// Whose move it is: the recipient while the thread is open or acked,
    /// the sender while it needs input or is resolved, and nobody once it
    /// has ended.
    pub fn turn(&self) -> Option<&Name> {
        match self.state {
            State::Open | State::Acked => Some(&self.recipient),
            State::NeedsInput | State::Resolved => Some(&self.sender),
            State::Closed | State::Declined | State::Withdrawn => None,
        }
    }

    pub fn role_of(&self, user: &Name) -> Option<Role> {
        if *user == self.sender {
            Some(Role::Sender)
        } else if *user == self.recipient {
            Some(Role::Recipient)
        } else {
            None
        }
    }

    /// The participant who is not `user`.
    pub fn peer(&self, user: &Name) -> &Name {
        if *user == self.sender {
            &self.recipient
        } else {
            &self.sender
        }
    }

    /// What `user` has added, if they are a participant.
    pub fn usage(&self, user: &Name) -> Option<&Usage> {
        match self.role_of(user)? {
            Role::Sender => Some(&self.sender_usage),
            Role::Recipient => Some(&self.recipient_usage),
        }
    }

    fn usage_mut(&mut self, user: &Name) -> Option<&mut Usage> {
        match self.role_of(user)? {
            Role::Sender => Some(&mut self.sender_usage),
            Role::Recipient => Some(&mut self.recipient_usage),
        }
    }
}
