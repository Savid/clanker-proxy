//! Events: one step in a thread.

use serde::{Deserialize, Serialize};

use crate::text::{Body, Id, Labels, Name, Title};
use crate::time::At;
use crate::workflow::{Action, Kind};

/// Highest Lamport clock an event may carry.
pub const MAX_CLOCK: u64 = 1_000_000_000;

/// What only an open event carries.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Opening {
    pub title: Title,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Labels::is_empty")]
    pub labels: Labels,
}

/// One step in a thread. `from` and `to` are names as this daemon knows
/// them: its owner's and the peer's. Each field is valid on its own; the
/// rules between fields are [`Event::validate`]'s.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub id: Id,
    pub thread: Id,
    pub from: Name,
    pub to: Name,
    pub at: At,
    /// Orders the thread: one more than the highest clock its author had
    /// seen in it.
    pub clock: u64,
    pub action: Action,
    /// Set on open only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening: Option<Opening>,
    #[serde(default)]
    pub body: Body,
}

/// Why an event is malformed, whatever thread it is for.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    #[error("from and to are both {0}")]
    ToSelf(Name),
    #[error("clock {0}: want 1 for open, 2 to {MAX_CLOCK} for the rest")]
    Clock(u64),
    #[error("title, kind and labels are set on open, and only there")]
    Opening,
    #[error("open: thread must equal id")]
    OpenThread,
}

impl Event {
    /// Checks the rules between fields.
    pub fn validate(&self) -> Result<(), EventError> {
        let open = self.action == Action::Open;
        if self.from == self.to {
            return Err(EventError::ToSelf(self.from.clone()));
        }
        if open != (self.clock == 1) || self.clock == 0 || self.clock > MAX_CLOCK {
            return Err(EventError::Clock(self.clock));
        }
        if open != self.opening.is_some() {
            return Err(EventError::Opening);
        }
        if open && self.thread != self.id {
            return Err(EventError::OpenThread);
        }
        Ok(())
    }

    /// Where the event falls in replay: opens first, then by clock, then ID.
    /// Both peers order the same events identically, and an event always
    /// follows those its author had seen.
    pub fn position(&self) -> (bool, u64, &Id) {
        position(self)
    }
}

/// What deciding an event needs: everything but its text. Replay works on
/// any `Move`, so a daemon can replay a thread without reading its bodies.
pub trait Move {
    fn id(&self) -> &Id;
    fn thread(&self) -> &Id;
    fn from(&self) -> &Name;
    fn to(&self) -> &Name;
    fn at(&self) -> At;
    fn clock(&self) -> u64;
    fn action(&self) -> Action;
    /// The body's size in bytes, which room is measured in.
    fn body_bytes(&self) -> u64;
    /// Whether the body is empty or only whitespace.
    fn body_blank(&self) -> bool;
    /// Whether the body is empty.
    fn body_empty(&self) -> bool {
        self.body_bytes() == 0
    }
}

impl Move for Event {
    fn id(&self) -> &Id {
        &self.id
    }
    fn thread(&self) -> &Id {
        &self.thread
    }
    fn from(&self) -> &Name {
        &self.from
    }
    fn to(&self) -> &Name {
        &self.to
    }
    fn at(&self) -> At {
        self.at
    }
    fn clock(&self) -> u64 {
        self.clock
    }
    fn action(&self) -> Action {
        self.action
    }
    fn body_bytes(&self) -> u64 {
        self.body.bytes()
    }
    fn body_blank(&self) -> bool {
        self.body.is_blank()
    }
}

/// An event as replay sees it, without its text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Step {
    pub id: Id,
    pub thread: Id,
    pub from: Name,
    pub to: Name,
    pub at: At,
    pub clock: u64,
    pub action: Action,
    pub body_bytes: u64,
    pub body_blank: bool,
}

impl From<&Event> for Step {
    fn from(e: &Event) -> Self {
        Self {
            id: e.id.clone(),
            thread: e.thread.clone(),
            from: e.from.clone(),
            to: e.to.clone(),
            at: e.at,
            clock: e.clock,
            action: e.action,
            body_bytes: e.body.bytes(),
            body_blank: e.body.is_blank(),
        }
    }
}

impl Move for Step {
    fn id(&self) -> &Id {
        &self.id
    }
    fn thread(&self) -> &Id {
        &self.thread
    }
    fn from(&self) -> &Name {
        &self.from
    }
    fn to(&self) -> &Name {
        &self.to
    }
    fn at(&self) -> At {
        self.at
    }
    fn clock(&self) -> u64 {
        self.clock
    }
    fn action(&self) -> Action {
        self.action
    }
    fn body_bytes(&self) -> u64 {
        self.body_bytes
    }
    fn body_blank(&self) -> bool {
        self.body_blank
    }
}

/// Where a move falls in replay: opens first, then by clock, then ID.
pub fn position<M: Move>(m: &M) -> (bool, u64, &Id) {
    (m.action() != Action::Open, m.clock(), m.id())
}
