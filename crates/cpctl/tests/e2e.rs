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
        version: "v0.0.0-test".into(),
    })
    .await
    .unwrap();
    let env = Env {
        url: Some(url.clone()),
        token: None,
        dir: Some(dir.to_string_lossy().into()),
        ..Env::default()
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
async fn aliases_name_a_peer_everywhere() {
    let (alice, bob) = peered().await;
    let out = ok(&alice, &["peer", "alias", "bob", "bobby", "b"]).await;
    assert!(out.starts_with("bob is also called b, bobby\n"), "{out}");
    assert!(out.contains("cpctl send b "), "{out}");
    let ls = ok(&alice, &["peer", "ls"]).await;
    assert!(ls.contains("ALIASES") && ls.contains("b,bobby"), "{ls}");
    let show = ok(&alice, &["peer", "show", "bobby"]).await;
    assert!(
        show.contains("b,bobby") && show.contains("cpctl send bob "),
        "{show}"
    );

    let sent = json(&alice, &["send", "bobby", "via an alias"]).await;
    assert_eq!(sent["peer"], "bob");
    let listed = json(&alice, &["ls", "--peer", "b"]).await;
    assert_eq!(listed["threads"][0]["id"], sent["id"]);

    let rm = cp(&alice, &["peer", "rm", "bobby"]).await;
    assert_eq!(rm.exit, cpctl::EXIT_REFUSED, "{}", rm.stderr);
    assert!(
        rm.stderr.contains("bobby is an alias of bob")
            && rm
                .stderr
                .contains("cpctl peer unalias bob bobby removes only the alias"),
        "{}",
        rm.stderr
    );
    assert_eq!(json(&alice, &["peer", "show", "bob"]).await["name"], "bob");

    let taken = cp(&alice, &["peer", "alias", "bob", "alice"]).await;
    assert_eq!(taken.exit, cpctl::EXIT_REFUSED, "{}", taken.stderr);
    assert_eq!(
        cp(&alice, &["peer", "unalias", "bob", "carl"]).await.exit,
        cpctl::EXIT_NOT_FOUND
    );
    let many: Vec<String> = (0..10).map(|i| format!("x{i}")).collect();
    let mut line = vec!["peer", "alias", "bob"];
    line.extend(many.iter().map(String::as_str));
    assert_eq!(cp(&alice, &line).await.exit, cpctl::EXIT_USAGE);

    let out = ok(&alice, &["peer", "unalias", "b", "b", "bobby"]).await;
    assert!(out.starts_with("bob has no aliases\n"), "{out}");
    assert_eq!(
        json(&alice, &["peer", "show", "bob"]).await["aliases"],
        serde_json::json!([])
    );
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

#[tokio::test]
async fn agent_tokens_send_and_work_threads_but_not_peering() {
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
        me.stdout.contains("agent token: reaches bob"),
        "{}",
        me.stdout
    );
    let o = cp_in(&agent, &["peer", "ls"], "").await;
    assert_eq!(o.exit, 0, "{}", o.stderr);
    assert!(o.stdout.contains("bob"), "{}", o.stdout);
    let o = cp_in(&agent, &["send", "bob", "from the agent", "-m", "hi"], "").await;
    assert_eq!(o.exit, 0, "{}", o.stderr);
    assert_eq!(cp_in(&agent, &["peer", "show", "bob"], "").await.exit, 0);
    let o = cp_in(&agent, &["send", "carol", "t", "-m", "b"], "").await;
    assert_eq!(o.exit, cpctl::EXIT_NOT_FOUND, "{}", o.stderr);
    assert!(
        o.stderr.contains("the peers this token reaches"),
        "{}",
        o.stderr
    );
    assert_eq!(cp_in(&agent, &["inbox"], "").await.exit, 0);
    for owner_only in [&["token", "ls"][..], &["requests"], &["webhook", "ls"]] {
        let o = cp_in(&agent, owner_only, "").await;
        assert_eq!(o.exit, cpctl::EXIT_REFUSED, "{owner_only:?}: {}", o.stderr);
        assert!(
            o.stderr
                .contains("peering, tokens and webhooks are the owner's"),
            "{}",
            o.stderr
        );
    }

    ok(&alice, &["token", "rm", "runner"]).await;
    let o = cp_in(&agent, &["inbox"], "").await;
    assert_eq!(o.exit, cpctl::EXIT_AUTH);
    assert!(o.stderr.contains("ask the owner"), "{}", o.stderr);
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn webhooks_are_configured_without_echoing_secrets() {
    let (alice, bob) = peered().await;
    let key = alice.dir.0.join("hook.key");
    std::fs::write(&key, "YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE=\n").unwrap();
    let headers = alice.dir.0.join("headers.json");
    std::fs::write(&headers, r#"{"x-api-key":"hunter2"}"#).unwrap();
    // A receiver nobody listens on: deliveries fail and retry.
    let out = cp_in(
        &alice.env,
        &[
            "webhook",
            "add",
            "runner",
            "--url-file",
            "-",
            "--secret-file",
            key.to_str().unwrap(),
            "--headers-file",
            headers.to_str().unwrap(),
            "--events",
            "thread.open,thread.overruled",
            "--origin",
            "both",
            "--peers",
            "bob",
        ],
        "http://127.0.0.1:9/hook?token=sekrit\n",
    )
    .await;
    assert_eq!(out.exit, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("runner → http://127.0.0.1:9 (generic"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("headers: X-Api-Key (values hidden)"),
        "{}",
        out.stdout
    );
    for secret in ["sekrit", "hunter2", "YWFh"] {
        assert!(
            !out.stdout.contains(secret),
            "{secret} leaked: {}",
            out.stdout
        );
    }

    let two_stdin = cp(
        &alice,
        &[
            "webhook",
            "add",
            "x",
            "--url-file",
            "-",
            "--secret-file",
            "-",
        ],
    )
    .await;
    assert_eq!(two_stdin.exit, cpctl::EXIT_USAGE);
    let signed_chat = cp(
        &alice,
        &[
            "webhook",
            "add",
            "chat",
            "https://hooks.slack.com/x",
            "--type",
            "slack",
            "--secret-file",
            key.to_str().unwrap(),
        ],
    )
    .await;
    assert_eq!(
        signed_chat.exit,
        cpctl::EXIT_USAGE,
        "{}",
        signed_chat.stderr
    );
    assert_eq!(
        cp(&alice, &["webhook", "set", "runner"]).await.exit,
        cpctl::EXIT_USAGE
    );

    let ls = ok(&alice, &["webhook", "ls"]).await;
    assert!(
        ls.contains("runner") && ls.contains("thread.open,thread.overruled") && ls.contains("bob"),
        "{ls}"
    );

    ok(&alice, &["send", "bob", "hello"]).await;
    let deliveries = json(&alice, &["webhook", "deliveries", "runner"]).await;
    assert_eq!(deliveries["deliveries"][0]["event"], "thread.open");

    let off = json(
        &alice,
        &[
            "webhook",
            "set",
            "runner",
            "--enabled=false",
            "--signing=false",
        ],
    )
    .await;
    assert_eq!(
        (off["enabled"].as_bool(), off["signing"].as_bool()),
        (Some(false), Some(false))
    );
    assert!(
        ok(&alice, &["webhook", "show", "runner"])
            .await
            .contains("paused:")
    );

    let types = json(&alice, &["webhook", "types"]).await;
    assert_eq!(types["types"].as_array().unwrap().len(), 10);
    assert!(
        ok(&alice, &["webhook", "events"])
            .await
            .contains("thread.overruled")
    );
    assert_eq!(cp(&alice, &["webhook"]).await.exit, cpctl::EXIT_USAGE);

    ok(&alice, &["webhook", "rm", "runner"]).await;
    assert_eq!(
        cp(&alice, &["webhook", "show", "runner"]).await.exit,
        cpctl::EXIT_NOT_FOUND
    );
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

/// Answers every request with a release whose tag is `tag`.
async fn release_server(tag: &'static str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/latest", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await;
            let body = format!(r#"{{"tag_name":"{tag}"}}"#);
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes()).await;
        }
    });
    url
}

#[tokio::test]
async fn owners_hear_about_newer_releases_and_peers() {
    let (alice, bob) = peered().await;
    let cache = alice.dir.0.join("cache").join("latest-release");
    let env = Env {
        releases: Some(release_server("v9.9.9").await),
        cache: Some(cache.to_string_lossy().into()),
        ..alice.env.clone()
    };
    let me = cp_in(&env, &["me"], "").await;
    assert!(
        me.stdout
            .contains("update available: v9.9.9 (this cpd runs v0.0.0-test)"),
        "{}",
        me.stdout
    );
    assert!(
        std::fs::read_to_string(&cache)
            .unwrap()
            .ends_with(" v9.9.9\n")
    );
    // The answer is remembered: a dead release server changes nothing.
    let offline = Env {
        releases: Some("http://127.0.0.1:9/latest".into()),
        ..env.clone()
    };
    assert!(cp_in(&offline, &["me"], "").await.stdout.contains("v9.9.9"));
    // JSON stays the API's answer.
    let json = cp_in(&env, &["--json", "me"], "").await;
    assert!(!json.stdout.contains("update available"), "{}", json.stdout);

    // Peers report their release; the column shows it.
    let peers = ok(&alice, &["peer", "ls"]).await;
    assert!(
        peers.contains("VERSION") && peers.contains("v0.0.0-test"),
        "{peers}"
    );
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

/// An environment with only a configuration directory: what a second
/// machine has before and after `cpctl login`.
fn bare(config: &std::path::Path) -> Env {
    Env {
        config: Some(config.to_string_lossy().into()),
        ..Env::default()
    }
}

fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn login_saves_a_daemon_that_flags_and_the_environment_override() {
    let (alice, bob) = (node("alice").await, node("bob").await);
    let config = alice.dir.0.join("config");
    let env = bare(&config);
    let token_file = alice.dir.0.join(cpd::TOKEN_FILE);
    let token = std::fs::read_to_string(&token_file).unwrap();
    let file = config.join("clanker-proxy").join("login");

    // A token cpd refuses saves nothing.
    let wrong = alice.dir.0.join("wrong.token");
    std::fs::write(&wrong, format!("cpo_{}\n", "x".repeat(43))).unwrap();
    let o = cp_in(
        &env,
        &["login", &alice.url, "--token-file", wrong.to_str().unwrap()],
        "",
    )
    .await;
    assert_eq!(o.exit, cpctl::EXIT_AUTH, "{}", o.stderr);
    assert!(o.stderr.contains("nothing was saved"), "{}", o.stderr);
    assert!(!file.exists());

    let o = cp_in(
        &env,
        &[
            "login",
            &alice.url,
            "--token-file",
            token_file.to_str().unwrap(),
        ],
        "",
    )
    .await;
    assert_eq!(o.exit, 0, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.starts_with(&format!(
            "logged in to alice with the owner token at {}",
            alice.url
        )),
        "{}",
        o.stdout
    );
    assert!(!o.stdout.contains(token.trim()) && !o.stderr.contains(token.trim()));
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(file.parent().unwrap()), 0o700);

    // Later commands use it, with nothing in the environment.
    let o = cp_in(&env, &["me"], "").await;
    assert_eq!(o.exit, 0, "{}", o.stderr);
    assert!(
        o.stdout.starts_with(&format!("alice at {}", alice.url)),
        "{}",
        o.stdout
    );

    // A URL or token from the environment or a flag replaces the login
    // whole: the saved token never goes to another daemon.
    let mut elsewhere = bare(&config);
    elsewhere.url = Some(bob.url.clone());
    elsewhere.dir = Some(bob.dir.0.to_string_lossy().into());
    let o = cp_in(&elsewhere, &["me"], "").await;
    assert!(o.stdout.starts_with("bob at"), "{}{}", o.stdout, o.stderr);
    let o = cp_in(&env, &["--url", &bob.url, "me"], "").await;
    assert_eq!(
        o.exit,
        cpctl::EXIT_AUTH,
        "only bob's own token reaches bob: {}",
        o.stdout
    );

    let o = cp_in(&env, &["logout"], "").await;
    assert!(
        o.stdout
            .starts_with(&format!("logged out of {}: removed ", alice.url)),
        "{}",
        o.stdout
    );
    assert!(!file.exists());
    let o = cp_in(&env, &["logout"], "").await;
    assert!(o.stdout.starts_with("no saved login"), "{}", o.stdout);
    alice.daemon.shutdown().await;
    bob.daemon.shutdown().await;
}

#[tokio::test]
async fn login_refuses_what_it_cannot_use_safely() {
    let alice = node("alice").await;
    let config = alice.dir.0.join("config");
    let env = bare(&config);

    // No terminal and no file: nowhere to read the token from.
    let o = cp_in(&env, &["login", &alice.url], "").await;
    assert_eq!(o.exit, cpctl::EXIT_USAGE, "{}", o.stderr);
    assert!(
        o.stderr.contains("needs a terminal") && o.stderr.contains("--token-file"),
        "{}",
        o.stderr
    );

    // A token on the command line would stay in shell history.
    let o = cp_in(&env, &["login", &alice.url, "--token", "cpo_x"], "").await;
    assert_eq!(o.exit, cpctl::EXIT_USAGE);
    assert!(o.stderr.contains("takes no --token"), "{}", o.stderr);

    // A file without a token is refused without showing what it holds.
    let junk = alice.dir.0.join("junk");
    std::fs::write(&junk, "hunter2\n").unwrap();
    let o = cp_in(
        &env,
        &["login", &alice.url, "--token-file", junk.to_str().unwrap()],
        "",
    )
    .await;
    assert_eq!(o.exit, cpctl::EXIT_USAGE);
    assert!(!o.stderr.contains("hunter2"), "{}", o.stderr);

    // An unreadable saved login is an error, not a quiet fall back; logout
    // still clears it.
    let file = config.join("clanker-proxy").join("login");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "nonsense\n").unwrap();
    let o = cp_in(&env, &["me"], "").await;
    assert_eq!(o.exit, cpctl::EXIT_ERROR);
    assert!(o.stderr.contains("unreadable"), "{}", o.stderr);
    let o = cp_in(&env, &["logout"], "").await;
    assert_eq!(o.exit, 0);
    assert!(!file.exists());
    alice.daemon.shutdown().await;
}
