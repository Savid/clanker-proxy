//! The workflow: states, roles, actions and the rules between them.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! names {
    ($ty:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

/// Where a thread is in its workflow. Closed, declined and withdrawn end
/// it; reopen leaves closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Open,
    Acked,
    NeedsInput,
    Resolved,
    Closed,
    Declined,
    Withdrawn,
}

names!(State {
    Open => "open",
    Acked => "acked",
    NeedsInput => "needs-input",
    Resolved => "resolved",
    Closed => "closed",
    Declined => "declined",
    Withdrawn => "withdrawn",
});

impl State {
    pub const ALL: [State; 7] = [
        State::Open,
        State::Acked,
        State::NeedsInput,
        State::Resolved,
        State::Closed,
        State::Declined,
        State::Withdrawn,
    ];

    /// Whether the thread is in progress.
    pub fn is_active(self) -> bool {
        ACTIVE.contains(&self)
    }
}

/// A participant's part in a thread: the sender opened it; the recipient
/// was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Sender,
    Recipient,
}

names!(Role {
    Sender => "sender",
    Recipient => "recipient",
});

/// What an event does to its thread. Open starts it; comment adds to it in
/// any state; the rest move it between states (see [`Action::rule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Open,
    Comment,
    Ack,
    NeedsInput,
    Resolve,
    Close,
    Reopen,
    Decline,
    Withdraw,
}

names!(Action {
    Open => "open",
    Comment => "comment",
    Ack => "ack",
    NeedsInput => "needs-input",
    Resolve => "resolve",
    Close => "close",
    Reopen => "reopen",
    Decline => "decline",
    Withdraw => "withdraw",
});

/// What the sender expects back: a request expects a resolution; an fyi
/// closes when acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Request,
    Fyi,
}

names!(Kind {
    Request => "request",
    Fyi => "fyi",
});

/// Who may take an action, from which states, and the state it leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub by: &'static [Role],
    pub from: &'static [State],
    pub to: State,
}

const ACTIVE: &[State] = &[State::Open, State::Acked, State::NeedsInput];
const RECIPIENT: &[Role] = &[Role::Recipient];
const SENDER: &[Role] = &[Role::Sender];
const EITHER: &[Role] = &[Role::Sender, Role::Recipient];

impl Action {
    /// Every action, in the order a thread usually meets them.
    pub const ALL: [Action; 9] = [
        Action::Open,
        Action::Comment,
        Action::Ack,
        Action::NeedsInput,
        Action::Resolve,
        Action::Close,
        Action::Reopen,
        Action::Decline,
        Action::Withdraw,
    ];

    /// The workflow: the recipient works the thread, the sender may withdraw
    /// it, and either participant may close or reopen it. Open and comment
    /// have no rule: open starts a thread, and either participant may
    /// comment in any state.
    pub fn rule(self) -> Option<Rule> {
        let rule = |by, from, to| Some(Rule { by, from, to });
        match self {
            Action::Open | Action::Comment => None,
            Action::Ack => rule(RECIPIENT, &[State::Open], State::Acked),
            Action::NeedsInput => rule(RECIPIENT, &[State::Open, State::Acked], State::NeedsInput),
            Action::Resolve => rule(RECIPIENT, ACTIVE, State::Resolved),
            Action::Decline => rule(RECIPIENT, ACTIVE, State::Declined),
            Action::Withdraw => rule(SENDER, ACTIVE, State::Withdrawn),
            Action::Close => rule(
                EITHER,
                &[
                    State::Open,
                    State::Acked,
                    State::NeedsInput,
                    State::Resolved,
                ],
                State::Closed,
            ),
            Action::Reopen => rule(EITHER, &[State::Resolved, State::Closed], State::Acked),
        }
    }

    /// Whether the action ends a thread: all an author who has used their
    /// room may still do.
    pub fn is_ending(self) -> bool {
        matches!(self, Action::Close | Action::Decline | Action::Withdraw)
    }

    /// Whether the action must carry a body to make sense.
    pub fn needs_body(self) -> bool {
        matches!(
            self,
            Action::Comment | Action::NeedsInput | Action::Resolve | Action::Reopen
        )
    }
}
