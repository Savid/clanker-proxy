//! `cpd`: the clanker-proxy daemon.
// A command-line program reports start-up failures on stderr.
#![allow(clippy::print_stderr)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use cp_api::DaemonUrl;
use cp_thread::Name;
use cpd::{Daemon, Options};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

/// Exchanges issue-like threads with peers' daemons. Drive it with cpctl.
#[derive(Debug, Parser)]
#[command(name = "cpd", version = concat!("v", env!("CARGO_PKG_VERSION")))]
struct Args {
    /// The owner's name: lower-case letters, digits and single hyphens.
    /// Fixed by the first start; defaults to the login name then.
    #[arg(long)]
    name: Option<String>,
    /// Where peers reach this daemon: https, or http on a loopback address.
    /// Remembered when left out.
    #[arg(long)]
    url: Option<String>,
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:18471")]
    listen: SocketAddr,
    /// Data directory: the database and the owner token.
    #[arg(long, env = "CP_DIR")]
    dir: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "text")]
    log_format: LogFormat,
    /// Log level: error, warn, info or debug.
    #[arg(long, default_value = "info")]
    log_level: tracing::Level,
}

fn default_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from(".cp"), |h| PathBuf::from(h).join(".cp"))
}

fn main() -> ExitCode {
    let args = Args::parse();
    // Colour only on a terminal: container and service logs keep plain text.
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(args.log_level)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_writer(std::io::stderr);
    match args.log_format {
        LogFormat::Text => subscriber.init(),
        LogFormat::Json => subscriber.json().init(),
    }
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cpd: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    let dir = args.dir.unwrap_or_else(default_dir);
    let name = match args.name {
        Some(n) => Some(Name::normalize(&n).map_err(|e| format!("--name {e}"))?),
        None => std::env::var("USER")
            .ok()
            .and_then(|u| Name::normalize(&u).ok()),
    };
    let url = args
        .url
        .map(|u| DaemonUrl::try_from(u).map_err(|e| format!("--url {e}")))
        .transpose()?;
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let daemon = Daemon::start(Options {
            dir: dir.clone(),
            name,
            url,
            listen: args.listen,
            version: concat!("v", env!("CARGO_PKG_VERSION")).to_string(),
        })
        .await?;
        tracing::info!(owner = %daemon.name, addr = %daemon.addr, dir = %dir.display(), url = daemon.url.as_ref().map(DaemonUrl::as_str), "serving");
        if daemon.url.is_none() {
            tracing::warn!("no --url: peers can reach you, but you cannot ask anyone to peer");
        }
        shutdown_signal().await;
        tracing::info!("stopping");
        daemon.shutdown().await;
        Ok(())
    })
}

async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            let _ = interrupt.await;
            return;
        };
        tokio::select! {
            _ = interrupt => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = interrupt.await;
}
