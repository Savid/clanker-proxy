//! A daemon on each host working a thread with the other: peering, delivery
//! both ways, the event stream, webhooks, agent tokens and errors; then
//! peer aliases and agent tokens revoking themselves.
//!
//! The far daemon is native unless `CP_CONFORMANCE_URL` and
//! `CP_CONFORMANCE_TOKEN` name another one; the near daemon is always native.
//! The far daemon may be reused across runs, so names here are unique per
//! run and everything made is removed at the end.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// How long anything crossing daemons may take.
const PATIENCE: Duration = Duration::from_secs(20);

/// A data directory removed when dropped.
struct Dir(std::path::PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Node {
    url: String,
    token: String,
    http: reqwest::Client,
    native: Option<(cpd::Daemon, Dir)>,
}

fn free_port() -> std::net::SocketAddr {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap()
}

fn client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

async fn native(name: &str) -> Node {
    let addr = free_port();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cp-conformance-{name}-{nanos}"));
    let url = format!("http://{addr}");
    let daemon = cpd::Daemon::start(cpd::Options {
        dir: dir.clone(),
        name: Some(cp_thread::Name::try_from(name).unwrap()),
        url: Some(cp_api::DaemonUrl::try_from(url.as_str()).unwrap()),
        listen: addr,
        version: "v0.0.0-conformance".into(),
    })
    .await
    .unwrap();
    let token = std::fs::read_to_string(dir.join(cpd::TOKEN_FILE))
        .unwrap()
        .trim()
        .to_string();
    Node {
        url,
        token,
        http: client(),
        native: Some((daemon, Dir(dir))),
    }
}

async fn far() -> Node {
    match (
        std::env::var("CP_CONFORMANCE_URL"),
        std::env::var("CP_CONFORMANCE_TOKEN"),
    ) {
        (Ok(url), Ok(token)) => Node {
            url: url.trim_end_matches('/').to_string(),
            token,
            http: client(),
            native: None,
        },
        _ => native("far").await,
    }
}

impl Node {
    async fn call_as(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(token)
            .timeout(PATIENCE);
        if let Some(b) = body {
            req = req
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call_as(&self.token, method, path, body).await
    }

    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (status, v) = self.call(method.clone(), path, body).await;
        assert!(
            (200..300).contains(&status),
            "{method} {path}: {status} {v}"
        );
        v
    }

    async fn get(&self, path: &str) -> Value {
        self.ok(Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        self.ok(Method::POST, path, Some(body)).await
    }

    async fn thread(&self, id: &str) -> Value {
        self.get(&format!("/api/v1/threads/{id}")).await
    }

    async fn stop(self) {
        if let Some((daemon, _dir)) = self.native {
            daemon.shutdown().await;
        }
    }
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

type Received = tokio::sync::mpsc::UnboundedReceiver<(Vec<(String, String)>, Vec<u8>)>;

/// Accepts HTTP requests, answers 204, and keeps their headers and bodies.
async fn receiver() -> (String, Received) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let start = loop {
                let n = s.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(i);
                }
            };
            let Some(end) = start else { continue };
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let headers: Vec<(String, String)> = head
                .lines()
                .skip(1)
                .filter_map(|l| {
                    l.split_once(':')
                        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                })
                .collect();
            let len: usize = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            while buf.len() < end + 4 + len {
                let n = s.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body = buf[end + 4..(end + 4 + len).min(buf.len())].to_vec();
            let _ = tx.send((headers, body));
            let _ = s
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        }
    });
    (url, rx)
}

/// Reads the event stream until it carries `needle`.
async fn stream_until(mut resp: reqwest::Response, needle: String) -> String {
    let mut seen = String::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if seen.contains(&needle) {
            break;
        }
    }
    seen
}

/// Approves the far daemon's pending request from the daemon at `from`,
/// with `body`, retrying while another test's approval confirms.
async fn approve(far: &Node, from: &str, body: Value) -> (u16, Value) {
    let requests = far.get("/api/v1/peering-requests").await;
    let id = requests["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["url"] == from)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/api/v1/peering-requests/{id}/approve");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let (status, v) = far.call(Method::POST, &path, Some(body.clone())).await;
        let busy = status == 409
            && v["detail"]
                .as_str()
                .is_some_and(|d| d.contains("confirming"));
        if !busy || tokio::time::Instant::now() > deadline {
            return (status, v);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // one conversation, read top to bottom
async fn a_thread_crosses_hosts_with_its_notifications() {
    let far = far().await;
    let near = native("near").await;
    // Unique per run, since the far daemon may outlive it.
    let near_as = format!("near{}", near.url.rsplit(':').next().unwrap());
    let far_name = far.get("/api/v1/me").await["name"]
        .as_str()
        .unwrap()
        .to_string();

    // Errors are problems, at the edge and in the daemon.
    let (status, p) = far.call_as("", Method::GET, "/api/v1/threads", None).await;
    assert_eq!((status, p["status"].as_u64()), (401, Some(401)), "{p}");
    let (status, p) = far
        .call(Method::GET, "/api/v1/threads/abcdef0123", None)
        .await;
    assert_eq!(status, 404, "{p}");
    let (status, _) = far.call(Method::PUT, "/api/v1/threads", None).await;
    assert_eq!(status, 405);

    // A signed webhook on the far side, for what the near side does.
    let (hook_url, mut hooks) = receiver().await;
    let key = "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE=";
    let hook = format!("hook{}", near_as.trim_start_matches("near"));
    far.post(
        "/api/v1/webhooks",
        json!({"name": hook, "type": "generic", "url": hook_url, "events": ["thread.open", "thread.close"],
               "origin": "incoming", "enabled": true, "secret": key}),
    )
    .await;

    // Peering: near asks, far approves under a name of its choosing, and
    // both see the same code.
    let asked = near
        .post(
            "/api/v1/peers",
            json!({"name": "far", "url": far.url, "note": "conformance"}),
        )
        .await;
    let requests = far.get("/api/v1/peering-requests").await;
    let request = requests["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["url"] == near.url.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        request["code"], asked["code"],
        "both owners see the same code"
    );
    let (status, p) = approve(&far, &near.url, json!({"name": near_as})).await;
    assert_eq!(status, 201, "{p}");
    assert_eq!(near.get("/api/v1/peers/far").await["status"], "active");

    // The stream is open before the thread arrives, so it carries it.
    let stream = far
        .http
        .get(format!("{}/api/v1/stream", far.url))
        .bearer_auth(&far.token)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status().as_u16(), 200);
    assert!(
        stream.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );

    let opened = near
        .post(
            "/api/v1/threads",
            json!({"to": "far", "title": "Bump the image", "body": "CI is red"}),
        )
        .await;
    let id = opened["id"].as_str().unwrap().to_string();
    let seen = tokio::time::timeout(PATIENCE, stream_until(stream, id.clone()))
        .await
        .expect("the stream carries the new thread");
    assert!(seen.starts_with("retry: 2000"), "{seen}");
    assert!(seen.contains("event: thread"), "{seen}");
    let t = far.thread(&id).await;
    assert_eq!(
        (
            t["state"].as_str(),
            t["myTurn"].as_bool(),
            t["peer"].as_str()
        ),
        (Some("open"), Some(true), Some(near_as.as_str()))
    );
    assert_eq!(
        t["sender"].as_str(),
        Some(near_as.as_str()),
        "names are local"
    );

    let (headers, body) = tokio::time::timeout(PATIENCE, hooks.recv())
        .await
        .unwrap()
        .unwrap();
    let h = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map_or_else(String::new, |(_, v)| v.clone())
    };
    let signature =
        cp_core::signature(key, &h("webhook-id"), &h("webhook-timestamp"), &body).unwrap();
    assert_eq!(
        h("webhook-signature"),
        signature,
        "signed with the shared key"
    );
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (payload["type"].as_str(), payload["subject"].as_str()),
        (Some("thread.open"), Some(id.as_str()))
    );
    assert!(
        !String::from_utf8_lossy(&body).contains("CI is red"),
        "metadata only"
    );

    // The far side answers; its events reach the near side.
    far.post(
        &format!("/api/v1/threads/{id}/events"),
        json!({"action": "ack", "body": "on it"}),
    )
    .await;
    far.post(
        &format!("/api/v1/threads/{id}/events"),
        json!({"action": "resolve", "body": "done"}),
    )
    .await;
    eventually("near sees the result", || {
        let near = &near;
        let id = id.clone();
        async move { near.thread(&id).await["state"] == "resolved" }
    })
    .await;
    eventually("far's events are delivered", || {
        let far = &far;
        let id = id.clone();
        async move { far.thread(&id).await["undelivered"] == 0 }
    })
    .await;

    near.post(
        &format!("/api/v1/threads/{id}/events"),
        json!({"action": "close", "body": "thanks"}),
    )
    .await;
    eventually("far sees it closed", || {
        let far = &far;
        let id = id.clone();
        async move { far.thread(&id).await["state"] == "closed" }
    })
    .await;
    let (_, body) = tokio::time::timeout(PATIENCE, hooks.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("thread.close"));
    let deliveries = far
        .get(&format!("/api/v1/webhooks/{hook}/deliveries"))
        .await;
    eventually("deliveries are recorded", || {
        let far = &far;
        let hook = hook.clone();
        async move {
            let d = far
                .get(&format!("/api/v1/webhooks/{hook}/deliveries"))
                .await;
            d["deliveries"]
                .as_array()
                .is_some_and(|l| l.len() == 2 && l.iter().all(|x| x["status"] == "delivered"))
        }
    })
    .await;
    drop(deliveries);

    // Each side knows which release the other runs.
    assert_eq!(
        far.get(&format!("/api/v1/peers/{near_as}")).await["version"],
        "v0.0.0-conformance"
    );
    let far_version = near.get("/api/v1/peers/far").await["version"].clone();
    assert!(
        far_version.as_str().is_some_and(|v| v.starts_with('v')),
        "{far_version}"
    );

    // An agent token scoped to the near peer sees that peer, sends to it and
    // works its threads until it is revoked, and never reaches owner
    // operations.
    let agent_name = format!("agent{}", near_as.trim_start_matches("near"));
    let agent = far
        .post(
            "/api/v1/agent-tokens",
            json!({"name": agent_name, "peers": [near_as]}),
        )
        .await;
    let token = agent["token"].as_str().unwrap().to_string();
    let (status, list) = far
        .call_as(
            &token,
            Method::GET,
            &format!("/api/v1/threads?peer={near_as}"),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert!(
        list["threads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == id.as_str())
    );
    let (status, peers) = far
        .call_as(&token, Method::GET, "/api/v1/peers", None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(peers["peers"].as_array().unwrap().len(), 1);
    assert_eq!(peers["peers"][0]["name"], near_as.as_str());
    let (status, _) = far
        .call_as(
            &token,
            Method::GET,
            &format!("/api/v1/peers/{near_as}"),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let (status, sent) = far
        .call_as(
            &token,
            Method::POST,
            "/api/v1/threads",
            Some(json!({"to": near_as, "title": "from an agent", "body": "hello"})),
        )
        .await;
    assert_eq!(status, 201, "{sent}");
    let (status, _) = far
        .call_as(
            &token,
            Method::POST,
            "/api/v1/threads",
            Some(json!({"to": "nobody", "title": "t", "body": "b"})),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = far
        .call_as(&token, Method::GET, "/api/v1/agent-tokens", None)
        .await;
    assert_eq!(status, 403);
    far.ok(
        Method::DELETE,
        &format!("/api/v1/agent-tokens/{agent_name}"),
        None,
    )
    .await;
    let (status, _) = far
        .call_as(&token, Method::GET, "/api/v1/threads", None)
        .await;
    assert_eq!(status, 401);

    // Tidy the far daemon, which may be reused.
    far.ok(Method::DELETE, &format!("/api/v1/webhooks/{hook}"), None)
        .await;
    far.ok(Method::DELETE, &format!("/api/v1/peers/{near_as}"), None)
        .await;
    assert_ne!(far_name, near_as);
    near.stop().await;
    far.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)] // one owner's session, read top to bottom
async fn aliases_name_peers_and_agent_tokens_revoke_themselves() {
    let far = far().await;
    let near = native("near").await;
    // Unique per run, since the far daemon may outlive it.
    let run = near.url.rsplit(':').next().unwrap().to_string();
    let other = native(&format!("other{run}")).await;
    let (near_as, other_as) = (format!("near{run}"), format!("o{run}"));
    let (alias, other_name) = (format!("bobby{run}"), format!("other{run}"));
    let aliases = |name: &str| format!("/api/v1/peers/{name}/aliases");

    near.post("/api/v1/peers", json!({"name": "far", "url": far.url}))
        .await;
    let (status, p) = approve(&far, &near.url, json!({"name": near_as})).await;
    assert_eq!(status, 201, "{p}");
    assert_eq!(p["aliases"], json!([]), "always present");

    // Aliases replace the peer's set, sorted and each once, and name the
    // peer wherever a peer's name goes.
    let p = far
        .ok(
            Method::PUT,
            &aliases(&near_as),
            Some(json!({"aliases": [other_name, alias, alias]})),
        )
        .await;
    assert_eq!(p["name"], near_as.as_str());
    assert_eq!(p["aliases"], json!([alias, other_name]));
    let p = far.get(&format!("/api/v1/peers/{alias}")).await;
    assert_eq!(
        (p["name"].as_str(), &p["aliases"]),
        (Some(near_as.as_str()), &json!([alias, other_name]))
    );
    let listed = far.get("/api/v1/peers").await;
    assert!(
        listed["peers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == near_as.as_str() && p["aliases"][0] == alias.as_str())
    );
    let opened = far
        .post(
            "/api/v1/threads",
            json!({"to": alias, "title": "Sent to an alias", "body": "hi"}),
        )
        .await;
    let id = opened["id"].as_str().unwrap().to_string();
    assert_eq!(opened["peer"], near_as.as_str(), "stored by name");
    assert_eq!(far.thread(&id).await["recipient"], near_as.as_str());
    let by_alias = far.get(&format!("/api/v1/threads?peer={alias}")).await;
    assert!(
        by_alias["threads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == id.as_str())
    );
    eventually("near gets the thread", || {
        let near = &near;
        let path = format!("/api/v1/threads/{id}");
        async move { near.call(Method::GET, &path, None).await.0 == 200 }
    })
    .await;

    // Peers' names and aliases share one namespace.
    let (status, p) = far
        .call(
            Method::POST,
            "/api/v1/peers",
            Some(json!({"name": alias, "url": "http://127.0.0.1:9"})),
        )
        .await;
    assert_eq!(status, 409, "{p}");
    assert!(
        p["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("{alias} is an alias of {near_as}")),
        "{p}"
    );
    other
        .post("/api/v1/peers", json!({"name": "far", "url": far.url}))
        .await;
    let (status, p) = approve(&far, &other.url, json!({})).await;
    assert_eq!(status, 409, "the name they asked for is an alias: {p}");
    assert!(
        p["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("{other_name} is an alias of {near_as}")),
        "{p}"
    );
    let (status, p) = approve(&far, &other.url, json!({"name": alias})).await;
    assert_eq!(status, 409, "{p}");
    let (status, p) = approve(&far, &other.url, json!({"name": other_as})).await;
    assert_eq!(status, 201, "{p}");
    for (peer, set) in [
        (near_as.as_str(), json!([other_as])),
        (other_as.as_str(), json!([alias])),
    ] {
        let (status, p) = far
            .call(Method::PUT, &aliases(peer), Some(json!({"aliases": set})))
            .await;
        assert_eq!(status, 409, "{peer} {set}: {p}");
    }
    let (status, _) = far
        .call(
            Method::PUT,
            &aliases(&format!("nobody{run}")),
            Some(json!({"aliases": []})),
        )
        .await;
    assert_eq!(status, 404);
    let eleven: Vec<String> = (0..11).map(|i| format!("a{i}x{run}")).collect();
    let (status, _) = far
        .call(
            Method::PUT,
            &aliases(&near_as),
            Some(json!({"aliases": eleven})),
        )
        .await;
    assert_eq!(status, 400);

    // An agent token limited to the other peer: near's alias is no peer to
    // it.
    let other_alias = format!("oo{run}");
    far.ok(
        Method::PUT,
        &aliases(&other_as),
        Some(json!({"aliases": [other_alias]})),
    )
    .await;
    let scoped_name = format!("scoped{run}");
    let scoped = far
        .post(
            "/api/v1/agent-tokens",
            json!({"name": scoped_name, "peers": [other_alias]}),
        )
        .await;
    assert_eq!(scoped["peers"], json!([other_as]), "kept by name");
    let scoped = scoped["token"].as_str().unwrap().to_string();
    let (status, _) = far
        .call_as(
            &scoped,
            Method::GET,
            &format!("/api/v1/peers/{alias}"),
            None,
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = far
        .call_as(
            &scoped,
            Method::POST,
            "/api/v1/threads",
            Some(json!({"to": alias, "title": "t", "body": "b"})),
        )
        .await;
    assert_eq!(status, 404);
    let (status, list) = far
        .call_as(
            &scoped,
            Method::GET,
            &format!("/api/v1/threads?peer={alias}"),
            None,
        )
        .await;
    assert_eq!((status, &list["threads"]), (200, &json!([])));
    let (status, p) = far
        .call_as(
            &scoped,
            Method::GET,
            &format!("/api/v1/peers/{other_alias}"),
            None,
        )
        .await;
    assert_eq!((status, p["name"].as_str()), (200, Some(other_as.as_str())));

    // An agent token revokes itself, and its webhook goes with it. Its peer
    // sends nothing, so the webhook is never called.
    let (status, p) = far
        .call_as(
            &scoped,
            Method::POST,
            "/api/v1/me/webhook",
            Some(json!({"url": "https://hooks.example.com/cp-conformance",
                        "secret": "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE="})),
        )
        .await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(
        far.get(&format!("/api/v1/webhooks/{scoped_name}")).await["agent"],
        scoped_name.as_str()
    );
    let (status, _) = far.call(Method::DELETE, "/api/v1/me", None).await;
    assert_eq!(status, 401, "the owner token is not an agent token");
    let (status, _) = far
        .call_as(&scoped, Method::DELETE, "/api/v1/me", None)
        .await;
    assert_eq!(status, 204);
    let (status, _) = far
        .call_as(&scoped, Method::GET, "/api/v1/threads", None)
        .await;
    assert_eq!(status, 401);
    let (status, _) = far
        .call(
            Method::GET,
            &format!("/api/v1/webhooks/{scoped_name}"),
            None,
        )
        .await;
    assert_eq!(status, 404);
    let tokens = far.get("/api/v1/agent-tokens").await;
    assert!(
        !tokens["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == scoped_name.as_str())
    );

    // Removing a peer, by an alias, removes its aliases.
    far.ok(
        Method::DELETE,
        &format!("/api/v1/peers/{other_alias}"),
        None,
    )
    .await;
    for name in [&other_as, &other_alias] {
        let (status, _) = far
            .call(Method::GET, &format!("/api/v1/peers/{name}"), None)
            .await;
        assert_eq!(status, 404, "{name}");
    }
    let p = far
        .ok(
            Method::PUT,
            &aliases(&alias),
            Some(json!({"aliases": [alias, other_alias]})),
        )
        .await;
    assert_eq!(p["aliases"], json!([alias, other_alias]), "free again");

    // Tidy the far daemon, which may be reused.
    far.ok(Method::DELETE, &format!("/api/v1/peers/{near_as}"), None)
        .await;
    let (status, _) = far
        .call(Method::GET, &format!("/api/v1/peers/{alias}"), None)
        .await;
    assert_eq!(status, 404);
    other.stop().await;
    near.stop().await;
    far.stop().await;
}
