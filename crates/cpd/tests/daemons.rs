//! Two real daemons on localhost, driven over HTTP.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use cp_api::{Access, Op, ThreadSummary, prefix};
use cpd::{Daemon, Options};
use serde_json::{Value, json};

struct Node {
    daemon: Daemon,
    token: String,
    url: String,
    _dir: TempDir,
}

/// A directory removed when dropped.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    TempDir(std::env::temp_dir().join(format!("cpd-test-{tag}-{}-{nanos}", std::process::id())))
}

async fn node(name: &str) -> Node {
    let dir = temp_dir(name);
    // Bind first to learn the port, so the daemon can be told its own URL.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let url = format!("http://{addr}");
    let daemon = Daemon::start(Options {
        dir: dir.0.clone(),
        name: Some(cp_thread::Name::try_from(name).unwrap()),
        url: Some(cp_api::DaemonUrl::try_from(url.as_str()).unwrap()),
        listen: addr,
        version: "test".into(),
    })
    .await
    .unwrap();
    let token = std::fs::read_to_string(dir.0.join(cpd::TOKEN_FILE))
        .unwrap()
        .trim()
        .to_string();
    Node {
        daemon,
        token,
        url,
        _dir: dir,
    }
}

fn client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}

impl Node {
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call_as(&self.token.clone(), method, path, body).await
    }

    async fn call_as(
        &self,
        token: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = client()
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (status, v) = self.call(reqwest::Method::GET, path, None).await;
        assert_eq!(status, 200, "GET {path}: {v}");
        v
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let (status, v) = self.call(reqwest::Method::POST, path, Some(body)).await;
        assert!((200..300).contains(&status), "POST {path}: {status} {v}");
        v
    }

    async fn thread(&self, id: &str) -> ThreadSummary {
        serde_json::from_value(self.get(&format!("/api/v1/threads/{id}")).await).unwrap()
    }
}

/// Waits until `f` holds, polling: delivery runs in the background.
async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn peered() -> (Node, Node) {
    let (alice, bob) = (node("alice").await, node("bob").await);
    let peer = alice
        .post(
            "/api/v1/peers",
            json!({"name": "bob", "url": bob.url, "note": "hi"}),
        )
        .await;
    let requests = bob.get("/api/v1/peering-requests").await;
    let req = &requests["requests"][0];
    assert_eq!(req["code"], peer["code"], "codes match");
    let id = req["id"].as_str().unwrap();
    bob.post(&format!("/api/v1/peering-requests/{id}/approve"), json!({}))
        .await;
    let p = alice.get("/api/v1/peers/bob").await;
    assert_eq!(p["status"], "active");
    (alice, bob)
}

#[tokio::test]
async fn two_daemons_run_a_thread() {
    let (alice, bob) = peered().await;
    let sent = alice
        .post(
            "/api/v1/threads",
            json!({"to": "bob", "title": "Bump the image", "body": "CI is red"}),
        )
        .await;
    let id = sent["id"].as_str().unwrap().to_string();

    eventually("bob holds the thread", || async {
        bob.call(reqwest::Method::GET, &format!("/api/v1/threads/{id}"), None)
            .await
            .0
            == 200
    })
    .await;
    for (who, action, body, state) in [
        (&bob, "ack", "", "acked"),
        (&bob, "resolve", "done in #1", "resolved"),
        (&alice, "close", "", "closed"),
    ] {
        who.post(
            &format!("/api/v1/threads/{id}/events"),
            json!({"action": action, "body": body}),
        )
        .await;
        let want = state.to_string();
        eventually(&format!("both {state}"), || {
            let (a, b, id, want) = (&alice, &bob, id.clone(), want.clone());
            async move {
                a.thread(&id).await.state.as_str() == want
                    && b.thread(&id).await.state.as_str() == want
            }
        })
        .await;
    }
    let t = alice.get(&format!("/api/v1/threads/{id}")).await;
    assert_eq!(t["log"].as_array().unwrap().len(), 4);
    assert_eq!(t["undelivered"], 0);
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

#[tokio::test]
async fn errors_are_problems() {
    let a = node("carol").await;
    let (status, v) = a
        .call(
            reqwest::Method::POST,
            "/api/v1/threads",
            Some(json!({"to": "nobody", "title": "t"})),
        )
        .await;
    assert_eq!(status, 404);
    assert_eq!(v["title"], "Not Found");
    let (status, _) = a.call(reqwest::Method::PUT, "/api/v1/threads", None).await;
    assert_eq!(status, 405);
    let (status, _) = a.call(reqwest::Method::GET, "/api/v1/nope", None).await;
    assert_eq!(status, 404);
    let resp = client()
        .post(format!("{}/api/v1/threads", a.url))
        .bearer_auth(&a.token)
        .body("{}")
        .header("content-type", "text/plain")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 415);
    let resp = client()
        .get(format!("{}/api/v1/me", a.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    assert!(resp.headers().contains_key("www-authenticate"));
    assert!(resp.headers().contains_key("x-request-id"));
    a.daemon.shutdown().await;
}

/// Every operation in the table, with every kind of caller, before any body
/// is read: public operations admit anyone, others refuse with 401, and a
/// valid agent token on an owner-only operation gets 403.
#[tokio::test]
async fn access_is_checked_for_every_operation() {
    let a = node("dan").await;
    let agent = a
        .post("/api/v1/agent-tokens", json!({"name": "runner"}))
        .await;
    let agent = agent["token"].as_str().unwrap().to_string();
    let fake_owner = format!("{}{}", prefix::OWNER, "A".repeat(43));
    let fake_agent = format!("{}{}", prefix::AGENT, "A".repeat(43));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();

    for op in Op::ALL {
        let path = op
            .path()
            .replace("{name}", "nobody")
            .replace("{id}", "00000000")
            .replace("{ref}", "abcd");
        let method = reqwest::Method::from_bytes(op.method().as_str().as_bytes()).unwrap();
        for (who, token) in [
            ("nobody", ""),
            ("garbage owner", fake_owner.as_str()),
            ("bad agent", fake_agent.as_str()),
            ("forged peer", ""),
            ("agent", agent.as_str()),
            ("owner", a.token.as_str()),
        ] {
            if *op == Op::StreamEvents && matches!(who, "agent" | "owner") {
                continue; // admitted, and a stream would not end
            }
            let mut req = client().request(method.clone(), format!("{}{path}", a.url));
            if !token.is_empty() {
                req = req.bearer_auth(token);
            }
            if who == "forged peer" {
                req = req
                    .header(cp_api::KEY_HEADER, "A".repeat(43))
                    .header(cp_api::DATE_HEADER, &now)
                    .header(cp_api::SIGNATURE_HEADER, "A".repeat(86));
            }
            let status = req.send().await.unwrap().status().as_u16();
            let admitted = match op.access() {
                Access::Public => true,
                Access::Owner => who == "owner",
                Access::OwnerOrAgent => matches!(who, "owner" | "agent"),
                Access::Peer => false,
            };
            if admitted {
                assert!(
                    status != 401 && status != 403,
                    "{} as {who}: {status}",
                    op.id()
                );
            } else if op.access() == Access::Owner && who == "agent" {
                assert_eq!(status, 403, "{} as {who}", op.id());
            } else {
                assert_eq!(status, 401, "{} as {who}", op.id());
            }
        }
    }
    a.daemon.shutdown().await;
}

#[tokio::test]
async fn the_stream_carries_changes_and_ends_when_its_token_is_revoked() {
    let (alice, bob) = peered().await;
    let agent = alice
        .post("/api/v1/agent-tokens", json!({"name": "watcher"}))
        .await;
    let token = agent["token"].as_str().unwrap().to_string();
    let mut resp = client()
        .get(format!("{}/api/v1/stream", alice.url))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");

    let sent = alice
        .post("/api/v1/threads", json!({"to": "bob", "title": "streamed"}))
        .await;
    let mut seen = String::new();
    while !seen.contains("event: thread") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(seen.starts_with("retry: 2000\n\n"), "{seen}");
    assert!(seen.contains(sent["id"].as_str().unwrap()), "{seen}");

    let (status, _) = alice
        .call(
            reqwest::Method::DELETE,
            "/api/v1/agent-tokens/watcher",
            None,
        )
        .await;
    assert_eq!(status, 204);
    // The next change finds the token revoked and ends the stream.
    alice
        .post("/api/v1/threads", json!({"to": "bob", "title": "after"}))
        .await;
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match resp.chunk().await {
                Ok(Some(c)) if String::from_utf8_lossy(&c).contains("after") => return false,
                Ok(Some(_)) => {}
                _ => return true,
            }
        }
    })
    .await
    .unwrap();
    assert!(ended, "a revoked token's stream got another event");
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

/// Accepts HTTP requests and keeps their headers and bodies.
async fn receiver() -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<(Vec<(String, String)>, Vec<u8>)>,
) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
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
            let (head, body_start) = loop {
                let n = s.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break (String::new(), 0);
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break (String::from_utf8_lossy(&buf[..i]).to_string(), i + 4);
                }
            };
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
            while buf.len() < body_start + len {
                let n = s.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let _ = tx.send((
                headers,
                buf[body_start..(body_start + len).min(buf.len())].to_vec(),
            ));
            let _ = s
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        }
    });
    (url, rx)
}

#[tokio::test]
async fn webhooks_reach_a_receiver_signed() {
    let (alice, bob) = peered().await;
    let (url, mut got) = receiver().await;
    let key = "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE=";
    bob.post(
        "/api/v1/webhooks",
        json!({"name": "runner", "type": "generic", "url": url, "events": ["thread.open"], "origin": "incoming", "enabled": true, "secret": key}),
    )
    .await;
    alice
        .post(
            "/api/v1/threads",
            json!({"to": "bob", "title": "private title", "body": "private body"}),
        )
        .await;
    let (headers, body) = tokio::time::timeout(Duration::from_secs(10), got.recv())
        .await
        .unwrap()
        .unwrap();
    let h = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map_or_else(String::new, |(_, v)| v.clone())
    };
    let want = cp_core::signature(key, &h("webhook-id"), &h("webhook-timestamp"), &body).unwrap();
    assert_eq!(h("webhook-signature"), want);
    assert_eq!(h("idempotency-key"), h("webhook-id"));
    assert_eq!(headers.iter().filter(|(k, _)| k == "user-agent").count(), 1);
    let text = String::from_utf8(body).unwrap();
    assert!(
        text.contains("\"type\":\"thread.open\"") && !text.contains("private"),
        "{text}"
    );
    eventually("delivery recorded", || {
        let bob = &bob;
        async move {
            bob.get("/api/v1/webhooks/runner/deliveries").await["deliveries"][0]["status"]
                == "delivered"
        }
    })
    .await;
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}
