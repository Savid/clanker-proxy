//! Request and response bodies, and query parameters.

use cp_thread::{
    Action, At, Body, Event, EventError, Id, Kind, Label, Labels, Name, Opening, Role, State, Title,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::text::{
    Aliases, Code, Cursor, DaemonUrl, Note, Peers, PublicKey, RequestRef, Token, Version,
};

// ---- system ----

/// Always `ok`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HealthStatus {
    Ok,
}

/// The daemon is up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub status: HealthStatus,
    /// The daemon's clock when it answered.
    pub at: At,
}

/// The daemon's owner, as peers see them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub name: Name,
    /// Other names the owner offers peers, sorted. Peering in either
    /// direction gives them to the peer, which keeps those still free there
    /// as aliases.
    #[schema(max_items = 10)]
    pub aliases: Vec<Name>,
    /// Where peers reach this daemon, if it has a public URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<DaemonUrl>,
    pub version: String,
    /// For an agent token limited to peers, those peers; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peers: Option<Peers>,
}

// ---- peering ----

/// One daemon asking another to become peers.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeeringRequest {
    /// The name the requester asks to be known by.
    pub name: Name,
    pub url: DaemonUrl,
    /// The requester's public key.
    pub key: PublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
    /// Other names the requester offers; approving keeps those still free
    /// as their aliases.
    #[serde(default, skip_serializing_if = "Aliases::is_empty")]
    pub aliases: Aliases,
}

/// What an approving daemon tells the requester.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeeringAccepted {
    /// Other names the approver offers; the requester keeps those still
    /// free as their aliases.
    #[serde(default)]
    pub aliases: Aliases,
}

/// Always `pending`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReceiptStatus {
    Pending,
}

/// A peering request was received and waits for the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RequestReceipt {
    pub status: ReceiptStatus,
    /// What both owners compare, from both daemons' keys.
    pub code: Code,
    /// This daemon's public key, which the requester verifies calls with.
    pub key: PublicKey,
}

/// A peering request waiting for the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: RequestRef,
    pub name: Name,
    /// Other names they offer, sorted; approving keeps those still free as
    /// their aliases.
    #[schema(max_items = 10)]
    pub aliases: Vec<Name>,
    pub url: DaemonUrl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
    pub code: Code,
    pub at: At,
}

/// Pending peering requests, oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RequestList {
    pub requests: Vec<Request>,
}

/// How to approve a peering request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Approval {
    /// What to call the new peer; the name they asked for when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Name>,
}

/// Who to ask to become peers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerInput {
    /// What to call them.
    pub name: Name,
    pub url: DaemonUrl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
}

/// `requested`: waiting for their owner to approve. `active`: both sides
/// accept each other's key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PeerStatus {
    Requested,
    Active,
}

/// A daemon this one exchanges threads with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub name: Name,
    /// Other names the owner calls them by here, sorted. Operations that
    /// take a peer's name accept these too, except removePeer.
    #[schema(max_items = 10)]
    pub aliases: Vec<Name>,
    pub url: DaemonUrl,
    pub status: PeerStatus,
    pub code: Code,
    pub added_at: At,
    /// The release the peer's daemon last reported calling this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Version>,
}

/// Aliases, replacing the ones there are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerAliases {
    /// Empty removes them all.
    pub aliases: Aliases,
}

/// Peers, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PeerList {
    pub peers: Vec<Peer>,
}

// ---- threads ----

/// A thread to open with a peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewThread {
    pub to: Name,
    pub title: Title,
    /// `request` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(value_type = Vec<Label>, max_items = 10)]
    pub labels: Vec<Label>,
    #[serde(default, skip_serializing_if = "Body::is_empty")]
    pub body: Body,
}

/// A reply or a move on a thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThreadAction {
    pub action: Action,
    #[serde(default, skip_serializing_if = "Body::is_empty")]
    pub body: Body,
}

/// A thread without its events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: Id,
    pub title: Title,
    pub kind: Kind,
    pub labels: Labels,
    pub sender: Name,
    pub recipient: Name,
    /// The participant who is not the owner.
    pub peer: Name,
    /// The owner's part. The sender opened it and can withdraw it; the
    /// recipient was asked and can ack, ask, resolve or decline it. Either
    /// can comment, close or reopen when the state allows.
    pub role: Role,
    pub state: State,
    /// Whose move it is; absent once the thread has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<Name>,
    /// The thread is waiting on the owner.
    pub my_turn: bool,
    /// Who made the latest move to arrive that changed something. With
    /// `myTurn`, the peer here means the owner has not answered it.
    pub last_from: Name,
    /// What the owner may do now, for `actOnThread`.
    pub actions: Vec<Action>,
    pub opened_at: At,
    pub updated_at: At,
    /// Events in the thread.
    #[schema(maximum = 1_000_000)]
    pub events: u32,
    /// The owner's events not yet stored by the peer.
    #[schema(maximum = 1_000_000)]
    pub undelivered: u32,
    /// The owner's events the peer refused for good.
    #[schema(maximum = 1_000_000)]
    pub failed: u32,
    /// The owner's moves that took no effect because a concurrent move from
    /// the peer replays first. Their events say so in `ignored`.
    #[schema(maximum = 1_000_000)]
    pub overruled: u32,
}

/// `pending`: not yet stored by the peer; retried until it is, or the peer
/// is removed, or a peer that no longer knows this daemon has refused it
/// for a week. `delivered`: the peer stored it. `failed`: never to be
/// delivered: refused, given up, or the peer was removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DeliveryStatus {
    Pending,
    Delivered,
    Failed,
}

/// How the owner's event is getting to the peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    pub status: DeliveryStatus,
    #[schema(maximum = 1_000_000)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<At>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<At>,
}

/// One event in a thread, as this daemon holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThreadEvent {
    pub id: Id,
    pub from: Name,
    pub at: At,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Body::is_empty")]
    pub body: Body,
    /// Why the event did not change the thread: the other side's move came
    /// first, or it was not its author's to take. Its body still stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored: Option<String>,
    /// For the owner's events only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
}

/// A thread and a page of its events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    #[serde(flatten)]
    pub summary: ThreadSummary,
    /// Events in replay order: those before `before`, at most `limit`.
    pub log: Vec<ThreadEvent>,
    /// Where `log` starts in the thread's replay order; 0 means the open.
    /// Pass it as `before` for the page before this one.
    #[schema(maximum = 1_000_000)]
    pub log_start: u32,
}

/// Threads, newest activity first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ThreadList {
    pub threads: Vec<ThreadSummary>,
}

/// Whose move it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TurnFilter {
    /// Waiting on the owner.
    Mine,
    /// Waiting on the peer.
    Theirs,
    /// Ended.
    None,
}

/// Default page size for thread listings.
pub const DEFAULT_THREADS: u32 = 100;
/// Largest page of threads.
pub const MAX_THREADS: u32 = 500;
/// Default page of a thread's events.
pub const DEFAULT_LOG: u32 = 100;
/// Largest page of a thread's events.
pub const MAX_LOG: u32 = 1000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ListThreadsQuery {
    /// Whose move it is.
    pub turn: Option<TurnFilter>,
    /// Only threads in this state.
    pub state: Option<State>,
    /// Only threads with this peer.
    pub peer: Option<Name>,
    /// Only threads with this label.
    pub label: Option<Label>,
    /// At most this many threads (default 100).
    #[param(minimum = 1, maximum = 500)]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct GetThreadQuery {
    /// At most this many events (default 100).
    #[param(minimum = 1, maximum = 1000)]
    pub limit: Option<u32>,
    /// Only events before this position in replay order; the latest when
    /// absent.
    #[param(minimum = 0, maximum = 1_000_000)]
    pub before: Option<u32>,
}

// ---- federation ----

/// One step in a thread, as daemons exchange it. Who sent it is the peer
/// whose secret delivered it; it is for the receiving daemon's owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = Event)]
pub struct WireEvent {
    pub id: Id,
    pub thread: Id,
    /// When it was written, by its author's clock; for display only.
    pub at: At,
    /// Orders the thread: 1 for the open, then one more than the highest
    /// clock its author had seen in the thread.
    #[schema(minimum = 1, maximum = 1_000_000_000)]
    pub clock: u64,
    pub action: Action,
    /// Open only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Title>,
    /// Open only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// Open only.
    #[serde(default, skip_serializing_if = "Labels::is_empty")]
    pub labels: Labels,
    #[serde(default, skip_serializing_if = "Body::is_empty")]
    pub body: Body,
}

/// Why a wire event is not an event.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("open needs a title and a kind")]
    OpenFields,
    #[error("title, kind and labels are set on open only")]
    NotOpen,
    #[error(transparent)]
    Event(#[from] EventError),
}

impl WireEvent {
    /// The event as `from` sent it to `to`.
    pub fn into_event(self, from: Name, to: Name) -> Result<Event, WireError> {
        let opening = if self.action == Action::Open {
            match (self.title, self.kind) {
                (Some(title), Some(kind)) => Some(Opening {
                    title,
                    kind,
                    labels: self.labels,
                }),
                _ => return Err(WireError::OpenFields),
            }
        } else if self.title.is_some() || self.kind.is_some() || !self.labels.is_empty() {
            return Err(WireError::NotOpen);
        } else {
            None
        };
        let e = Event {
            id: self.id,
            thread: self.thread,
            from,
            to,
            at: self.at,
            clock: self.clock,
            action: self.action,
            opening,
            body: self.body,
        };
        e.validate()?;
        Ok(e)
    }
}

impl From<&Event> for WireEvent {
    fn from(e: &Event) -> Self {
        let (title, kind, labels) = match &e.opening {
            Some(o) => (Some(o.title.clone()), Some(o.kind), o.labels.clone()),
            None => (None, None, Labels::default()),
        };
        Self {
            id: e.id.clone(),
            thread: e.thread.clone(),
            at: e.at,
            clock: e.clock,
            action: e.action,
            title,
            kind,
            labels,
            body: e.body.clone(),
        }
    }
}

/// An event was stored, or already had been.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Receipt {
    pub id: Id,
    /// The event was already stored.
    pub duplicate: bool,
}

// ---- agent tokens ----

/// A new agent token. With peers, it reaches only those peers and their
/// threads: it lists, streams and sends only to them, and others answer 404.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentTokenInput {
    pub name: Name,
    #[serde(default)]
    pub peers: Peers,
    /// When it stops working; it works until revoked when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<At>,
}

/// An agent token, without the token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentTokenSummary {
    pub name: Name,
    pub peers: Peers,
    pub created_at: At,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<At>,
    /// Its last use, to the minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_at: Option<At>,
}

/// A new agent token and the token itself, which is never shown again.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAgentToken {
    pub name: Name,
    pub peers: Peers,
    pub token: Token,
    pub created_at: At,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<At>,
}

/// Agent tokens, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentTokenList {
    pub tokens: Vec<AgentTokenSummary>,
}

// ---- paging for webhook deliveries ----

/// Default and largest page of webhook deliveries.
pub const MAX_DELIVERIES: u32 = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct WebhookDeliveriesQuery {
    /// At most this many records (default 100).
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
    /// From a previous page's `nextCursor`.
    pub cursor: Option<Cursor>,
    /// Only deliveries with this status.
    pub status: Option<DeliveryStatus>,
}
