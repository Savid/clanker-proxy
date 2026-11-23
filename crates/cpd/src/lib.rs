//! clanker-proxy's daemon on a machine: storage in a data directory, the
//! API on a TCP listener, and delivery to peers in the background.

mod host;
mod server;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use cp_api::DaemonUrl;
use cp_core::{Config, Core};
use cp_http::Service;
use cp_sqlite::Sqlite;
use cp_thread::Name;
use tokio::net::TcpListener;
use tokio::sync::watch;

pub use host::{NativeHost, OsRuntime};
pub use server::SHUTDOWN_GRACE;

/// The owner token's file in the data directory.
pub const TOKEN_FILE: &str = "owner.token";
/// The database's file in the data directory.
pub const DB_FILE: &str = "cp.db";
/// Held locked while a daemon uses the data directory.
pub const LOCK_FILE: &str = "cpd.lock";

/// How to start a daemon.
#[derive(Clone, Debug)]
pub struct Options {
    pub dir: PathBuf,
    /// The owner's name; required on the first start, fixed after.
    pub name: Option<Name>,
    /// Where peers reach this daemon; remembered when left out.
    pub url: Option<DaemonUrl>,
    pub listen: SocketAddr,
    pub version: String,
}

/// A running daemon.
pub struct Daemon {
    pub addr: SocketAddr,
    pub name: Name,
    pub url: Option<DaemonUrl>,
    stop: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    _lock: std::fs::File,
}

impl Daemon {
    /// Opens the data directory and starts serving.
    pub async fn start(opts: Options) -> Result<Self, String> {
        create_dir(&opts.dir)?;
        // One daemon per directory: a second would migrate data the first
        // still serves under the old schema.
        let lock = lock(&opts.dir)?;
        // Bound before storage is touched, so a daemon that cannot serve
        // (its port still held by the one it replaces) changes nothing.
        let listener = TcpListener::bind(opts.listen)
            .await
            .map_err(|e| format!("listen on {}: {e}", opts.listen))?;
        // The token file is the owner's credential for cpctl; the daemon keeps
        // only its hash.
        let owner_hash = cp_core::token_hash(&owner_token(&opts.dir.join(TOKEN_FILE))?);
        let sql = Sqlite::open(&opts.dir.join(DB_FILE)).map_err(|e| e.to_string())?;
        // A migration cannot be undone in place, and an earlier release
        // refuses migrated data: keep a copy as the data is now, which that
        // release still reads. Made afresh on every attempt, so it is never
        // older than the data it stands for.
        if let Some(v) = cp_core::stored_version(&sql).map_err(|e| e.detail)?
            && (cp_core::UPGRADES_FROM..cp_core::SCHEMA_VERSION).contains(&v)
        {
            sql.copy_to(&opts.dir.join(format!("{DB_FILE}.schema-{v}")))
                .map_err(|e| e.to_string())?;
        }
        // One transaction, so a migration applies whole or not at all; on
        // failure the dropped connection rolls it back.
        sql.begin().map_err(|e| e.to_string())?;
        let core = Core::open(
            sql,
            OsRuntime,
            Config {
                name: opts.name,
                url: opts.url,
            },
        )
        .map_err(|e| e.detail)?;
        core.sql().commit().map_err(|e| e.to_string())?;
        let (name, url) = (core.me().clone(), core.url().cloned());
        let host = NativeHost::start(core, owner_hash, opts.version)?;

        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let (stop, stopped) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(server::serve(
                listener,
                Service::new(host.clone()),
                stopped.clone(),
            )),
            tokio::spawn(host::run_delivery(host, stopped)),
        ];
        Ok(Self {
            addr,
            name,
            url,
            stop,
            tasks,
            _lock: lock,
        })
    }

    /// Stops serving and delivering, ending streams.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        for t in self.tasks {
            let _ = t.await;
        }
    }
}

fn lock(dir: &Path) -> Result<std::fs::File, String> {
    let path = dir.join(LOCK_FILE);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    f.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => {
            format!("another cpd is using {}", dir.display())
        }
        std::fs::TryLockError::Error(e) => format!("lock {}: {e}", path.display()),
    })?;
    Ok(f)
}

fn create_dir(dir: &Path) -> Result<(), String> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
        .map_err(|e| format!("create {}: {e}", dir.display()))
}

/// The owner token in `path`, made on first start: `cpo_` and 32 random
/// bytes, readable only by the owner.
pub fn owner_token(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => return Ok(t.trim().to_string()),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("owner token: {e}"));
        }
        Err(_) => {}
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("owner token: {e}"))?;
    let token = format!("{}{}", cp_api::prefix::OWNER, base64url(&bytes));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(path).map_err(|e| format!("owner token: {e}"))?;
    std::io::Write::write_all(&mut f, format!("{token}\n").as_bytes())
        .map_err(|e| format!("owner token: {e}"))?;
    Ok(token)
}

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        let chars = chunk.len() + 1;
        for i in 0..chars {
            let idx = usize::try_from((n >> (18 - 6 * i)) & 63).unwrap_or(0);
            if let Some(c) = A.get(idx) {
                out.push(char::from(*c));
            }
        }
    }
    out
}
