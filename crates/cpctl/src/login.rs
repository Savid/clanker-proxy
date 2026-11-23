//! The login `cpctl login` saves: one daemon's URL and a token, used when
//! neither flags nor the environment name a daemon or a token.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};

use crate::Env;
use crate::fail::{EXIT_ERROR, Failure};

/// A saved login.
pub(crate) struct Saved {
    pub(crate) url: String,
    pub(crate) token: SecretString,
}

/// Where the login lives: next to the tokens cloudflare.sh saves.
pub(crate) fn path(env: &Env) -> Option<PathBuf> {
    env.config
        .as_ref()
        .map(|c| Path::new(c).join("clanker-proxy").join("login"))
}

/// The saved login, if there is one. A file that cannot be read is an
/// error, not a silent fall back to another daemon.
pub(crate) fn load(env: &Env) -> Result<Option<Saved>, Failure> {
    let Some(file) = path(env) else {
        return Ok(None);
    };
    let text = match fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(&file, &e.to_string())),
    };
    let mut url = None;
    let mut token = None;
    for line in text.lines() {
        match line.split_once('=') {
            Some(("url", v)) if url.is_none() => url = Some(v.trim().to_string()),
            Some(("token", v)) if token.is_none() => {
                token = Some(SecretString::from(v.trim().to_string()));
            }
            _ => return Err(unreadable(&file, "unexpected content")),
        }
    }
    match (url, token) {
        (Some(url), Some(token)) if !url.is_empty() && looks_like_token(&token) => {
            Ok(Some(Saved { url, token }))
        }
        _ => Err(unreadable(&file, "incomplete")),
    }
}

fn unreadable(file: &Path, why: &str) -> Failure {
    Failure::new(
        EXIT_ERROR,
        format!("the saved login in {} is unreadable: {why}", file.display()),
    )
    .hint("run cpctl login <cpd-url> again, or cpctl logout")
}

/// Saves the login, replacing any other: written to a private temporary
/// file in the same directory, then renamed over the old one.
pub(crate) fn save(env: &Env, url: &str, token: &SecretString) -> Result<PathBuf, Failure> {
    let file = path(env).ok_or_else(|| {
        Failure::new(EXIT_ERROR, "no place to save the login").hint("set HOME or XDG_CONFIG_HOME")
    })?;
    let failed = |e: std::io::Error| {
        Failure::new(
            EXIT_ERROR,
            format!("cannot save the login in {}: {e}", file.display()),
        )
    };
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(failed)?;
    }
    let tmp = dir.join(format!(".login.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let written = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        writeln!(f, "url={url}")?;
        writeln!(f, "token={}", token.expose_secret())?;
        f.sync_all()?;
        fs::rename(&tmp, &file)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(failed(e));
    }
    Ok(file)
}

/// Removes the saved login and says which daemon it was for, if it could
/// tell; `None` when there was none.
pub(crate) fn remove(env: &Env) -> Result<Option<(PathBuf, Option<String>)>, Failure> {
    let Some(file) = path(env) else {
        return Ok(None);
    };
    if !file.exists() {
        return Ok(None);
    }
    let url = load(env).ok().flatten().map(|s| s.url);
    fs::remove_file(&file)
        .map_err(|e| Failure::new(EXIT_ERROR, format!("cannot remove {}: {e}", file.display())))?;
    Ok(Some((file, url)))
}

/// Whether a token has the shape of an owner or agent token; its value is
/// never shown.
pub(crate) fn looks_like_token(token: &SecretString) -> bool {
    let t = token.expose_secret();
    (t.starts_with(cp_api::prefix::OWNER) || t.starts_with(cp_api::prefix::AGENT))
        && t.len() > 4
        && !t.chars().any(char::is_whitespace)
}

/// A token read from a file, as cloudflare.sh saves them.
pub(crate) fn read_file(file: &str) -> Result<SecretString, Failure> {
    let raw =
        fs::read_to_string(file).map_err(|e| Failure::usage(format!("cannot read {file}: {e}")))?;
    Ok(SecretString::from(raw.trim().to_string()))
}

/// A token typed on the terminal, without echo. Echo comes back even when
/// reading fails.
pub(crate) fn read_terminal(tty: &str, prompt: &str) -> Result<SecretString, Failure> {
    let no_terminal = || {
        Failure::usage("cpctl login needs a terminal to ask for the token")
            .hint("pass --token-file <file> with the token in it")
    };
    let term = OpenOptions::new()
        .read(true)
        .write(true)
        .open(tty)
        .map_err(|_| no_terminal())?;
    if !rustix::termios::isatty(&term) {
        return Err(no_terminal());
    }
    let saved = rustix::termios::tcgetattr(&term).map_err(|_| no_terminal())?;
    let mut quiet = saved.clone();
    quiet.local_modes.remove(rustix::termios::LocalModes::ECHO);
    let mut out = &term;
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    rustix::termios::tcsetattr(&term, rustix::termios::OptionalActions::Now, &quiet)
        .map_err(|_| no_terminal())?;
    let mut line = String::new();
    let read = BufReader::new(&term).read_line(&mut line);
    let _ = rustix::termios::tcsetattr(&term, rustix::termios::OptionalActions::Now, &saved);
    let _ = writeln!(out);
    read.map_err(|e| Failure::new(EXIT_ERROR, format!("cannot read the token: {e}")))?;
    Ok(SecretString::from(line.trim().to_string()))
}
