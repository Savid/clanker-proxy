//! cpctl: drives a clanker-proxy daemon for its owner. Written for coding
//! agents: every output ends with the commands that usually follow, and
//! every failure carries an exit code and a hint.

mod cli;
mod client;
mod fail;
mod output;
mod update;
mod webhooks;

use std::fmt::Write as _;
use std::io::Read;
use std::time::Duration;

use cp_api::{
    AgentTokenInput, Approval, Me, NewAgentToken, NewThread, Peer, PeerInput, PeerList, Peers,
    RequestList, ThreadAction, ThreadList, ThreadSummary,
};
use cp_thread::{Action, At, Body, Kind, Label, Name, Title};
use serde::Serialize;

use crate::cli::{COMMANDS, Parsed};
use crate::client::{Client, segment};
pub use crate::fail::{
    EXIT_AUTH, EXIT_ERROR, EXIT_NOT_FOUND, EXIT_REFUSED, EXIT_TIMEOUT, EXIT_UNREACHABLE,
    EXIT_USAGE, Failure,
};
use crate::output::{Step, next, step, thread_steps};

/// What a run printed and how it ended.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub exit: u8,
    pub stdout: String,
    pub stderr: String,
}

/// Where cpctl finds cpd and its token.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub url: Option<String>,
    pub token: Option<String>,
    pub dir: Option<String>,
    pub home: Option<String>,
    /// Where to ask for the newest release; `None` skips the check.
    pub releases: Option<String>,
    /// Where to remember the answer for a day.
    pub cache: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let var = |k| std::env::var(k).ok().filter(|v: &String| !v.is_empty());
        Self {
            url: var("CP_URL"),
            token: var("CP_TOKEN"),
            dir: var("CP_DIR"),
            home: var("HOME"),
            releases: var("CP_NO_UPDATE_CHECK")
                .is_none()
                .then(|| update::LATEST_API.to_string()),
            cache: var("XDG_CACHE_HOME")
                .or_else(|| var("HOME").map(|h| format!("{h}/.cache")))
                .map(|d| format!("{d}/clanker-proxy/latest-release")),
        }
    }

    fn token(&self) -> String {
        if let Some(t) = &self.token {
            return t.trim().to_string();
        }
        let dir = self
            .dir
            .clone()
            .or_else(|| self.home.as_ref().map(|h| format!("{h}/.cp")));
        dir.and_then(|d| std::fs::read_to_string(format!("{d}/owner.token")).ok())
            .map_or_else(String::new, |t| t.trim().to_string())
    }
}

/// Runs one command line (without the program name). `watch` returns only
/// when its stream ends for good; `out` receives each line as it happens.
pub async fn run(
    argv: &[String],
    env: &Env,
    stdin: &mut dyn Read,
    out: &mut dyn FnMut(&str),
) -> Outcome {
    let parsed = match cli::parse(argv) {
        Ok(p) => p,
        Err(f) => return report(&f, argv.iter().any(|a| a == "--json")),
    };
    let json = parsed.set("json");
    if parsed.is_help() {
        let words = if parsed.command.is_some() {
            vec![parsed.command.map_or("", |c| c.name).to_string()]
        } else {
            parsed.words.iter().skip(1).cloned().collect()
        };
        return match cli::help_for(&words) {
            Ok(text) => Outcome {
                exit: 0,
                stdout: text,
                stderr: String::new(),
            },
            Err(f) => report(&f, json),
        };
    }
    let Some(command) = parsed.command else {
        return report(&Failure::usage("no command"), json);
    };
    let url = parsed
        .value("url")
        .map(str::to_string)
        .or_else(|| env.url.clone())
        .unwrap_or_else(|| "http://127.0.0.1:18471".into());
    let token = parsed
        .value("token")
        .map_or_else(|| env.token(), str::to_string);
    let mut app = match Client::new(&url, &token, command.name) {
        Ok(client) => App {
            client,
            json,
            stdout: String::new(),
            updates: env.releases.clone().map(|url| update::Updates {
                url,
                cache: env.cache.clone().map(std::path::PathBuf::from),
            }),
        },
        Err(f) => return report(&f, json),
    };
    match app.exec(command.name, &parsed, stdin, out).await {
        Ok(()) => Outcome {
            exit: 0,
            stdout: app.stdout,
            stderr: String::new(),
        },
        Err(mut f) => {
            if f.exit == EXIT_USAGE && f.hint.is_none() {
                f.hint = Some(format!("cpctl help {}", command.name));
            }
            let mut o = report(&f, json);
            o.stdout = app.stdout;
            o
        }
    }
}

fn report(f: &Failure, json: bool) -> Outcome {
    let stderr = if json {
        #[derive(Serialize)]
        struct E<'a> {
            message: &'a str,
            exit: u8,
            #[serde(skip_serializing_if = "Option::is_none")]
            status: Option<u16>,
            #[serde(skip_serializing_if = "Option::is_none")]
            hint: Option<&'a str>,
        }
        let e = serde_json::json!({ "error": E { message: &f.message, exit: f.exit, status: f.status, hint: f.hint.as_deref() } });
        format!("{e}\n")
    } else {
        let mut s = format!("cpctl: {}\n", f.message);
        if let Some(h) = &f.hint {
            let _ = writeln!(s, "hint: {h}");
        }
        s
    };
    Outcome {
        exit: f.exit,
        stdout: String::new(),
        stderr,
    }
}

struct App {
    client: Client,
    json: bool,
    stdout: String,
    updates: Option<update::Updates>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn name(s: &str) -> Result<Name, Failure> {
    Name::normalize(s).map_err(|e| Failure::usage(e.to_string()))
}

fn action_of(command: &str) -> Option<Action> {
    Some(match command {
        "reply" => Action::Comment,
        "ack" => Action::Ack,
        "needs-input" => Action::NeedsInput,
        "resolve" => Action::Resolve,
        "decline" => Action::Decline,
        "close" => Action::Close,
        "reopen" => Action::Reopen,
        "withdraw" => Action::Withdraw,
        _ => return None,
    })
}

/// The `--peers` list: comma-separated names, or `*` for every peer. Set but
/// empty is refused, so an unset shell variable cannot widen a scope.
fn peers_flag(p: &Parsed) -> Result<Peers, Failure> {
    let Some(raw) = p.value("peers") else {
        return Ok(Peers::default());
    };
    if raw.trim() == "*" {
        return Ok(Peers::default());
    }
    let names: Vec<Name> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(name)
        .collect::<Result<_, _>>()?;
    if names.is_empty() {
        return Err(Failure::usage(
            "--peers is set but names no peer; use '*' for every peer",
        ));
    }
    Peers::try_from(names).map_err(|e| Failure::usage(e.to_string()))
}

impl App {
    /// Prints cpd's answer as it came with `--json`, else as text.
    fn print(&mut self, json_text: &str, text: impl FnOnce(&mut String)) {
        if self.json {
            self.stdout.push_str(json_text.trim());
            self.stdout.push('\n');
        } else {
            text(&mut self.stdout);
        }
    }

    /// A command whose answer has no body: `{}` with `--json`.
    fn done(&mut self, message: &str, steps: &[Step]) {
        if self.json {
            self.stdout.push_str("{}\n");
        } else {
            let _ = writeln!(self.stdout, "{message}");
            next(&mut self.stdout, steps);
        }
    }

    fn body(p: &Parsed, stdin: &mut dyn Read) -> Result<Body, Failure> {
        let raw = match p.value("message") {
            None => String::new(),
            Some("-") => {
                let mut buf = Vec::new();
                let limit = u64::try_from(4 * cp_thread::MAX_BODY + 1).unwrap_or(u64::MAX);
                stdin
                    .take(limit)
                    .read_to_end(&mut buf)
                    .map_err(|e| Failure::new(EXIT_ERROR, format!("read body: {e}")))?;
                String::from_utf8_lossy(&buf)
                    .trim_end_matches('\n')
                    .to_string()
            }
            Some(m) => m.to_string(),
        };
        Body::try_from(raw).map_err(|e| Failure::usage(e.to_string()))
    }

    async fn exec(
        &mut self,
        command: &'static str,
        p: &Parsed,
        stdin: &mut dyn Read,
        out: &mut dyn FnMut(&str),
    ) -> Result<(), Failure> {
        let arg = |i: usize| p.args.get(i).cloned().unwrap_or_default();
        if let Some(action) = action_of(command) {
            return self.act(action, &arg(0), p, stdin).await;
        }
        match command {
            "me" => self.me().await,
            "peer add" => self.peer_add(&arg(0), &arg(1), p).await,
            "peer ls" => self.peer_ls().await,
            "peer show" => self.peer_show(&arg(0)).await,
            "peer rm" => {
                let n = name(&arg(0))?;
                self.client.delete(&format!("/api/v1/peers/{n}")).await?;
                self.done(
                    &format!("removed {n}; threads with them are kept"),
                    &[step("cpctl peer ls", "see the remaining peers")],
                );
                Ok(())
            }
            "requests" => self.requests().await,
            "approve" => self.approve(&arg(0), p).await,
            "deny" => {
                let id = arg(0);
                self.client
                    .delete(&format!("/api/v1/peering-requests/{}", segment(&id)?))
                    .await?;
                self.done(
                    &format!("denied {id}"),
                    &[step("cpctl requests", "see any other requests")],
                );
                Ok(())
            }
            "inbox" => self.list("?turn=mine".into(), true, 100).await,
            "ls" => self.ls(p).await,
            "show" => self.show(&arg(0), p.set("all")).await,
            "wait" => self.wait(p.args.first().cloned(), p).await,
            "watch" => self.watch(out).await,
            "send" => self.send(&arg(0), &arg(1), p, stdin).await,
            "token add" => self.token_add(&arg(0), p).await,
            "token ls" => self.token_ls().await,
            "token rm" => {
                let n = name(&arg(0))?;
                self.client
                    .delete(&format!("/api/v1/agent-tokens/{n}"))
                    .await?;
                self.done(
                    &format!("revoked {n}"),
                    &[step("cpctl token ls", "see the remaining tokens")],
                );
                Ok(())
            }
            c if c.starts_with("webhook ") => self.webhook(c, p, stdin).await,
            _ => Err(Failure::usage(format!("no command {command:?}"))),
        }
    }

    /// The newest release, unless checks are off or the output is JSON.
    async fn latest(&self) -> Option<cp_api::Version> {
        match (&self.updates, self.json) {
            (Some(u), false) => u.latest().await,
            _ => None,
        }
    }

    async fn me(&mut self) -> Result<(), Failure> {
        let (me, raw): (Me, _) = self.client.get("/api/v1/me").await?;
        let agent = self.client.is_agent();
        let notice = update::notice(self.latest().await.as_ref(), &me.version);
        self.print(&raw, |w| {
            if let Some(n) = &notice {
                let _ = writeln!(w, "{n}");
            }
            if agent {
                let scope = me.peers.as_ref().filter(|p| !p.is_empty()).map_or_else(
                    || "every peer".to_string(),
                    |p| p.as_slice().iter().map(Name::as_str).collect::<Vec<_>>().join(", "),
                );
                let _ = writeln!(w, "{} (agent token: reaches {scope})", me.name);
                next(
                    w,
                    &[
                        step("cpctl inbox", "see what is waiting on you"),
                        step(r#"cpctl send <peer> "<title>" -m "<details>""#, "ask a peer for something"),
                    ],
                );
                return;
            }
            match &me.url {
                None => {
                    let _ = writeln!(w, "{}; this daemon has no public URL, so nobody can connect to it", me.name);
                    next(w, &[step("restart cpd with --url https://<where peers reach it>", "then they can connect")]);
                }
                Some(u) => {
                    let _ = writeln!(w, "{} at {u}\nFor someone to connect, their agent runs: cpctl peer add {} {u}", me.name, me.name);
                    next(w, &[step("cpctl requests", "see who has asked to connect")]);
                }
            }
        });
        Ok(())
    }

    async fn peer_add(&mut self, n: &str, url: &str, p: &Parsed) -> Result<(), Failure> {
        let input = PeerInput {
            name: name(n)?,
            url: cp_api::DaemonUrl::try_from(url).map_err(|e| Failure::usage(e.to_string()))?,
            note: p
                .value("message")
                .map(cp_api::Note::try_from)
                .transpose()
                .map_err(|e| Failure::usage(e.to_string()))?,
        };
        let (peer, raw): (Peer, _) = self.client.post("/api/v1/peers", &input).await?;
        self.print(&raw, |w| {
            let (n, code) = (&peer.name, &peer.code);
            let _ = writeln!(w, "{n}'s daemon at {} has your peering request; code {code}", peer.url);
            let _ = writeln!(
                w,
                "{n} stays requested until their owner approves it. Before that, your owner and {n}'s owner should\nconfirm over a channel they trust (chat, a call) that both see code {code}; {n}'s owner sees it in cpctl requests.\nIf the codes differ, remove the peer: someone else may have answered. Threads sent to {n} now are\ndelivered once they approve."
            );
            next(w, &[step(format!("cpctl peer show {n}"), "status becomes active once approved"), step(format!("cpctl peer rm {n}"), "if the codes differ")]);
        });
        Ok(())
    }

    async fn peer_ls(&mut self) -> Result<(), Failure> {
        let (l, raw): (PeerList, _) = self.client.get("/api/v1/peers").await?;
        // Peers on a newer release are the nudge to update: they may rely on
        // fixes this daemon lacks.
        let mut ahead = Vec::new();
        if !self.json && l.peers.iter().any(|p| p.version.is_some()) {
            let mine = self.client.get::<Me>("/api/v1/me").await.ok();
            if let Some(mine) = mine.and_then(|(m, _)| cp_api::Version::try_from(m.version).ok()) {
                for p in &l.peers {
                    if let Some(v) = p.version.as_ref().filter(|v| v.newer_than(&mine)) {
                        ahead.push(format!(
                            "{} runs {v}, newer than this cpd ({mine}): update, see {}",
                            p.name,
                            update::RELEASES
                        ));
                    }
                }
            }
        }
        self.print(&raw, |w| {
            if l.peers.is_empty() {
                w.push_str("no peers\n");
                next(
                    w,
                    &[
                        step("cpctl peer add <name> <url>", "connect to someone"),
                        step("cpctl requests", "see who has asked"),
                    ],
                );
                return;
            }
            output::peers(w, &l.peers);
            for line in &ahead {
                let _ = writeln!(w, "{line}");
            }
            next(
                w,
                &[
                    step(
                        r#"cpctl send <name> "<title>" -m "<details>""#,
                        "ask one of them for something",
                    ),
                    step("cpctl peer show <name>", "one peer's code"),
                ],
            );
        });
        Ok(())
    }

    async fn peer_show(&mut self, n: &str) -> Result<(), Failure> {
        let n = name(n)?;
        let (peer, raw): (Peer, _) = self.client.get(&format!("/api/v1/peers/{n}")).await?;
        self.print(&raw, |w| {
            output::peers(w, std::slice::from_ref(&peer));
            next(
                w,
                &[
                    step(
                        format!(r#"cpctl send {n} "<title>" -m "<details>""#),
                        "ask them for something",
                    ),
                    step(format!("cpctl ls --peer {n}"), "threads with them"),
                ],
            );
        });
        Ok(())
    }

    async fn requests(&mut self) -> Result<(), Failure> {
        let (l, raw): (RequestList, _) = self.client.get("/api/v1/peering-requests").await?;
        self.print(&raw, |w| {
            if l.requests.is_empty() {
                w.push_str("no peering requests\n");
                next(
                    w,
                    &[step("cpctl me", "what someone needs to connect to you")],
                );
                return;
            }
            output::requests(w, &l.requests, now());
            w.push_str(
                "Approve only a request whose code your owner has confirmed with that person.\n",
            );
            next(
                w,
                &[
                    step(
                        "cpctl approve <id> [--as <name>]",
                        "accept; --as names them differently",
                    ),
                    step("cpctl deny <id>", "refuse"),
                ],
            );
        });
        Ok(())
    }

    async fn approve(&mut self, id: &str, p: &Parsed) -> Result<(), Failure> {
        let approval = Approval {
            name: p.value("as").map(name).transpose()?,
        };
        let (peer, raw): (Peer, _) = self
            .client
            .post(
                &format!("/api/v1/peering-requests/{}/approve", segment(id)?),
                &approval,
            )
            .await?;
        self.print(&raw, |w| {
            let _ = writeln!(
                w,
                "{} is now a peer ({}). Threads they sent while waiting arrive within seconds.",
                peer.name, peer.url
            );
            next(
                w,
                &[
                    step(
                        "cpctl wait --timeout 1m",
                        "pick up anything they already sent",
                    ),
                    step(
                        format!(r#"cpctl send {} "<title>" -m "<details>""#, peer.name),
                        "ask them for something",
                    ),
                ],
            );
        });
        Ok(())
    }

    async fn ls(&mut self, p: &Parsed) -> Result<(), Failure> {
        let limit: u32 = match p.value("limit") {
            None => 50,
            Some(n) => n
                .parse()
                .map_err(|_| Failure::usage(format!("-n {n:?}: want a number")))?,
        };
        let mut q = vec![format!("limit={}", limit.clamp(1, 500))];
        for (flag, key) in [
            ("turn", "turn"),
            ("state", "state"),
            ("peer", "peer"),
            ("label", "label"),
        ] {
            if let Some(v) = p.value(flag) {
                let v = v.trim().to_lowercase();
                if !v
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-._/".contains(&c))
                {
                    return Err(Failure::usage(format!("--{flag} {v:?} is not valid")));
                }
                q.push(format!("{key}={v}"));
            }
        }
        self.list(format!("?{}", q.join("&")), false, limit.clamp(1, 500))
            .await
    }

    async fn list(&mut self, query: String, inbox: bool, limit: u32) -> Result<(), Failure> {
        let (l, raw): (ThreadList, _) = self.client.get(&format!("/api/v1/threads{query}")).await?;
        self.print(&raw, |w| {
            if l.threads.is_empty() {
                if inbox {
                    w.push_str("nothing is waiting on you\n");
                    next(
                        w,
                        &[step("cpctl wait --timeout 30m", "block until something is")],
                    );
                } else {
                    w.push_str("no threads match\n");
                    next(w, &[step("cpctl ls", "list every thread")]);
                }
                return;
            }
            output::threads(w, &l.threads, now());
            if u32::try_from(l.threads.len()).unwrap_or(u32::MAX) >= limit {
                let more = if inbox {
                    "cpctl ls --turn mine -n 500 shows up to 500"
                } else {
                    "narrow with --turn, --state, --peer or --label, or raise -n (up to 500)"
                };
                let _ = writeln!(w, "showing the newest {limit}; there may be more: {more}");
            }
            next(
                w,
                &[step("cpctl show <id>", "read one and see what you can do")],
            );
        });
        Ok(())
    }

    async fn show(&mut self, r: &str, all: bool) -> Result<(), Failure> {
        self.client.thread_ref = Some(r.to_string());
        let path = format!("/api/v1/threads/{}", segment(r)?);
        let (mut t, raw): (cp_api::Thread, String) =
            self.client.get(&format!("{path}?limit=20")).await?;
        let raw = if all && t.log_start > 0 {
            let (full, raw): (cp_api::Thread, String) =
                self.client.get(&format!("{path}?limit=1000")).await?;
            t = full;
            raw
        } else {
            raw
        };
        self.print(&raw, |w| {
            output::summary(w, &t.summary);
            if t.log_start > 0 {
                let _ = write!(
                    w,
                    "\n({} earlier {}; cpctl show {r} --all shows them)\n",
                    t.log_start,
                    output::plural(t.log_start, "event")
                );
            }
            output::log(w, &t.log);
            w.push('\n');
            let mut steps = thread_steps(&t.summary);
            let refused = t.log.iter().filter_map(|e| e.delivery.as_ref()).any(|d| {
                d.status != cp_api::DeliveryStatus::Delivered
                    && d.last_error
                        .as_deref()
                        .is_some_and(|m| m.starts_with("401 ") || m.starts_with("403 "))
            });
            if refused {
                steps.insert(
                    0,
                    step(
                        format!("cpctl peer show {}", t.summary.peer),
                        "their daemon refuses this one: they may have removed you",
                    ),
                );
            }
            next(w, &steps);
        });
        Ok(())
    }

    async fn send(
        &mut self,
        to: &str,
        title: &str,
        p: &Parsed,
        stdin: &mut dyn Read,
    ) -> Result<(), Failure> {
        let n = NewThread {
            to: name(to)?,
            title: Title::try_from(title.trim()).map_err(|e| Failure::usage(e.to_string()))?,
            kind: p.set("fyi").then_some(Kind::Fyi),
            labels: p
                .values("label")
                .iter()
                .map(|l| {
                    Label::try_from(l.trim().to_lowercase())
                        .map_err(|e| Failure::usage(e.to_string()))
                })
                .collect::<Result<_, _>>()?,
            body: Self::body(p, stdin)?,
        };
        let (t, raw): (ThreadSummary, _) = self.client.post("/api/v1/threads", &n).await?;
        let waiting = if self.json {
            false
        } else {
            self.client
                .get::<Peer>(&format!("/api/v1/peers/{}", t.peer))
                .await
                .is_ok_and(|(peer, _)| peer.status == cp_api::PeerStatus::Requested)
        };
        self.print(&raw, |w| {
            output::summary(w, &t);
            if waiting {
                let _ = writeln!(w, "{} has not approved your peering request yet; this thread is delivered once they do", t.peer);
            }
            next(w, &thread_steps(&t));
        });
        Ok(())
    }

    async fn act(
        &mut self,
        action: Action,
        r: &str,
        p: &Parsed,
        stdin: &mut dyn Read,
    ) -> Result<(), Failure> {
        let body = Self::body(p, stdin)?;
        if action.needs_body() && body.is_blank() {
            let cmd = self.client.command.clone();
            return Err(Failure::usage(format!(
                r#"{cmd} needs a body: -m "<text>", or -m - to read stdin"#
            )));
        }
        self.client.thread_ref = Some(r.to_string());
        let (t, raw): (ThreadSummary, _) = self
            .client
            .post(
                &format!("/api/v1/threads/{}/events", segment(r)?),
                &ThreadAction { action, body },
            )
            .await?;
        self.print(&raw, |w| {
            output::summary(w, &t);
            next(w, &thread_steps(&t));
        });
        Ok(())
    }

    async fn token_add(&mut self, n: &str, p: &Parsed) -> Result<(), Failure> {
        let file = p
            .value("output")
            .ok_or_else(|| Failure::usage("token add needs -o <file>, or -o - to print the token"))?
            .to_string();
        let expires = match p.value("expires") {
            None => None,
            Some(v) => {
                let d = cli::duration(v)
                    .filter(|d| d.as_secs() >= 60 && d.as_secs() <= 3650 * 86_400)
                    .ok_or_else(|| {
                        Failure::usage(format!(
                            "--expires {v:?}: want a lifetime from 1m to 3650d, e.g. 30d or 12h"
                        ))
                    })?;
                Some(
                    At::from_unix_seconds(now() + i64::try_from(d.as_secs()).unwrap_or(0))
                        .map_err(|e| Failure::usage(e.to_string()))?,
                )
            }
        };
        let input = AgentTokenInput {
            name: name(n)?,
            peers: peers_flag(p)?,
            expires_at: expires,
        };
        // The file exists before the token does, so a token never goes
        // nowhere; if writing fails, the token is revoked.
        let mut handle = if file == "-" {
            None
        } else {
            Some(create_private(&file)?)
        };
        let (t, raw): (NewAgentToken, _) =
            match self.client.post("/api/v1/agent-tokens", &input).await {
                Ok(v) => v,
                Err(f) => {
                    if handle.is_some() {
                        let _ = std::fs::remove_file(&file);
                    }
                    return Err(f);
                }
            };
        if let Some(h) = handle.as_mut() {
            use std::io::Write as _;
            if let Err(e) = h
                .write_all(format!("{}\n", t.token.expose()).as_bytes())
                .and_then(|()| h.sync_all())
            {
                let _ = std::fs::remove_file(&file);
                let _ = self
                    .client
                    .delete(&format!("/api/v1/agent-tokens/{}", t.name))
                    .await;
                return Err(Failure::new(
                    EXIT_ERROR,
                    format!("write {file}: {e}; the token was revoked"),
                ));
            }
        } else {
            let _ = writeln!(self.stdout, "{}", t.token.expose());
            return Ok(());
        }
        let redacted = raw.replace(t.token.expose(), "[written to file]");
        self.print(&redacted, |w| {
            let scope = if t.peers.is_empty() {
                "every peer".to_string()
            } else {
                t.peers
                    .as_slice()
                    .iter()
                    .map(Name::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = writeln!(
                w,
                "agent token {} written to {file} (threads with {scope})",
                t.name
            );
            next(
                w,
                &[
                    step(format!(r#"CP_TOKEN="$(cat {file})" cpctl inbox"#), "use it"),
                    step(format!("cpctl token rm {}", t.name), "revoke it"),
                ],
            );
        });
        Ok(())
    }

    async fn token_ls(&mut self) -> Result<(), Failure> {
        let (l, raw): (cp_api::AgentTokenList, _) = self.client.get("/api/v1/agent-tokens").await?;
        self.print(&raw, |w| {
            if l.tokens.is_empty() {
                w.push_str("no agent tokens\n");
            } else {
                let mut rows = vec![vec![
                    "NAME".into(),
                    "PEERS".into(),
                    "CREATED".into(),
                    "EXPIRES".into(),
                    "USED".into(),
                ]];
                for t in &l.tokens {
                    let peers = if t.peers.is_empty() {
                        "*".to_string()
                    } else {
                        t.peers
                            .as_slice()
                            .iter()
                            .map(Name::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    };
                    rows.push(vec![
                        t.name.to_string(),
                        peers,
                        t.created_at.to_string(),
                        t.expires_at.map_or("never".into(), |a| a.to_string()),
                        t.used_at.map_or("never".into(), |a| output::ago(now(), a)),
                    ]);
                }
                output::table(w, &rows);
            }
            next(
                w,
                &[
                    step("cpctl token add <name> -o <file>", "make one"),
                    step("cpctl token rm <name>", "revoke one"),
                ],
            );
        });
        Ok(())
    }

    async fn wait(&mut self, r: Option<String>, p: &Parsed) -> Result<(), Failure> {
        let timeout = match p.value("timeout") {
            None => None,
            Some(v) => Some(cli::duration(v).ok_or_else(|| {
                Failure::usage(format!("--timeout {v:?}: want a duration such as 30m"))
            })?),
        };
        let found = match timeout {
            None => self.wait_until(r.as_deref()).await?,
            Some(d) => tokio::time::timeout(d, self.wait_until(r.as_deref()))
                .await
                .map_err(|_| {
                    Failure::new(EXIT_TIMEOUT, "timed out waiting")
                        .hint("nothing changed in time; run the same wait again to keep waiting")
                })??,
        };
        let raw = serde_json::to_string(&found).unwrap_or_default();
        self.print(&raw, |w| {
            w.push_str(if found.my_turn {
                "your turn on:\n"
            } else {
                "ended:\n"
            });
            output::summary(w, &found);
            let mut steps = vec![step(
                format!("cpctl show {}", output::short(found.id.as_str())),
                "read what changed",
            )];
            steps.extend(thread_steps(&found));
            next(w, &steps);
        });
        Ok(())
    }

    /// Subscribes, then checks the current state, so nothing between the two
    /// is missed; then follows the stream. Reconnects a dropped stream, but
    /// reports a daemon it never reached.
    async fn wait_until(&self, r: Option<&str>) -> Result<ThreadSummary, Failure> {
        let mut reached = false;
        loop {
            match self.wait_once(r).await {
                Ok(t) => return Ok(t),
                Err((f, opened)) => {
                    reached |= opened;
                    if f.status.is_some_and(|s| s < 500)
                        || (!reached && f.exit == EXIT_UNREACHABLE)
                        || f.exit == EXIT_USAGE
                    {
                        return Err(f);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn wait_once(&self, r: Option<&str>) -> Result<ThreadSummary, (Failure, bool)> {
        let mut stream = Events::open(&self.client).await.map_err(|f| (f, false))?;
        let ready = |t: &ThreadSummary| t.my_turn || t.turn.is_none();
        let unanswered = |t: &ThreadSummary| t.my_turn && t.last_from == t.peer;
        let id = if let Some(r) = r {
            let path = format!(
                "/api/v1/threads/{}?limit=1",
                segment(r).map_err(|f| (f, true))?
            );
            let (t, _): (cp_api::Thread, _) =
                self.client.get(&path).await.map_err(|f| (f, true))?;
            if ready(&t.summary) {
                return Ok(t.summary);
            }
            Some(t.summary.id)
        } else {
            let (l, _): (ThreadList, _) = self
                .client
                .get("/api/v1/threads?turn=mine&limit=500")
                .await
                .map_err(|f| (f, true))?;
            if let Some(t) = l.threads.into_iter().find(unanswered) {
                return Ok(t);
            }
            None
        };
        loop {
            let t = stream.next().await.map_err(|f| (f, true))?;
            let done = match &id {
                Some(id) => t.id == *id && ready(&t),
                None => unanswered(&t),
            };
            if done {
                return Ok(t);
            }
        }
    }

    async fn watch(&mut self, out: &mut dyn FnMut(&str)) -> Result<(), Failure> {
        loop {
            match Events::open(&self.client).await {
                Ok(mut stream) => {
                    while let Ok(t) = stream.next().await {
                        let line = if self.json {
                            format!("{}\n", serde_json::to_string(&t).unwrap_or_default())
                        } else {
                            format!(
                                "{}  {} {}  {}  turn: {}  {}\n",
                                output::short(t.id.as_str()),
                                if t.role == cp_thread::Role::Recipient {
                                    "<-"
                                } else {
                                    "->"
                                },
                                t.peer,
                                t.state,
                                output::turn(&t),
                                output::title(&t)
                            )
                        };
                        out(&line);
                    }
                }
                Err(f) if f.status.is_some_and(|s| s < 500) => return Err(f),
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

fn create_private(path: &str) -> Result<std::fs::File, Failure> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
        .map_err(|e| Failure::usage(format!("create {path}: {e}")))
}

/// The thread changes on cpd's event stream.
struct Events {
    resp: reqwest::Response,
    buf: String,
}

impl Events {
    async fn open(client: &Client) -> Result<Self, Failure> {
        Ok(Self {
            resp: client.stream().await?,
            buf: String::new(),
        })
    }

    async fn next(&mut self) -> Result<ThreadSummary, Failure> {
        loop {
            while let Some(end) = self.buf.find("\n\n") {
                let frame: String = self.buf.drain(..end + 2).collect();
                let (mut event, mut data) = ("", String::new());
                for line in frame.lines() {
                    if let Some(e) = line.strip_prefix("event: ") {
                        event = e;
                    } else if let Some(d) = line.strip_prefix("data: ") {
                        data.push_str(d);
                    }
                }
                if event == cp_api::STREAM_EVENT
                    && let Ok(t) = serde_json::from_str(&data)
                {
                    return Ok(t);
                }
            }
            match self.resp.chunk().await {
                Ok(Some(c)) => self.buf.push_str(&String::from_utf8_lossy(&c)),
                Ok(None) => return Err(Failure::new(EXIT_ERROR, "stream: closed by cpd")),
                Err(e) => return Err(Failure::new(EXIT_ERROR, format!("stream: {e}"))),
            }
        }
    }
}

/// Command names, for tests and tools.
pub fn command_names() -> Vec<&'static str> {
    COMMANDS.iter().map(|c| c.name).collect()
}

/// The manual `cpctl help` prints.
pub fn manual() -> String {
    cli::overview()
}
