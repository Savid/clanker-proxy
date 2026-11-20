//! Webhooks: owner-configured notifications of thread and peering events.

use std::fmt;

use cp_thread::{At, Id, Name, State};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Type};
use utoipa::openapi::{RefOr, Schema};
use utoipa::{PartialSchema, ToSchema};

use crate::model::DeliveryStatus;
use crate::text::{
    Cursor, Peers, WebhookHeaderName, WebhookHeaderValue, WebhookSecret, WebhookUrl,
};

/// A destination format. Immutable once the webhook exists, so queued
/// notifications never switch format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WebhookType {
    Generic,
    Discord,
    Slack,
    Teams,
    GoogleChat,
    Mattermost,
    Rocketchat,
    Ntfy,
    Gotify,
    Apprise,
}

/// What a destination format is and how it authenticates, for discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    #[serde(rename = "type")]
    pub kind: WebhookType,
    pub description: &'static str,
    pub auth: &'static str,
    pub example_url: &'static str,
}

impl WebhookType {
    pub const ALL: [WebhookType; 10] = [
        WebhookType::Generic,
        WebhookType::Discord,
        WebhookType::Slack,
        WebhookType::Teams,
        WebhookType::GoogleChat,
        WebhookType::Mattermost,
        WebhookType::Rocketchat,
        WebhookType::Ntfy,
        WebhookType::Gotify,
        WebhookType::Apprise,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WebhookType::Generic => "generic",
            WebhookType::Discord => "discord",
            WebhookType::Slack => "slack",
            WebhookType::Teams => "teams",
            WebhookType::GoogleChat => "google-chat",
            WebhookType::Mattermost => "mattermost",
            WebhookType::Rocketchat => "rocketchat",
            WebhookType::Ntfy => "ntfy",
            WebhookType::Gotify => "gotify",
            WebhookType::Apprise => "apprise",
        }
    }

    pub fn provider(self) -> Provider {
        let p = |description, auth, example_url| Provider {
            kind: self,
            description,
            auth,
            example_url,
        };
        match self {
            WebhookType::Generic => p(
                "Event JSON for automation; optional Standard Webhooks signing",
                "Optional signing key or custom headers",
                "https://runner.example.com/hooks/clanker",
            ),
            WebhookType::Discord => p(
                "Discord channel embed; mentions disabled",
                "Secret webhook URL",
                "https://discord.com/api/webhooks/ID/TOKEN",
            ),
            WebhookType::Slack => p(
                "Slack incoming webhook with plain-text blocks",
                "Secret webhook URL",
                "https://hooks.slack.com/services/TEAM/CHANNEL/TOKEN",
            ),
            WebhookType::Teams => p(
                "Teams Workflows Adaptive Card; use a webhook callable by Anyone",
                "Secret workflow URL",
                "https://WORKFLOW-HOST/WORKFLOW-PATH?sig=TOKEN",
            ),
            WebhookType::GoogleChat => p(
                "Google Chat space message",
                "Secret webhook URL",
                "https://chat.googleapis.com/v1/spaces/SPACE/messages?key=KEY&token=TOKEN",
            ),
            WebhookType::Mattermost => p(
                "Mattermost incoming webhook",
                "Secret webhook URL",
                "https://mattermost.example.com/hooks/TOKEN",
            ),
            WebhookType::Rocketchat => p(
                "Rocket.Chat incoming integration",
                "Secret webhook URL",
                "https://chat.example.com/hooks/ID/TOKEN",
            ),
            WebhookType::Ntfy => p(
                "Plain-text notification to a topic URL",
                "Optional Authorization header",
                "https://ntfy.example.com/TOPIC",
            ),
            WebhookType::Gotify => p(
                "Gotify application message",
                "X-Gotify-Key header or token query parameter",
                "https://gotify.example.com/message",
            ),
            WebhookType::Apprise => p(
                "Apprise API saved configuration; routes to additional providers",
                "Configured API authentication headers",
                "https://apprise.example.com/notify/CONFIG",
            ),
        }
    }
}

impl fmt::Display for WebhookType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a notification is about. Thread events fire once when stored;
/// `thread.overruled` fires when a peer's move arrives that replays before
/// one of the owner's, so the owner's move took no effect; peering requests
/// fire when first stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum WebhookEventType {
    #[serde(rename = "thread.open")]
    ThreadOpen,
    #[serde(rename = "thread.reply")]
    ThreadReply,
    #[serde(rename = "thread.ack")]
    ThreadAck,
    #[serde(rename = "thread.needs-input")]
    ThreadNeedsInput,
    #[serde(rename = "thread.resolve")]
    ThreadResolve,
    #[serde(rename = "thread.decline")]
    ThreadDecline,
    #[serde(rename = "thread.close")]
    ThreadClose,
    #[serde(rename = "thread.reopen")]
    ThreadReopen,
    #[serde(rename = "thread.withdraw")]
    ThreadWithdraw,
    #[serde(rename = "thread.overruled")]
    ThreadOverruled,
    #[serde(rename = "peering.requested")]
    PeeringRequested,
}

impl WebhookEventType {
    pub const ALL: [WebhookEventType; 11] = [
        WebhookEventType::ThreadOpen,
        WebhookEventType::ThreadReply,
        WebhookEventType::ThreadAck,
        WebhookEventType::ThreadNeedsInput,
        WebhookEventType::ThreadResolve,
        WebhookEventType::ThreadDecline,
        WebhookEventType::ThreadClose,
        WebhookEventType::ThreadReopen,
        WebhookEventType::ThreadWithdraw,
        WebhookEventType::ThreadOverruled,
        WebhookEventType::PeeringRequested,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WebhookEventType::ThreadOpen => "thread.open",
            WebhookEventType::ThreadReply => "thread.reply",
            WebhookEventType::ThreadAck => "thread.ack",
            WebhookEventType::ThreadNeedsInput => "thread.needs-input",
            WebhookEventType::ThreadResolve => "thread.resolve",
            WebhookEventType::ThreadDecline => "thread.decline",
            WebhookEventType::ThreadClose => "thread.close",
            WebhookEventType::ThreadReopen => "thread.reopen",
            WebhookEventType::ThreadWithdraw => "thread.withdraw",
            WebhookEventType::ThreadOverruled => "thread.overruled",
            WebhookEventType::PeeringRequested => "peering.requested",
        }
    }

    /// The type a thread event's action fires, in the CLI's vocabulary
    /// (a comment is a reply).
    pub fn for_action(action: cp_thread::Action) -> Self {
        use cp_thread::Action;
        match action {
            Action::Open => WebhookEventType::ThreadOpen,
            Action::Comment => WebhookEventType::ThreadReply,
            Action::Ack => WebhookEventType::ThreadAck,
            Action::NeedsInput => WebhookEventType::ThreadNeedsInput,
            Action::Resolve => WebhookEventType::ThreadResolve,
            Action::Decline => WebhookEventType::ThreadDecline,
            Action::Close => WebhookEventType::ThreadClose,
            Action::Reopen => WebhookEventType::ThreadReopen,
            Action::Withdraw => WebhookEventType::ThreadWithdraw,
        }
    }
}

impl fmt::Display for WebhookEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which event types a webhook wants: all of them, including types added
/// later (`["*"]`), or an explicit nonempty list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebhookEvents {
    All,
    Only(Vec<WebhookEventType>),
}

impl WebhookEvents {
    pub fn includes(&self, t: WebhookEventType) -> bool {
        match self {
            WebhookEvents::All => true,
            WebhookEvents::Only(types) => types.contains(&t),
        }
    }

    fn strings(&self) -> Vec<&'static str> {
        match self {
            WebhookEvents::All => vec!["*"],
            WebhookEvents::Only(types) => types.iter().map(|t| t.as_str()).collect(),
        }
    }
}

impl Serialize for WebhookEvents {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.strings().serialize(s)
    }
}

impl<'de> Deserialize<'de> for WebhookEvents {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let raw = Vec::<String>::deserialize(d)?;
        if raw.len() == 1 && raw.first().is_some_and(|s| s == "*") {
            return Ok(WebhookEvents::All);
        }
        if raw.is_empty() {
            return Err(D::Error::custom(
                "select '*' or a nonempty list of webhook event types",
            ));
        }
        let mut types = Vec::with_capacity(raw.len());
        for s in &raw {
            let t = WebhookEventType::ALL
                .into_iter()
                .find(|t| t.as_str() == s)
                .ok_or_else(|| {
                    D::Error::custom(
                        "invalid webhook event; use '*' alone or supported event types",
                    )
                })?;
            if types.contains(&t) {
                return Err(D::Error::custom(format!(
                    "webhook event {s} is listed twice"
                )));
            }
            types.push(t);
        }
        Ok(WebhookEvents::Only(types))
    }
}

impl PartialSchema for WebhookEvents {
    fn schema() -> RefOr<Schema> {
        let names = std::iter::once("*").chain(WebhookEventType::ALL.iter().map(|t| t.as_str()));
        ArrayBuilder::new()
            .items(ObjectBuilder::new().schema_type(Type::String).enum_values(Some(names)))
            .min_items(Some(1))
            .max_items(Some(WebhookEventType::ALL.len()))
            .unique_items(true)
            .description(Some("A sole '*' subscribes to all current and future event types; otherwise an explicit nonempty list."))
            .build()
            .into()
    }
}

impl ToSchema for WebhookEvents {}

/// Which events a webhook hears: from peers (incoming), the owner's own
/// moves (outgoing), or both. Peering requests are incoming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WebhookOrigin {
    Incoming,
    Outgoing,
    Both,
}

/// Whether one event came from a peer (incoming) or the owner (outgoing).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WebhookEventOrigin {
    Incoming,
    Outgoing,
}

impl WebhookOrigin {
    pub fn includes(self, o: WebhookEventOrigin) -> bool {
        matches!(
            (self, o),
            (WebhookOrigin::Both, _)
                | (WebhookOrigin::Incoming, WebhookEventOrigin::Incoming)
                | (WebhookOrigin::Outgoing, WebhookEventOrigin::Outgoing)
        )
    }
}

/// A custom header, such as authentication. The value is write-only.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookHeader {
    pub name: WebhookHeaderName,
    pub value: WebhookHeaderValue,
}

/// An owner-configured destination, without its URL path and query, header
/// values or signing key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Webhook {
    pub name: Name,
    #[serde(rename = "type")]
    pub kind: WebhookType,
    /// Scheme and host only; path and query are never returned.
    pub destination: String,
    pub events: WebhookEvents,
    pub origin: WebhookOrigin,
    pub peers: Peers,
    pub enabled: bool,
    /// Whether generic notifications are signed.
    pub signing: bool,
    pub header_names: Vec<WebhookHeaderName>,
    /// Set while the endpoint backs off after failures or a receiver's
    /// Retry-After; nothing is sent before it. Any update or retry clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(read_only)]
    pub paused_until: Option<At>,
}

/// A new webhook. Chat destinations authenticate with their URL or custom
/// headers, without a signing key. With peers, only those peers' thread
/// events are sent, and never peering requests, whose names the requester
/// chooses.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookCreate {
    pub name: Name,
    #[serde(rename = "type")]
    pub kind: WebhookType,
    pub url: WebhookUrl,
    pub events: WebhookEvents,
    pub origin: WebhookOrigin,
    pub enabled: bool,
    #[serde(default)]
    pub peers: Peers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<WebhookSecret>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schema(max_items = 16)]
    pub headers: Vec<WebhookHeader>,
}

/// Changes to a webhook; omitted settings are kept. An empty secret
/// disables signing, empty headers remove them, empty peers send every
/// peer's events. The type cannot change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<WebhookUrl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<WebhookEvents>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<WebhookOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peers: Option<Peers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<WebhookSecret>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(max_items = 16)]
    pub headers: Option<Vec<WebhookHeader>>,
}

/// Webhooks, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookList {
    pub webhooks: Vec<Webhook>,
}

/// Delivery metadata, without the signing key or response body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebhookDelivery {
    pub id: Id,
    pub event: WebhookEventType,
    pub origin: WebhookEventOrigin,
    /// The thread's ID, or the peering request's reference.
    #[schema(min_length = 1, max_length = 36)]
    pub subject: String,
    pub status: DeliveryStatus,
    #[schema(maximum = 1_000_000)]
    pub attempts: u32,
    pub created_at: At,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<At>,
    /// A failure category or HTTP status, never a URL or response body.
    pub last_error: String,
}

/// Recent deliveries, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebhookDeliveryList {
    pub deliveries: Vec<WebhookDelivery>,
    /// Pass as `cursor` for older records; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
}

/// The body of a generic webhook notification. It carries metadata only:
/// fetch the thread before acting. When signing is on, verify the raw body,
/// signature and timestamp. Deduplicate by `id`. Ignore unknown fields, and
/// unknown types when subscribed to '*'. For `peering.requested`, `peer` is
/// the name the unauthenticated requester asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebhookPayload {
    /// The notification's ID, the same on every retry.
    pub id: Id,
    #[serde(rename = "type")]
    pub kind: WebhookEventType,
    pub origin: WebhookEventOrigin,
    /// When this daemon stored the event.
    pub at: At,
    /// The thread's ID, or the peering request's reference.
    #[schema(min_length = 1, max_length = 36)]
    pub subject: String,
    /// The event, for thread notifications. For `thread.overruled`, the
    /// owner's event that took no effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Id>,
    pub peer: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
    /// Whether the thread is waiting on the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_turn: Option<bool>,
}
