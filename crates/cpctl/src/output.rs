//! Text output: what a thread says and what usually comes next.

use std::fmt::Write as _;

use cp_api::{Delivery, DeliveryStatus, Peer, PeerStatus, Request, ThreadEvent, ThreadSummary};
use cp_thread::{Action, Kind, Role, State};

/// How much of a thread ID lists show.
pub(crate) const SHORT_ID: usize = 8;

/// A command that usually comes next, and what it is for.
pub(crate) struct Step {
    pub(crate) cmd: String,
    pub(crate) why: String,
}

pub(crate) fn step(cmd: impl Into<String>, why: impl Into<String>) -> Step {
    Step {
        cmd: cmd.into(),
        why: why.into(),
    }
}

/// Ends output with the commands that usually follow.
pub(crate) fn next(out: &mut String, steps: &[Step]) {
    if steps.is_empty() {
        return;
    }
    out.push_str("next:\n");
    let width = steps
        .iter()
        .map(|s| s.cmd.chars().count())
        .max()
        .unwrap_or(0);
    for s in steps {
        let _ = writeln!(out, "  {:<width$}   {}", s.cmd, s.why);
    }
}

pub(crate) fn short(id: &str) -> &str {
    id.get(..SHORT_ID).unwrap_or(id)
}

/// Drops characters that could rewrite the terminal or disguise text;
/// `multiline` keeps line breaks and tabs.
pub(crate) fn printable(s: &str, multiline: bool) -> String {
    s.chars()
        .filter(|c| {
            (multiline && matches!(c, '\n' | '\t')) || !(c.is_control() || cp_thread::is_hidden(*c))
        })
        .collect()
}

fn action_step(action: Action, r: &str) -> Step {
    match action {
        Action::Comment => step(format!(r#"cpctl reply {r} -m "<text>""#), "add a message"),
        Action::Ack => step(format!("cpctl ack {r}"), "optional: say you are on it"),
        Action::NeedsInput => step(
            format!(r#"cpctl needs-input {r} -m "<question>""#),
            "ask the sender something",
        ),
        Action::Resolve => step(
            format!(r#"cpctl resolve {r} -m "<result>""#),
            "done; report the result",
        ),
        Action::Decline => step(
            format!(r#"cpctl decline {r} -m "<why>""#),
            "refuse it; ends the thread",
        ),
        Action::Close => step(
            format!("cpctl close {r}"),
            "done here; ends the thread for both sides",
        ),
        Action::Reopen => step(
            format!(r#"cpctl reopen {r} -m "<why>""#),
            "not done; back to the recipient",
        ),
        Action::Withdraw => step(
            format!(r#"cpctl withdraw {r} -m "<why>""#),
            "no longer needed; ends the thread",
        ),
        Action::Open => step("cpctl send <peer> <title>", "open a thread"),
    }
}

/// Puts each side's main moves first.
const STEP_ORDER: [Action; 8] = [
    Action::Resolve,
    Action::NeedsInput,
    Action::Close,
    Action::Reopen,
    Action::Ack,
    Action::Decline,
    Action::Withdraw,
    Action::Comment,
];

/// What the owner can do with the thread: wait while it is the peer's turn,
/// then the actions they may take.
pub(crate) fn thread_steps(t: &ThreadSummary) -> Vec<Step> {
    let r = short(t.id.as_str());
    let mut steps = Vec::new();
    if !t.my_turn && t.turn.is_some() && t.kind != Kind::Fyi {
        steps.push(step(
            format!("cpctl wait {r} --timeout 30m"),
            format!("block until {} acts", t.peer),
        ));
    }
    let fyi_ack = t.kind == Kind::Fyi && t.actions.contains(&Action::Ack);
    if fyi_ack {
        steps.push(step(
            format!("cpctl ack {r}"),
            "say you have seen it; closes it",
        ));
    }
    // The sender's answer to a question is a reply, and the move that hands
    // the thread back, so it comes before ending the thread.
    let answering = t.state == State::NeedsInput
        && t.role == Role::Sender
        && t.actions.contains(&Action::Comment);
    if answering {
        steps.push(step(
            format!(r#"cpctl reply {r} -m "<answer>""#),
            "answer their question; hands it back",
        ));
    }
    for a in STEP_ORDER {
        if !t.actions.contains(&a)
            || (a == Action::Ack && fyi_ack)
            || (a == Action::Comment && answering)
        {
            continue;
        }
        let mut s = action_step(a, r);
        if a == Action::Close && t.role == Role::Recipient {
            s.why = if t.state == State::Resolved {
                "only if the sender has agreed; ends the thread for both sides".into()
            } else {
                "end it without a result, e.g. no longer needed; to report work, resolve".into()
            };
        }
        steps.push(s);
    }
    steps
}

/// The peer's text, quoted, so it cannot pass for cpctl's own words.
pub(crate) fn title(t: &ThreadSummary) -> String {
    let mut s = format!("{:?}", printable(t.title.as_str(), false));
    if t.kind == Kind::Fyi {
        s = format!("[fyi] {s}");
    }
    if t.failed > 0 {
        let _ = write!(
            s,
            "  ({} {} undeliverable)",
            t.failed,
            plural(t.failed, "event")
        );
    } else if t.undelivered > 0 {
        let _ = write!(
            s,
            "  ({} {} queued for delivery)",
            t.undelivered,
            plural(t.undelivered, "event")
        );
    }
    s
}

pub(crate) fn plural(n: u32, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

pub(crate) fn turn(t: &ThreadSummary) -> String {
    match (&t.turn, t.my_turn) {
        (_, true) => "mine".into(),
        (Some(p), false) => p.to_string(),
        (None, _) => "nobody (ended)".into(),
    }
}

fn direction(t: &ThreadSummary) -> &'static str {
    if t.role == Role::Recipient {
        "<-"
    } else {
        "->"
    }
}

/// A thread's header: what it is, where it stands, and whose turn it is.
pub(crate) fn summary(out: &mut String, t: &ThreadSummary) {
    let _ = writeln!(out, "{}  {}", t.id, title(t));
    let _ = write!(
        out,
        "{} -> {} · {} · {} · you are the {} · turn: {}",
        t.sender,
        t.recipient,
        t.kind,
        t.state,
        t.role,
        turn(t)
    );
    if !t.labels.is_empty() {
        let labels: Vec<&str> = t
            .labels
            .as_slice()
            .iter()
            .map(cp_thread::Label::as_str)
            .collect();
        let _ = write!(out, " · labels: {}", labels.join(", "));
    }
    out.push('\n');
    let _ = writeln!(out, "{}", standing(t));
    if t.overruled > 0 {
        let _ = writeln!(
            out,
            "{} of your {} did not take effect: a move from {} came first. The thread is {} now.",
            t.overruled,
            plural(t.overruled, "move"),
            t.peer,
            t.state
        );
    }
}

/// What the thread's state asks of the owner.
pub(crate) fn standing(t: &ThreadSummary) -> String {
    let peer = t.peer.as_str();
    let sender = t.role == Role::Sender;
    if t.kind == Kind::Fyi && t.state == State::Open {
        return if sender {
            format!("fyi: nothing is needed from you; it closes when {peer} acks it.")
        } else {
            "fyi: no answer expected; ack it to say you have seen it, which closes it.".into()
        };
    }
    match (t.state, sender) {
        (State::Open | State::Acked, true) => format!("Waiting on {peer} to work on it."),
        (State::Open | State::Acked, false) => {
            "Waiting on you: resolve it with the result when done, or ask with needs-input.".into()
        }
        (State::NeedsInput, true) => {
            format!("{peer} asked you something: reply to answer, which hands it back.")
        }
        (State::NeedsInput, false) => format!("Waiting on {peer} to answer your question."),
        (State::Resolved, true) => {
            format!(
                "{peer} says it is done: close it if you agree, or reopen it saying what is missing."
            )
        }
        (State::Resolved, false) => {
            format!(
                "Awaiting {peer}'s review. Close only if they have agreed; you can also reopen it with a reason."
            )
        }
        (State::Closed, _) => {
            "Ended: closed. Either side can reopen it with a reason, or still reply.".into()
        }
        (State::Declined, _) => {
            "Ended: the recipient declined it. Either side can still reply.".into()
        }
        (State::Withdrawn, _) => {
            "Ended: the sender withdrew it. Either side can still reply.".into()
        }
    }
}

fn delivery_note(d: Option<&Delivery>) -> String {
    match d {
        Some(d) if d.status == DeliveryStatus::Pending => {
            let mut s = format!(
                " · queued for delivery ({} {})",
                d.attempts,
                plural(d.attempts, "attempt")
            );
            if let Some(e) = &d.last_error {
                let _ = write!(s, ": {:?}", printable(e, false));
            }
            s
        }
        Some(d) if d.status == DeliveryStatus::Failed => {
            format!(
                " · NOT DELIVERED: {:?}",
                printable(d.last_error.as_deref().unwrap_or("refused"), false)
            )
        }
        _ => String::new(),
    }
}

/// The events. Every body line is marked, so a peer's text cannot pass for
/// an event header or a `next:` step.
pub(crate) fn log(out: &mut String, events: &[ThreadEvent]) {
    for e in events {
        let _ = writeln!(
            out,
            "\n-- {} · {} · {}{}",
            e.action,
            e.from,
            e.at,
            delivery_note(e.delivery.as_ref())
        );
        if let Some(why) = &e.ignored {
            let _ = writeln!(out, "   (no effect: {why})");
        }
        if !e.body.is_empty() {
            for line in printable(e.body.as_str(), true).split('\n') {
                let _ = writeln!(out, "  │ {line}");
            }
        }
    }
}

/// How long ago `then` was, roughly.
pub(crate) fn ago(now: i64, then: cp_thread::At) -> String {
    let d = now - then.as_unix_seconds();
    match d {
        ..60 => "just now".into(),
        60..3600 => format!("{}m ago", d / 60),
        3600..172_800 => format!("{}h ago", d / 3600),
        _ => then.to_string().get(..10).unwrap_or_default().to_string(),
    }
}

/// Rows aligned in columns, two spaces apart.
pub(crate) fn table(out: &mut String, rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for r in rows {
        let mut line = String::new();
        for (i, cell) in r.iter().enumerate() {
            if i + 1 == r.len() {
                line.push_str(cell);
            } else {
                let w = widths.get(i).copied().unwrap_or(0);
                let _ = write!(line, "{cell:<w$}  ");
            }
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
}

pub(crate) fn threads(out: &mut String, threads: &[ThreadSummary], now: i64) {
    let mut rows = vec![vec![
        "ID".into(),
        "PEER".into(),
        "STATE".into(),
        "TURN".into(),
        "UPDATED".into(),
        "TITLE".into(),
    ]];
    for t in threads {
        rows.push(vec![
            short(t.id.as_str()).into(),
            format!("{} {}", direction(t), t.peer),
            t.state.to_string(),
            turn(t),
            ago(now, t.updated_at),
            title(t),
        ]);
    }
    table(out, &rows);
}

pub(crate) fn peers(out: &mut String, peers: &[Peer]) {
    let mut rows = vec![vec![
        "NAME".into(),
        "ALIASES".into(),
        "STATUS".into(),
        "CODE".into(),
        "VERSION".into(),
        "URL".into(),
    ]];
    let mut requested = false;
    for p in peers {
        let status = match p.status {
            PeerStatus::Requested => "requested",
            PeerStatus::Active => "active",
        };
        requested |= p.status == PeerStatus::Requested;
        let aliases: Vec<&str> = p.aliases.iter().map(cp_thread::Name::as_str).collect();
        rows.push(vec![
            p.name.to_string(),
            if aliases.is_empty() {
                "-".into()
            } else {
                aliases.join(",")
            },
            status.into(),
            p.code.to_string(),
            p.version
                .as_ref()
                .map_or_else(|| "-".to_string(), ToString::to_string),
            p.url.to_string(),
        ]);
    }
    table(out, &rows);
    if requested {
        out.push_str("requested: waiting for their owner to approve; threads to them are delivered after that\n");
    }
}

pub(crate) fn requests(out: &mut String, requests: &[Request], now: i64) {
    let mut rows = vec![vec![
        "ID".into(),
        "NAME".into(),
        "CODE".into(),
        "ASKED".into(),
        "URL".into(),
        "NOTE".into(),
    ]];
    for r in requests {
        rows.push(vec![
            r.id.to_string(),
            r.name.to_string(),
            r.code.to_string(),
            ago(now, r.at),
            r.url.to_string(),
            format!(
                "{:?}",
                printable(r.note.as_ref().map_or("", cp_api::Note::as_str), false)
            ),
        ]);
    }
    table(out, &rows);
}
