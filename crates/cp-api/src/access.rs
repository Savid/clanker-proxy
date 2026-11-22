//! Every operation, and who may call it. Access is declared here once: the
//! HTTP service enforces it before reading a request body, and the OpenAPI
//! document's `security` is generated from it.

use std::fmt;

/// Who may call an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Access {
    /// Anyone, without a token.
    Public,
    /// The daemon's owner, with the owner token: what decides who is trusted
    /// and who is told (peering, agent tokens, webhooks).
    Owner,
    /// The owner, or an agent token the owner made, which may be limited to
    /// some peers.
    OwnerOrAgent,
    /// An agent token only: what an agent sets up for itself.
    Agent,
    /// Another daemon, by a request signed with its key.
    Peer,
}

/// An HTTP method the API uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

macro_rules! operations {
    ($($op:ident => $id:literal, $method:ident, $path:literal, $access:ident, $body:literal;)+) => {
        /// An API operation.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Op {
            $($op),+
        }

        impl Op {
            pub const ALL: &'static [Op] = &[$(Op::$op),+];

            /// The OpenAPI `operationId`.
            pub fn id(self) -> &'static str {
                match self {
                    $(Op::$op => $id),+
                }
            }

            pub fn method(self) -> Method {
                match self {
                    $(Op::$op => Method::$method),+
                }
            }

            /// The path template, with `{param}` segments.
            pub fn path(self) -> &'static str {
                match self {
                    $(Op::$op => $path),+
                }
            }

            pub fn access(self) -> Access {
                match self {
                    $(Op::$op => Access::$access),+
                }
            }

            /// Whether the request carries a JSON body.
            pub fn has_body(self) -> bool {
                match self {
                    $(Op::$op => $body),+
                }
            }
        }
    };
}

operations! {
    GetHealth => "getHealth", Get, "/api/v1/health", Public, false;
    GetMe => "getMe", Get, "/api/v1/me", OwnerOrAgent, false;
    RequestPeering => "requestPeering", Post, "/api/v1/peering-requests", Public, true;
    ListRequests => "listRequests", Get, "/api/v1/peering-requests", Owner, false;
    DenyRequest => "denyRequest", Delete, "/api/v1/peering-requests/{id}", Owner, false;
    ApproveRequest => "approveRequest", Post, "/api/v1/peering-requests/{id}/approve", Owner, true;
    ListPeers => "listPeers", Get, "/api/v1/peers", OwnerOrAgent, false;
    AddPeer => "addPeer", Post, "/api/v1/peers", Owner, true;
    GetPeer => "getPeer", Get, "/api/v1/peers/{name}", OwnerOrAgent, false;
    RemovePeer => "removePeer", Delete, "/api/v1/peers/{name}", Owner, false;
    SetPeerAliases => "setPeerAliases", Put, "/api/v1/peers/{name}/aliases", Owner, true;
    ListThreads => "listThreads", Get, "/api/v1/threads", OwnerOrAgent, false;
    OpenThread => "openThread", Post, "/api/v1/threads", OwnerOrAgent, true;
    GetThread => "getThread", Get, "/api/v1/threads/{ref}", OwnerOrAgent, false;
    ActOnThread => "actOnThread", Post, "/api/v1/threads/{ref}/events", OwnerOrAgent, true;
    StreamEvents => "streamEvents", Get, "/api/v1/stream", OwnerOrAgent, false;
    DeliverEvent => "deliverEvent", Post, "/api/v1/federation/events", Peer, true;
    NotifyPeeringAccepted => "notifyPeeringAccepted", Post, "/api/v1/federation/accepted", Peer, false;
    ListAgentTokens => "listAgentTokens", Get, "/api/v1/agent-tokens", Owner, false;
    CreateAgentToken => "createAgentToken", Post, "/api/v1/agent-tokens", Owner, true;
    DeleteAgentToken => "deleteAgentToken", Delete, "/api/v1/agent-tokens/{name}", Owner, false;
    RevokeOwnToken => "revokeOwnToken", Delete, "/api/v1/me", Agent, false;
    ListWebhooks => "listWebhooks", Get, "/api/v1/webhooks", Owner, false;
    CreateWebhook => "createWebhook", Post, "/api/v1/webhooks", Owner, true;
    GetWebhook => "getWebhook", Get, "/api/v1/webhooks/{name}", Owner, false;
    UpdateWebhook => "updateWebhook", Patch, "/api/v1/webhooks/{name}", Owner, true;
    DeleteWebhook => "deleteWebhook", Delete, "/api/v1/webhooks/{name}", Owner, false;
    ListWebhookDeliveries => "listWebhookDeliveries", Get, "/api/v1/webhooks/{name}/deliveries", Owner, false;
    RetryWebhookDelivery => "retryWebhookDelivery", Post, "/api/v1/webhooks/{name}/deliveries/{id}/retry", Owner, false;
    SetOwnWebhook => "setOwnWebhook", Post, "/api/v1/me/webhook", Agent, true;
    DeleteOwnWebhook => "deleteOwnWebhook", Delete, "/api/v1/me/webhook", Agent, false;
}

/// Token prefixes, so a token's kind shows and kinds are never confused.
pub mod prefix {
    pub const OWNER: &str = "cpo_";
    pub const AGENT: &str = "cpa_";
}
