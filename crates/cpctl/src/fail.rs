//! Failures: what to report, the exit code, and how to recover.

use cp_api::Problem;

pub const EXIT_ERROR: u8 = 1;
pub const EXIT_TIMEOUT: u8 = 2;
pub const EXIT_USAGE: u8 = 3;
pub const EXIT_NOT_FOUND: u8 = 4;
pub const EXIT_REFUSED: u8 = 5;
pub const EXIT_AUTH: u8 = 6;
pub const EXIT_UNREACHABLE: u8 = 7;

/// A command that did not succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub exit: u8,
    pub message: String,
    pub hint: Option<String>,
    /// The HTTP status cpd answered with, if it answered.
    pub status: Option<u16>,
}

impl Failure {
    pub fn new(exit: u8, message: impl Into<String>) -> Self {
        Self {
            exit,
            message: message.into(),
            hint: None,
            status: None,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(EXIT_USAGE, message)
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// What a refusal from cpd means for the command that got it.
pub(crate) fn from_problem(
    status: u16,
    p: &Problem,
    command: &str,
    thread_ref: Option<&str>,
    agent: bool,
) -> Failure {
    let message = p.detail.clone().unwrap_or_else(|| p.title.clone());
    let (exit, hint) = match status {
        400 => (EXIT_USAGE, Some(bad_input_hint(command, &message))),
        401 => (
            EXIT_AUTH,
            Some(if agent {
                "this agent token was revoked, has expired or is unknown; ask the owner for a new one".to_string()
            } else {
                "set CP_TOKEN (or --token) to the owner.token in cpd's data directory (CP_DIR, else ~/.cp), or to an agent token from cpctl token add".to_string()
            }),
        ),
        403 => (
            EXIT_REFUSED,
            Some("this is an agent token: it runs me, send, inbox, ls, show, wait, watch, peer ls, peer show and the thread actions (reply, ack, needs-input, resolve, decline, close, reopen, withdraw); peering, tokens and webhooks are the owner's".to_string()),
        ),
        404 => (EXIT_NOT_FOUND, Some(not_found_hint(command, agent))),
        409 | 422 => (
            EXIT_REFUSED,
            match command {
                "token add" => Some("choose another name, or revoke the existing one with cpctl token rm <name>".to_string()),
                "approve" => Some("cpctl approve <id> --as <name> gives them another name".to_string()),
                "peer alias" => Some("cpctl peer ls lists peers' names and aliases".to_string()),
                _ => thread_ref.map(|r| format!("cpctl show {r} lists what you can do now, as commands")),
            },
        ),
        502 => (
            EXIT_ERROR,
            Some("the other daemon did not answer; check its URL and that its cpd is running, then try again".to_string()),
        ),
        _ => (EXIT_ERROR, None),
    };
    Failure {
        exit,
        message,
        hint,
        status: Some(status),
    }
}

fn bad_input_hint(command: &str, message: &str) -> String {
    if message.contains("matches more than one thread") {
        "give more of the thread ID; cpctl ls shows them".into()
    } else if matches!(command, "approve" | "deny") {
        "cpctl requests lists request IDs".into()
    } else {
        format!("cpctl help {command}")
    }
}

fn not_found_hint(command: &str, agent: bool) -> String {
    if agent && matches!(command, "send" | "peer show") {
        "cpctl peer ls lists the peers this token reaches".into()
    } else if command.starts_with("token ") {
        "cpctl token ls lists agent tokens".into()
    } else {
        match command {
            "approve" | "deny" => "cpctl requests lists pending requests and their IDs".into(),
            "send" | "peer show" | "peer rm" | "peer alias" | "peer unalias" => {
                "cpctl peer ls lists peers; cpctl peer add <name> <url> connects someone new".into()
            }
            _ => "cpctl ls lists threads; a <ref> is a thread ID or a unique prefix of it".into(),
        }
    }
}
