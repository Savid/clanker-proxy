//! A thread replayed from its events.

use crate::event::Event;
use crate::fold::{Fold, Refusal};
use crate::text::{Id, Labels, Name, Title};
use crate::workflow::Action;

/// An event and its effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub event: Event,
    /// Why the event had no effect: the other side's move came first, or the
    /// workflow refused it.
    pub ignored: Option<Refusal>,
}

/// Why events do not make a thread.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    #[error("thread has no open event")]
    NoOpen,
    #[error("event {event} belongs to thread {belongs}, not {thread}")]
    ForeignEvent { event: Id, belongs: Id, thread: Id },
}

/// The state replayed from a thread's events, with the events in replay
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub fold: Fold,
    pub events: Vec<Applied>,
}

impl Thread {
    /// Orders events and folds them into a thread. The open comes first; the
    /// rest follow by clock, then ID, so two peers holding the same events
    /// reach the same state, whatever order they arrived in.
    pub fn replay(events: impl IntoIterator<Item = Event>) -> Result<Self, ReplayError> {
        let mut sorted: Vec<Event> = events.into_iter().collect();
        sorted.sort_by(|a, b| a.position().cmp(&b.position()));
        let mut it = sorted.into_iter();
        let open = it.next().ok_or(ReplayError::NoOpen)?;
        let mut fold = Fold::open(&open).ok_or(ReplayError::NoOpen)?;
        let mut applied = vec![Applied {
            event: open,
            ignored: None,
        }];
        for e in it {
            if e.thread != fold.id {
                return Err(ReplayError::ForeignEvent {
                    event: e.id,
                    belongs: e.thread,
                    thread: fold.id,
                });
            }
            let ignored = fold.step(&e);
            applied.push(Applied { event: e, ignored });
        }
        Ok(Self {
            fold,
            events: applied,
        })
    }

    /// The thread with `e` added. An event that replays after everything
    /// held is folded in directly; one that lands earlier (the other side's
    /// concurrent move) replays the thread, since it can change what later
    /// events did.
    pub fn insert(&self, e: Event) -> Result<Self, ReplayError> {
        if e.thread != self.fold.id {
            return Err(ReplayError::ForeignEvent {
                event: e.id,
                belongs: e.thread,
                thread: self.fold.id.clone(),
            });
        }
        if self.fold.follows(&e) {
            let mut next = self.clone();
            let ignored = next.fold.step(&e);
            next.events.push(Applied { event: e, ignored });
            return Ok(next);
        }
        Self::replay(self.events.iter().map(|a| a.event.clone()).chain([e]))
    }

    /// The open event.
    pub fn opening(&self) -> Option<&Event> {
        self.events
            .first()
            .map(|a| &a.event)
            .filter(|e| e.action == Action::Open)
    }

    pub fn title(&self) -> Option<&Title> {
        self.opening()
            .and_then(|e| e.opening.as_ref())
            .map(|o| &o.title)
    }

    pub fn labels(&self) -> Option<&Labels> {
        self.opening()
            .and_then(|e| e.opening.as_ref())
            .map(|o| &o.labels)
    }

    /// `user`'s moves that took no effect. A daemon only stores its owner's
    /// move after checking it applies, so each of these lost a race: a
    /// concurrent move from the peer replays first.
    pub fn overruled(&self, user: &Name) -> impl Iterator<Item = &Applied> {
        self.events
            .iter()
            .filter(move |a| a.event.from == *user && a.ignored.is_some())
    }
}
