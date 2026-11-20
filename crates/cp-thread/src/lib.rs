//! clanker-proxy threads: the events two peers exchange, the workflow they
//! follow, and the replay that turns events into a thread's state.
//!
//! The crate is pure: the same events give the same thread on both peers,
//! whatever order they arrived in. It never reads the clock or randomness;
//! callers pass those in.

mod event;
mod fold;
mod text;
mod thread;
mod time;
mod workflow;

pub use event::{Event, EventError, MAX_CLOCK, Opening};
pub use fold::{
    ClockError, ENDING_SLACK, Fold, MAX_EVENTS, MAX_LEAD, MAX_THREAD_BODY, Refusal, RoomError,
    Usage,
};
pub use text::{
    Body, Id, Label, Labels, MAX_BODY, MAX_LABEL, MAX_LABELS, MAX_NAME, MAX_TITLE, Name, TextError,
    Title, is_hidden,
};
pub use thread::{Applied, ReplayError, Thread};
pub use time::{At, AtError};
pub use workflow::{Action, Kind, Role, Rule, State};
