//! The OpenAPI document. Each operation is described once below; its
//! `security` comes from [`Op::access`], so access is declared in one place.
// The functions exist only to carry #[utoipa::path] descriptions.
#![allow(dead_code, clippy::unused_async)]

use utoipa::OpenApi;
use utoipa::openapi::path::Operation;
use utoipa::openapi::security::{
    ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme,
};
use utoipa::openapi::{OpenApi as Document, PathItem};

use crate::access::{Access, Method, Op};
use crate::error::Problem;
use crate::model::{
    AgentTokenInput, AgentTokenList, AgentTokenSummary, Approval, Delivery, DeliveryStatus,
    GetThreadQuery, Health, HealthStatus, ListThreadsQuery, Me, NewAgentToken, NewThread, Peer,
    PeerAliases, PeerInput, PeerList, PeerStatus, PeeringRequest, Receipt, ReceiptStatus, Request,
    RequestList, RequestReceipt, Thread, ThreadAction, ThreadEvent, ThreadList, ThreadSummary,
    TurnFilter, WebhookDeliveriesQuery, WireEvent,
};
use crate::text::{Aliases, Cursor, ThreadRef};
use crate::webhooks::{
    OwnWebhook, Webhook, WebhookCreate, WebhookDelivery, WebhookDeliveryList, WebhookEventOrigin,
    WebhookEventType, WebhookEvents, WebhookHeader, WebhookList, WebhookOrigin, WebhookPayload,
    WebhookType, WebhookUpdate,
};
use cp_thread::{Action, At, Id, Kind, Labels, Name, Role, State};

const PROBLEM: &str = "application/problem+json";

#[utoipa::path(get, path = "/api/v1/health", operation_id = "getHealth", tag = "system",
    summary = "Report daemon health",
    description = "Answers while the daemon is serving; for load balancers and uptime checks.",
    responses((status = 200, description = "The daemon is serving.", body = Health),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn get_health() {}

#[utoipa::path(get, path = "/api/v1/me", operation_id = "getMe", tag = "system",
    summary = "Read the owner's name and public URL",
    description = "The name this daemon offers peers, and the URL it gives them to call back on. An agent token limited to peers also sees those peers.",
    responses((status = 200, description = "The owner.", body = Me),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn get_me() {}

#[utoipa::path(post, path = "/api/v1/peering-requests", operation_id = "requestPeering", tag = "peering",
    summary = "Ask to become a peer",
    description = "Another daemon asks this one's owner to accept it as a peer, giving its public key; the answer gives this daemon's key and the code both owners compare. Nothing is accepted from it until the owner approves. Requests expire after a week; at most 20 wait, and a new one drops the oldest. Past 10 requests a minute it answers 503.",
    request_body(content = PeeringRequest, description = "The requester and its public key."),
    responses((status = 202, description = "The request waits for the owner.", body = RequestReceipt),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn request_peering() {}

#[utoipa::path(get, path = "/api/v1/peering-requests", operation_id = "listRequests", tag = "peering",
    summary = "List pending peering requests",
    description = "Requests not yet approved or denied, from the last week. Compare each code with the requester before approving.",
    responses((status = 200, description = "Pending requests, oldest first.", body = RequestList),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn list_requests() {}

#[utoipa::path(delete, path = "/api/v1/peering-requests/{id}", operation_id = "denyRequest", tag = "peering",
    summary = "Deny a peering request",
    description = "Drops the request; nothing is sent to the requester. Refused with 409 while the request is being approved.",
    params(("id" = crate::text::RequestRef, Path, description = "The request's reference.")),
    responses((status = 204, description = "The request is gone."),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn deny_request() {}

#[utoipa::path(post, path = "/api/v1/peering-requests/{id}/approve", operation_id = "approveRequest", tag = "peering",
    summary = "Approve a peering request",
    description = "First sends the requester's daemon, at the URL it gave, an acceptance signed for its key: only if that daemon confirms does the requester become a peer, under the name given or the one they asked for. So a request cannot claim someone else's URL. Fails with 409 when that name is another peer's name or alias: give another. Fails with 502 when their daemon does not confirm, and the request stays. While confirming, calls signed with the requester's key answer 503 to retry. Only one approval runs at a time; another answers 409. Redirects are refused.",
    params(("id" = crate::text::RequestRef, Path, description = "The request's reference.")),
    request_body(content = Approval, description = "What to call the new peer."),
    responses((status = 201, description = "The new peer.", body = Peer),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn approve_request() {}

#[utoipa::path(get, path = "/api/v1/peers", operation_id = "listPeers", tag = "peering",
    summary = "List peers",
    description = "Every peer, requested or active, with its aliases. An agent token limited to peers lists only those.",
    responses((status = 200, description = "Peers, by name.", body = PeerList),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn list_peers() {}

#[utoipa::path(post, path = "/api/v1/peers", operation_id = "addPeer", tag = "peering",
    summary = "Ask another daemon to become peers",
    description = "Makes a secret, saves the peer as requested, and sends a peering request to the daemon at the URL; if that fails, nothing is kept and it answers 502. Events to them queue until their owner approves. Fails with 409 when the name is a peer's name or alias, the URL already belongs to a peer, or that daemon has already asked to peer (approve its request instead). Redirects are refused.",
    request_body(content = PeerInput, description = "Who to ask, and where."),
    responses((status = 201, description = "The requested peer.", body = Peer),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn add_peer() {}

#[utoipa::path(get, path = "/api/v1/peers/{name}", operation_id = "getPeer", tag = "peering",
    summary = "Read a peer",
    description = "One peer, with the short code both owners compared. For an agent token limited to peers, any other answers 404 as if it did not exist.",
    params(("name" = Name, Path, description = "The peer's name or an alias.")),
    responses((status = 200, description = "The peer.", body = Peer),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn get_peer() {}

#[utoipa::path(delete, path = "/api/v1/peers/{name}", operation_id = "removePeer", tag = "peering",
    summary = "Remove a peer",
    description = "Their key stops working here, their aliases go, and events not yet delivered to them fail. Threads are kept, so their name can later go only to a peer at the same URL. Takes the peer's name only: an alias answers 409 and removes nothing.",
    params(("name" = Name, Path, description = "The peer's name, not an alias.")),
    responses((status = 204, description = "The peer is gone."),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn remove_peer() {}

#[utoipa::path(put, path = "/api/v1/peers/{name}/aliases", operation_id = "setPeerAliases", tag = "peering",
    summary = "Set a peer's aliases",
    description = "Replaces the peer's aliases: other names the owner calls them by here, which every operation that takes a peer's name accepts, except removePeer. Responses and threads keep the peer's name, and peers never see aliases. Peers' names and aliases share one namespace, so this fails with 409 when an alias is the owner's name, a peer's name, another peer's alias, the name of a removed peer kept for their threads, or a name being approved; adding or approving a peer under an alias fails the same way. At most 10.",
    params(("name" = Name, Path, description = "The peer's name or an alias.")),
    request_body(content = PeerAliases, description = "Every alias the peer is to have."),
    responses((status = 200, description = "The peer.", body = Peer),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn set_peer_aliases() {}

#[utoipa::path(get, path = "/api/v1/threads", operation_id = "listThreads", tag = "threads",
    summary = "List threads",
    description = "Newest activity first. The peer filter takes a peer's name or an alias. An agent token limited to peers lists only threads with those peers.",
    params(ListThreadsQuery),
    responses((status = 200, description = "Matching threads.", body = ThreadList),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn list_threads() {}

#[utoipa::path(post, path = "/api/v1/threads", operation_id = "openThread", tag = "threads",
    summary = "Open a thread with a peer",
    description = "The thread is stored and queued for delivery to the peer, named by its name or an alias. An agent token limited to peers opens threads only with those; any other answers 404 as if it were not a peer.",
    request_body(content = NewThread, description = "The thread to open."),
    responses((status = 201, description = "The new thread, without its events.", body = ThreadSummary),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn open_thread() {}

#[utoipa::path(get, path = "/api/v1/threads/{ref}", operation_id = "getThread", tag = "threads",
    summary = "Read a thread and its events",
    description = "The thread's summary and a page of its events, in the order both daemons replay them: the latest when `before` is absent. The ref is a thread ID or a unique prefix of one; for an agent token limited to peers, only threads with those peers count, and any other answers 404 as if it did not exist.",
    params(("ref" = ThreadRef, Path, description = "A thread ID or a unique prefix."), GetThreadQuery),
    responses((status = 200, description = "The thread.", body = Thread),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn get_thread() {}

#[utoipa::path(post, path = "/api/v1/threads/{ref}/events", operation_id = "actOnThread", tag = "threads",
    summary = "Reply to a thread or move it on",
    description = "Fails with 409 when the owner may not take the action now; the detail lists what they may. A reopen needs a reason: without one it fails with 400. Each side has room for 1000 events and 4 MiB of bodies in a thread; past that, only close, decline or withdraw without a body remain. An agent token limited to peers reaches only threads with those peers.",
    params(("ref" = ThreadRef, Path, description = "A thread ID or a unique prefix.")),
    request_body(content = ThreadAction, description = "The action, with an optional body."),
    responses((status = 200, description = "The thread with the event applied, without its events.", body = ThreadSummary),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn act_on_thread() {}

#[utoipa::path(get, path = "/api/v1/stream", operation_id = "streamEvents", tag = "threads",
    summary = "Stream thread changes",
    description = "Server-sent events named `thread`, each a ThreadSummary, sent whenever a thread changes: an event sent or received, or a delivery that succeeded or failed. A `: ping` comment every 15 seconds keeps idle connections open. Nothing is replayed on connect: subscribe, then list threads. A subscriber that falls behind misses changes. An agent token limited to peers gets only those peers' threads, and its stream ends once the token is revoked or expires.",
    responses((status = 200, description = "Server-sent `thread` events.", content_type = "text/event-stream", item_schema = ThreadSummary),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn stream_events() {}

#[utoipa::path(post, path = "/api/v1/federation/events", operation_id = "deliverEvent", tag = "federation",
    summary = "Deliver an event from a peer",
    description = "The peer's secret says who sent it. Delivering the same event again is harmless and answers `duplicate`. Fails with 422 when its thread is unknown or not shared with this peer, its clock is not after the peer's earlier events in the thread, or the peer has used its room in the thread; with 503, to retry later, when it is dated more than ten minutes ahead or the peering is being confirmed.",
    request_body(content = WireEvent, description = "The event, without sender or recipient."),
    responses((status = 200, description = "The event is stored.", body = Receipt),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn deliver_event() {}

#[utoipa::path(post, path = "/api/v1/federation/accepted", operation_id = "notifyPeeringAccepted", tag = "federation",
    summary = "Say a peering request was approved",
    description = "The approving daemon calls this, signed with its key, so the requester starts delivering at once.",
    responses((status = 204, description = "Noted."),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn notify_peering_accepted() {}

#[utoipa::path(get, path = "/api/v1/agent-tokens", operation_id = "listAgentTokens", tag = "tokens",
    summary = "List agent tokens",
    description = "Every agent token, without the tokens themselves.",
    responses((status = 200, description = "Agent tokens, by name.", body = AgentTokenList),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn list_agent_tokens() {}

#[utoipa::path(post, path = "/api/v1/agent-tokens", operation_id = "createAgentToken", tag = "tokens",
    summary = "Create an agent token",
    description = "A token that can send, list, read, stream and act on threads and read peers, optionally only some peers, but cannot peer, make tokens or change the owner's webhooks. Peers may be named by alias; the token keeps their names. The token is in this response only. Fails with 409 when the name is taken, 400 for an unknown peer or a past expiry.",
    request_body(content = AgentTokenInput, description = "The new token's name, peers and expiry."),
    responses((status = 201, description = "The token.", body = NewAgentToken),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn create_agent_token() {}

#[utoipa::path(delete, path = "/api/v1/agent-tokens/{name}", operation_id = "deleteAgentToken", tag = "tokens",
    summary = "Revoke an agent token",
    description = "The token stops working at once, its webhook is deleted, and its open streams end.",
    params(("name" = Name, Path, description = "The token's name.")),
    responses((status = 204, description = "Revoked."),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn delete_agent_token() {}

#[utoipa::path(delete, path = "/api/v1/me", operation_id = "revokeOwnToken", tag = "tokens",
    summary = "Revoke the calling agent token",
    description = "For agent tokens: revokes the token making the call, as deleteAgentToken would, so an integration can uninstall itself; an expired token may still. It stops working at once, its webhook is deleted, and its open streams end.",
    responses((status = 204, description = "Revoked."),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn revoke_own_token() {}

#[utoipa::path(get, path = "/api/v1/webhooks", operation_id = "listWebhooks", tag = "webhooks",
    summary = "List webhooks",
    description = "Every webhook, without URL paths, header values or signing keys.",
    responses((status = 200, description = "Webhooks, by name.", body = WebhookList),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn list_webhooks() {}

#[utoipa::path(post, path = "/api/v1/webhooks", operation_id = "createWebhook", tag = "webhooks",
    summary = "Create a webhook",
    description = "Validation errors never echo the URL, header values or key.",
    request_body(content = WebhookCreate, description = "The new webhook."),
    responses((status = 201, description = "The webhook.", body = Webhook),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn create_webhook() {}

#[utoipa::path(get, path = "/api/v1/webhooks/{name}", operation_id = "getWebhook", tag = "webhooks",
    summary = "Read a webhook",
    description = "One webhook, without its URL path, header values or signing key.",
    params(("name" = Name, Path, description = "The webhook's name.")),
    responses((status = 200, description = "The webhook.", body = Webhook),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn get_webhook() {}

#[utoipa::path(patch, path = "/api/v1/webhooks/{name}", operation_id = "updateWebhook", tag = "webhooks",
    summary = "Change a webhook",
    description = "Applies the changes atomically: an invalid result changes nothing. Any change clears a pause. Narrowing peers drops queued notifications about the others.",
    params(("name" = Name, Path, description = "The webhook's name.")),
    request_body(content = WebhookUpdate, description = "The settings to change."),
    responses((status = 200, description = "The webhook.", body = Webhook),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn update_webhook() {}

#[utoipa::path(delete, path = "/api/v1/webhooks/{name}", operation_id = "deleteWebhook", tag = "webhooks",
    summary = "Delete a webhook",
    description = "Its queued and past deliveries go with it.",
    params(("name" = Name, Path, description = "The webhook's name.")),
    responses((status = 204, description = "Deleted."),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn delete_webhook() {}

#[utoipa::path(get, path = "/api/v1/webhooks/{name}/deliveries", operation_id = "listWebhookDeliveries", tag = "webhooks",
    summary = "List a webhook's deliveries",
    description = "Delivery history from the last week, newest first, a page at a time.",
    params(("name" = Name, Path, description = "The webhook's name."), WebhookDeliveriesQuery),
    responses((status = 200, description = "A page of deliveries.", body = WebhookDeliveryList),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn list_webhook_deliveries() {}

#[utoipa::path(post, path = "/api/v1/webhooks/{name}/deliveries/{id}/retry", operation_id = "retryWebhookDelivery", tag = "webhooks",
    summary = "Retry a failed delivery",
    description = "Queues a failed delivery of an enabled webhook again, with a fresh week of retries, and clears the endpoint's pause. Fails with 409 otherwise.",
    params(("name" = Name, Path, description = "The webhook's name."), ("id" = Id, Path, description = "The delivery's ID.")),
    responses((status = 204, description = "Queued."),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn retry_webhook_delivery() {}

#[utoipa::path(post, path = "/api/v1/me/webhook", operation_id = "setOwnWebhook", tag = "webhooks",
    summary = "Register this agent token's webhook",
    description = "For agent tokens: registers, or replaces, the one webhook this token has, so an agent runner can be woken without the owner. It is generic and signed with the given key, hears only the peers' moves, and only for the token's peers. The URL must be public https. Revoking the token deletes it. The owner adds webhooks with createWebhook instead.",
    request_body(content = OwnWebhook, description = "Where to send notifications, and the signing key."),
    responses((status = 200, description = "The webhook.", body = Webhook),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn set_own_webhook() {}

#[utoipa::path(delete, path = "/api/v1/me/webhook", operation_id = "deleteOwnWebhook", tag = "webhooks",
    summary = "Remove this agent token's webhook",
    description = "For agent tokens: removes the webhook the token registered, and its deliveries.",
    responses((status = 204, description = "Removed."),
        (status = "default", description = "An error.", body = Problem, content_type = PROBLEM)))]
async fn delete_own_webhook() {}

#[utoipa::path(post, path = "notification", operation_id = "receiveWebhookNotification", tag = "webhooks",
    summary = "Receive an event notification",
    description = "Implemented by a generic webhook receiver, not by the daemon. When signing is on, verify the signature and the timestamp's freshness. Deduplicate by Webhook-Id, then durably enqueue before answering 2xx. Other destination types get their provider's message format instead.",
    params(
        ("Webhook-Id" = Id, Header, description = "The notification's ID, unchanged across retries."),
        ("Idempotency-Key" = Id, Header, description = "Webhook-Id again, for intermediaries that deduplicate retries by it."),
        ("Webhook-Timestamp" = crate::text::UnixTime, Header, description = "Unix seconds of this attempt, included in the signature."),
    ),
    request_body(content = WebhookPayload, description = "Event metadata; signed as raw bytes when a signing key is set."),
    responses((status = 204, description = "The receiver durably accepted the notification."),
        (status = "default", description = "A failed delivery: 408, 429 and 5xx are retried, honouring Retry-After on 429 and 503; anything else fails it. The body is ignored.")))]
async fn receive_webhook_notification() {}

#[derive(OpenApi)]
#[openapi(
    version = "3.2.0",
    info(
        title = "clanker-proxy API",
        description = "One API with four access levels, declared per operation and checked before the request body is read. **owner**: the daemon's owner, with its owner token. **agent**: a token the owner makes for an agent; it sends, lists, reads, streams and acts on threads, reads peers and the owner's name, and registers its own webhook, optionally only for some peers. Peering, agent tokens and the owner's webhooks stay with the owner. **peer**: another daemon, by a request signed with its Ed25519 key (see peerSignature). **public**: no token; only getHealth and requestPeering. Every error is an RFC 9457 problem.",
        license(name = "MIT", identifier = "MIT"),
    ),
    servers((url = "/", description = "The daemon")),
    tags(
        (name = "system", description = "Health and the owner."),
        (name = "peering", description = "Becoming and managing peers."),
        (name = "threads", description = "Conversations with peers."),
        (name = "federation", description = "What peers call."),
        (name = "tokens", description = "Agent tokens: threads and peers, optionally only some peers, without peering, tokens or the owner's webhooks."),
        (name = "webhooks", description = "Notifications to owner-configured endpoints."),
    ),
    paths(
        get_health, get_me, request_peering, list_requests, deny_request, approve_request,
        list_peers, add_peer, get_peer, remove_peer, set_peer_aliases, list_threads, open_thread,
        get_thread, act_on_thread, stream_events, deliver_event, notify_peering_accepted,
        list_agent_tokens, create_agent_token, delete_agent_token, revoke_own_token, list_webhooks,
        create_webhook, get_webhook, update_webhook, delete_webhook, list_webhook_deliveries,
        retry_webhook_delivery, set_own_webhook, delete_own_webhook,
    ),
    components(schemas(
        Problem, Health, HealthStatus, Me, PeeringRequest, ReceiptStatus, RequestReceipt, Request,
        RequestList, Approval, PeerInput, PeerStatus, Peer, PeerList, PeerAliases, Aliases, NewThread, ThreadAction,
        ThreadSummary, Thread, ThreadEvent, Delivery, DeliveryStatus, ThreadList, TurnFilter,
        WireEvent, Receipt, AgentTokenInput, AgentTokenSummary, NewAgentToken, AgentTokenList,
        WebhookType, WebhookEventType, WebhookEvents, WebhookOrigin, WebhookEventOrigin,
        WebhookHeader, Webhook, WebhookCreate, WebhookUpdate, OwnWebhook, WebhookList, WebhookDelivery,
        WebhookDeliveryList, WebhookPayload, Cursor, ThreadRef, crate::text::RequestRef, crate::text::UnixTime,
        State, Action, Kind, Role, Name, Id, At, Labels,
    )),
)]
struct Api;

#[derive(OpenApi)]
#[openapi(paths(receive_webhook_notification))]
struct Receiver;

fn operation_mut(item: &mut PathItem, method: Method) -> Option<&mut Operation> {
    match method {
        Method::Get => item.get.as_mut(),
        Method::Post => item.post.as_mut(),
        Method::Put => item.put.as_mut(),
        Method::Patch => item.patch.as_mut(),
        Method::Delete => item.delete.as_mut(),
    }
}

fn requirement(name: &str) -> SecurityRequirement {
    SecurityRequirement::new(name, Vec::<String>::new())
}

/// The API's OpenAPI document.
pub fn openapi() -> Document {
    let mut doc = Api::openapi();

    for op in Op::ALL {
        let Some(operation) = doc
            .paths
            .paths
            .get_mut(op.path())
            .and_then(|item| operation_mut(item, op.method()))
        else {
            continue; // the tests fail on a missing operation
        };
        operation.security = Some(match op.access() {
            Access::Public => Vec::new(),
            Access::Owner => vec![requirement("ownerToken")],
            Access::OwnerOrAgent => vec![requirement("ownerToken"), requirement("agentToken")],
            Access::Agent => vec![requirement("agentToken")],
            Access::Peer => vec![requirement("peerSignature")],
        });
    }

    let mut receiver = Receiver::openapi().paths;
    for item in receiver.paths.values_mut() {
        if let Some(op) = item.post.as_mut() {
            op.security = Some(vec![
                requirement("webhookSignature"),
                SecurityRequirement::default(),
            ]);
        }
    }
    doc.webhooks = Some(receiver);

    let bearer = |description: &str| {
        SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .description(Some(description))
                .build(),
        )
    };
    let components = doc.components.get_or_insert_with(Default::default);
    components.add_security_scheme(
        "ownerToken",
        bearer("The owner token (cpo_…), kept in the daemon's data directory."),
    );
    components.add_security_scheme(
        "agentToken",
        bearer("An agent token (cpa_…) the owner created."),
    );
    components.add_security_scheme(
        "peerSignature",
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
            "Cp-Signature",
            "Another daemon's Ed25519 signature, base64url, with Cp-Key (its public key) and Cp-Date (Unix seconds). It signs `clanker-proxy/1`, the method, the API path, Cp-Date, the recipient's public key and the body's SHA-256 in hex, one per line; a signature over five minutes old is refused.",
        ))),
    );
    components.add_security_scheme(
        "webhookSignature",
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
            "Webhook-Signature",
            "v1, then the base64 HMAC-SHA256 of id.timestamp.body under the decoded signing key (Standard Webhooks).",
        ))),
    );
    doc
}

/// The schemas the document names, so a type is not left out by mistake.
#[cfg(test)]
pub(crate) const NAMED: &[&str] = &[
    "Problem",
    "Health",
    "Me",
    "Thread",
    "ThreadSummary",
    "Event",
    "Webhook",
    "WebhookPayload",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_is_documented_with_its_access() {
        let doc = openapi();
        let mut documented = 0;
        for item in doc.paths.paths.values() {
            documented += [&item.get, &item.post, &item.patch, &item.delete, &item.put]
                .iter()
                .filter(|o| o.is_some())
                .count();
        }
        assert_eq!(
            documented,
            Op::ALL.len(),
            "documented operations differ from the table"
        );

        for op in Op::ALL {
            let mut paths = doc.paths.paths.clone();
            let item = paths
                .get_mut(op.path())
                .unwrap_or_else(|| panic!("{} not documented", op.id()));
            let operation = operation_mut(item, op.method())
                .unwrap_or_else(|| panic!("{} {} not documented", op.method(), op.path()));
            assert_eq!(operation.operation_id.as_deref(), Some(op.id()));
            let security = operation.security.as_ref().unwrap();
            assert_eq!(
                security.is_empty(),
                op.access() == Access::Public,
                "{}",
                op.id()
            );
        }
    }

    #[test]
    fn named_schemas_exist() {
        let doc = openapi();
        let schemas = &doc.components.as_ref().unwrap().schemas;
        for name in NAMED {
            assert!(schemas.contains_key(*name), "{name} missing");
        }
        assert!(
            doc.webhooks
                .as_ref()
                .is_some_and(|w| w.paths.contains_key("notification"))
        );
    }
}
