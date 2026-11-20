//! The daemon's state machine over SQL.

use cp_api::{DaemonUrl, Error, ErrorKind, PublicKey};
use cp_thread::{Id, Name};

use crate::runtime::{Millis, Runtime};
use crate::schema::{BASELINE, MIGRATIONS, SCHEMA_VERSION, STATEMENTS};
use crate::signing::Identity;
use crate::sql::{Sql, Value, text};

/// Who this daemon is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The owner's name. Fixed by the first start; a later start may leave
    /// it out, but may not change it.
    pub name: Option<Name>,
    /// Where peers reach this daemon. Remembered when left out.
    pub url: Option<DaemonUrl>,
}

/// What a core call changed that the host acts on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Threads whose summary changed, for streams.
    pub changed: Vec<Id>,
    /// Peer deliveries may be due sooner than the host planned.
    pub deliver: bool,
}

impl Effects {
    pub(crate) fn changed(&mut self, id: &Id) {
        if !self.changed.contains(id) {
            self.changed.push(id.clone());
        }
    }
}

/// The daemon's logic. Every method is synchronous and runs its SQL in the
/// host's transaction for that call; network I/O happens in the host between
/// calls.
pub struct Core<S, R> {
    pub(crate) sql: S,
    pub(crate) rt: R,
    pub(crate) me: Name,
    pub(crate) url: Option<DaemonUrl>,
    pub(crate) identity: Identity,
    pub(crate) key: PublicKey,
    pub(crate) effects: Effects,
}

/// The daemon's key, made on first start; peers know it by its public half
/// from then on.
fn identity(
    sql: &impl Sql,
    rt: &mut impl Runtime,
    stored: Option<String>,
) -> Result<Identity, Error> {
    if let Some(stored) = stored {
        return Identity::from_stored(&stored);
    }
    let identity = Identity::generate(rt);
    sql.execute(
        "INSERT INTO meta (key, value) VALUES ('signing_key', ?)",
        &[identity.stored().into()],
    )?;
    Ok(identity)
}

impl<S: Sql, R: Runtime> Core<S, R> {
    /// Opens the daemon's storage: creates the schema on empty storage,
    /// migrates older data forward, refuses data from a newer release, and
    /// settles the owner's name and URL. Hosts run it in one transaction, so
    /// a migration applies whole or not at all.
    pub fn open(sql: S, mut rt: R, config: Config) -> Result<Self, Error> {
        if let Some(create_meta) = STATEMENTS.first() {
            sql.execute(create_meta, &[])?;
        }
        let meta = |key: &str| -> Result<Option<String>, Error> {
            sql.query_one("SELECT value FROM meta WHERE key = ?", &[key.into()])?
                .map(|r| r.text(0).map(str::to_string))
                .transpose()
        };
        let stored = meta("schema_version")?
            .map(|v| {
                v.parse::<u32>().map_err(|_| {
                    Error::new(ErrorKind::Internal, "stored schema version is not a number")
                })
            })
            .transpose()?;
        let pending: &[&[&str]] = match stored {
            None => {
                for statement in STATEMENTS {
                    sql.execute(statement, &[])?;
                }
                MIGRATIONS
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!(
                        "stored data is schema version {v}, from a newer release than this one (schema {SCHEMA_VERSION}); run that release or later"
                    ),
                ));
            }
            Some(v) if v < BASELINE => {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!(
                        "stored data is schema version {v}, from an earlier release this one cannot migrate; start from empty storage"
                    ),
                ));
            }
            Some(v) => MIGRATIONS
                .get(usize::try_from(v - BASELINE).unwrap_or(usize::MAX)..)
                .unwrap_or_default(),
        };
        for migration in pending {
            for statement in *migration {
                sql.execute(statement, &[])?;
            }
        }
        sql.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            &[SCHEMA_VERSION.to_string().into()],
        )?;

        let stored = meta("name")?
            .map(Name::try_from)
            .transpose()
            .map_err(|_| Error::new(ErrorKind::Internal, "stored owner name is invalid"))?;
        let me = match (stored, config.name) {
            (Some(stored), Some(given)) if stored != given => {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "this storage belongs to {stored}, not {given}; use other storage for a new name"
                    ),
                ));
            }
            (Some(stored), _) => stored,
            (None, Some(given)) => given,
            (None, None) => {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    "the first start needs the owner's name",
                ));
            }
        };
        sql.execute(
            "INSERT INTO meta (key, value) VALUES ('name', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            &[text(&me)],
        )?;

        let url = match config.url {
            Some(url) => {
                sql.execute(
                    "INSERT INTO meta (key, value) VALUES ('url', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    &[text(url.as_str())],
                )?;
                Some(url)
            }
            None => meta("url")?.and_then(|u| DaemonUrl::try_from(u).ok()),
        };

        let identity = identity(&sql, &mut rt, meta("signing_key")?)?;
        let key = identity.public()?;

        Ok(Self {
            sql,
            rt,
            me,
            url,
            identity,
            key,
            effects: Effects::default(),
        })
    }

    /// The owner's name.
    pub fn me(&self) -> &Name {
        &self.me
    }

    /// This daemon's public key, which peers verify its calls with.
    pub fn public_key(&self) -> &PublicKey {
        &self.key
    }

    /// The headers that sign a call from this daemon to the peer with key
    /// `recipient`, made now.
    pub(crate) fn sign(
        &self,
        method: &str,
        path: &str,
        recipient: &PublicKey,
        body: &[u8],
    ) -> Result<Vec<(String, String)>, Error> {
        self.identity.sign(
            method,
            path,
            self.now().0.div_euclid(Millis::SECOND),
            recipient,
            body,
        )
    }

    /// Where peers reach this daemon, if it has a public URL.
    pub fn url(&self) -> Option<&DaemonUrl> {
        self.url.as_ref()
    }

    /// What calls since the last take changed. A host takes them after each
    /// call that committed.
    pub fn take_effects(&mut self) -> Effects {
        std::mem::take(&mut self.effects)
    }

    /// Drops effects of a call whose transaction rolled back.
    pub fn discard_effects(&mut self) {
        self.effects = Effects::default();
    }

    pub fn sql(&self) -> &S {
        &self.sql
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.rt
    }

    pub(crate) fn now(&self) -> Millis {
        self.rt.now()
    }

    /// The current time to the second, as the API reports it.
    pub fn now_at(&self) -> cp_thread::At {
        self.rt.now().at()
    }

    pub(crate) fn new_id(&mut self) -> Id {
        let mut bytes = [0u8; 16];
        self.rt.random(&mut bytes);
        Id::from_random(bytes)
    }
}

/// The peers a caller may reach, as a JSON array for `json_each`, or NULL
/// for every peer.
pub(crate) fn scope_json(scope: Option<&cp_api::Peers>) -> Value {
    match scope {
        None => Value::Null,
        Some(peers) if peers.is_empty() => Value::Null,
        Some(peers) => Value::Text(
            serde_json::to_string(
                &peers
                    .as_slice()
                    .iter()
                    .map(Name::as_str)
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|_| "[]".into()),
        ),
    }
}
