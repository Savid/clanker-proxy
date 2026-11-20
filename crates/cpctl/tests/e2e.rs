//! cpctl against real daemons on localhost.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use cpctl::{Env, Outcome};
use cpd::{Daemon, Options};
use serde_json::Value;

struct Node {
    daemon: Daemon,
    env: Env,
    url: String,
    dir: Dir,
}

/// A data directory removed when dropped.
struct Dir(std::path::PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn node(name: &str) -> Node {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("cpctl-test-{name}-{}-{nanos}", std::process::id()));
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let url = format!("http://{addr}");
    let daemon = Daemon::start(Options {
        dir: dir.clone(),
        name: Some(cp_thread::Name::try_from(name).unwrap()),
        url: Some(cp_api::DaemonUrl::try_from(url.as_str()).unwrap()),
        listen: addr,
        version: "test".into(),
    })
    .await
    .unwrap();
    let env = Env {
        url: Some(url.clone()),
        token: None,
        dir: Some(dir.to_string_lossy().into()),
        home: None,
    };
    Node {
        daemon,
        env,
        url,
        dir: Dir(dir),
    }
}

async fn cp_in(env: &Env, args: &[&str], stdin: &str) -> Outcome {
    let line: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let mut input = stdin.as_bytes();
    let mut sink = |_: &str| {};
    cpctl::run(&line, env, &mut input, &mut sink).await
}

async fn cp(n: &Node, args: &[&str]) -> Outcome {
    cp_in(&n.env, args, "").await
}

async fn ok(n: &Node, args: &[&str]) -> String {
    let o = cp(n, args).await;
    assert_eq!(o.exit, 0, "{args:?}: {}{}", o.stdout, o.stderr);
    o.stdout
}

async fn json(n: &Node, args: &[&str]) -> Value {
    let mut line = vec!["--json"];
    line.extend_from_slice(args);
    serde_json::from_str(&ok(n, &line).await).unwrap()
}

async fn peered() -> (Node, Node) {
    let (alice, bob) = (node("alice").await, node("bob").await);
    let out = ok(
        &alice,
        &["peer", "add", "bob", &bob.url, "-m", "hi, it's alice"],
    )
    .await;
    let code = out
        .split("code ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let reqs = json(&bob, &["requests"]).await;
    assert_eq!(
        reqs["requests"][0]["code"],
        code.as_str(),
        "both owners see the same code"
    );
    let id = reqs["requests"][0]["id"].as_str().unwrap().to_string();
    ok(&bob, &["approve", &id]).await;
    assert_eq!(
        json(&alice, &["peer", "show", "bob"]).await["status"],
        "active"
    );
    (alice, bob)
}

#[tokio::test]
async fn help_is_the_manual() {
    let env = Env::default();
    let manual = cp_in(&env, &["help"], "").await.stdout;
    for name in cpctl::command_names() {
        assert!(manual.contains(&format!("  {name}")), "manual lists {name}");
        let words: Vec<&str> = std::iter::once("help").chain(name.split(' ')).collect();
        let h = cp_in(&env, &words, "").await;
        assert!(
            h.stdout.starts_with(&format!("cpctl {name}")),
            "{name}: {}",
            h.stdout
        );
        assert!(
            h.stdout.contains(&format!("Example:\n  cpctl {name}")),
            "{name} has an example"
        );
    }
    assert!(
        cp_in(&env, &["send", "-h"], "")
            .await
            .stdout
            .contains("Example:")
    );
    for bad in [
        &["frobnicate"][..],
        &["peer"],
        &["show"],
        &["ls", "--bogus"],
    ] {
        let o = cp_in(&env, bad, "").await;
        assert_eq!(o.exit, cpctl::EXIT_USAGE, "{bad:?}");
        assert!(
            o.stderr.contains("hint: cpctl help"),
            "{bad:?}: {}",
            o.stderr
        );
    }
}

#[tokio::test]
async fn a_request_runs_its_course_through_the_cli() {
    let (alice, bob) = peered().await;
    let sent = ok(
        &alice,
        &[
            "send",
            "bob",
            "Bump the reth image",
            "-m",
            "CI is red",
            "-l",
            "infra",
        ],
    )
    .await;
    assert!(
        sent.contains("next:\n  cpctl wait ") && sent.contains("cpctl withdraw "),
        "{sent}"
    );
    let id = sent.split_whitespace().next().unwrap().to_string();
    let r = &id[..8];

    let waited = cp(&bob, &["wait", "--timeout", "5s"]).await;
    assert_eq!(waited.exit, 0, "{}", waited.stderr);
    assert!(
        waited.stdout.starts_with("your turn on:"),
        "{}",
        waited.stdout
    );

    let shown = ok(&bob, &["show", r]).await;
    assert!(
        shown.contains(&format!(r#"cpctl resolve {r} -m "<result>""#)),
        "{shown}"
    );
    assert!(
        shown.contains("  │ CI is red"),
        "bodies are marked: {shown}"
    );

    ok(&bob, &["ack", r]).await;
    ok(&bob, &["needs-input", r, "-m", "which tag?"]).await;
    assert!(
        cp(&alice, &["wait", r, "--timeout", "5s"])
            .await
            .stdout
            .contains("asked you something")
    );
    ok(&alice, &["reply", r, "-m", "v1.2"]).await;
    let o = cp_in(&bob.env, &["resolve", r, "-m", "-"], "done in #4312\n").await;
    assert_eq!(o.exit, 0, "{}", o.stderr);
    assert!(
        cp(&alice, &["wait", r, "--timeout", "5s"])
            .await
            .stdout
            .contains("says it is done")
    );
    ok(&alice, &["close", r]).await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    let a = json(&alice, &["show", r]).await;
    let b = json(&bob, &["show", r]).await;
    assert_eq!(
        (a["state"].as_str(), b["state"].as_str()),
        (Some("closed"), Some("closed"))
    );
    assert_eq!(a["events"], 6);
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

#[tokio::test]
async fn json_output_and_errors() {
    let (alice, bob) = peered().await;
    let t = json(&alice, &["send", "bob", "t", "--json"]).await;
    assert_eq!(t["role"], "sender");
    assert_eq!(
        t["actions"],
        serde_json::json!(["comment", "close", "withdraw"])
    );
    assert!(t.get("log").is_none());

    let o = cp(&alice, &["--json", "show", "zzzz"]).await;
    assert_eq!(
        o.exit,
        cpctl::EXIT_USAGE,
        "zzzz is not a valid ref: {}",
        o.stderr
    );
    let o = cp(&alice, &["--json", "show", "ffff"]).await;
    assert_eq!(o.exit, cpctl::EXIT_NOT_FOUND);
    let e: Value = serde_json::from_str(&o.stderr).unwrap();
    assert_eq!(e["error"]["status"], 404);
    assert!(e["error"]["hint"].as_str().is_some_and(|h| !h.is_empty()));
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

#[tokio::test]
async fn exit_codes_say_what_happened() {
    let (alice, bob) = peered().await;
    let id = json(&alice, &["send", "bob", "t"]).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = &id[..8];
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        cp(&alice, &["ack", r]).await.exit,
        cpctl::EXIT_REFUSED,
        "only the recipient may ack"
    );
    let refused = cp(&alice, &["ack", r]).await;
    assert!(
        refused.stderr.contains("you can now")
            && refused.stderr.contains(&format!("cpctl show {r}")),
        "{}",
        refused.stderr
    );
    assert_eq!(
        cp(&alice, &["peer", "show", "nobody"]).await.exit,
        cpctl::EXIT_NOT_FOUND
    );
    assert_eq!(
        cp(&alice, &["reply", r, "-m", "  "]).await.exit,
        cpctl::EXIT_USAGE,
        "whitespace body refused locally"
    );

    let mut bad = alice.env.clone();
    bad.token = Some(format!("cpo_{}", "A".repeat(43)));
    assert_eq!(cp_in(&bad, &["inbox"], "").await.exit, cpctl::EXIT_AUTH);
    assert_eq!(
        cp_in(&bad, &["wait", "--timeout", "1s"], "").await.exit,
        cpctl::EXIT_AUTH,
        "the stream refuses too"
    );

    let gone = Env {
        url: Some("http://127.0.0.1:9".into()),
        ..alice.env.clone()
    };
    assert_eq!(
        cp_in(&gone, &["inbox"], "").await.exit,
        cpctl::EXIT_UNREACHABLE
    );
    let plain = Env {
        url: Some("http://cp.example.com".into()),
        ..alice.env.clone()
    };
    assert_eq!(
        cp_in(&plain, &["inbox"], "").await.exit,
        cpctl::EXIT_USAGE,
        "plain http only to this machine"
    );

    assert_eq!(
        cp(&alice, &["wait", r, "--timeout", "1s"]).await.exit,
        cpctl::EXIT_TIMEOUT
    );
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

#[tokio::test]
async fn agent_tokens_work_threads_only() {
    let (alice, bob) = peered().await;
    let file = alice.dir.0.join("runner.token");
    let path = file.to_string_lossy().to_string();
    let out = ok(
        &alice,
        &["token", "add", "runner", "-o", &path, "--peers", "bob"],
    )
    .await;
    let token = std::fs::read_to_string(&file).unwrap().trim().to_string();
    assert!(
        token.starts_with("cpa_") && !out.contains(&token),
        "the token stays out of output"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        cp(&alice, &["token", "add", "again", "-o", &path])
            .await
            .exit,
        cpctl::EXIT_USAGE,
        "an existing file is not overwritten"
    );
    assert_eq!(
        cp(&alice, &["token", "add", "x", "-o", "-", "--peers", ""])
            .await
            .exit,
        cpctl::EXIT_USAGE,
        "an empty scope is refused"
    );

    let agent = Env {
        token: Some(token),
        ..alice.env.clone()
    };
    let me = cp_in(&agent, &["me"], "").await;
    assert!(
        me.stdout.contains("agent token: threads with bob"),
        "{}",
        me.stdout
    );
    let o = cp_in(&agent, &["peer", "ls"], "").await;
    assert_eq!(o.exit, cpctl::EXIT_REFUSED, "{}", o.stderr);
    assert_eq!(cp_in(&agent, &["inbox"], "").await.exit, 0);

    ok(&alice, &["token", "rm", "runner"]).await;
    let o = cp_in(&agent, &["inbox"], "").await;
    assert_eq!(o.exit, cpctl::EXIT_AUTH);
    assert!(o.stderr.contains("ask the owner"), "{}", o.stderr);
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}
