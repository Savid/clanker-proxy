//! Peering: asking to connect, approving requests, and knowing peers by
//! their keys.
//!
//! Steps that call another daemon are split in two: the core prepares and
//! records what is about to happen, the host makes the call, and the core
//! commits or aborts. Other calls may run in between, so commits check
//! again.

use cp_api::{
    Approval, DaemonUrl, Error, ErrorKind, Note, Peer, PeerInput, PeerList, PeerStatus,
    PeeringRequest, PublicKey, ReceiptStatus, Request, RequestList, RequestReceipt, RequestRef,
    Version,
};
use cp_thread::Name;

use crate::core::Core;
use crate::runtime::{Millis, Runtime};
use crate::secrets;
use crate::signing::{MAX_SKEW_SECS, SignedCall, code};
use crate::sql::{Row, Sql, text};

/// How long a peering request waits for the owner, and how long events to a
/// peer that never approved are retried.
pub const REQUEST_TTL: i64 = 7 * Millis::DAY;
/// Most peering requests waiting at once; a new one drops the oldest.
pub const MAX_PENDING_REQUESTS: u32 = 20;
/// Public peering requests accepted per minute.
pub const REQUESTS_PER_MINUTE: u32 = 10;
/// How long an approval may wait on the requester's daemon before another
/// can start. Longer than the host's request timeout.
pub const CONFIRM_TTL: i64 = 30 * Millis::SECOND;

fn err(kind: ErrorKind, detail: impl Into<String>) -> Error {
    Error::new(kind, detail)
}

/// A POST to another daemon, ready to send: `path` on `url`, with these
/// headers (a signature, when the call is signed) and body.
#[derive(Clone, Debug)]
pub struct Call {
    pub url: DaemonUrl,
    pub path: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// An approval waiting on the requester's daemon to confirm.
#[derive(Clone, Debug)]
pub struct PendingApproval {
    pub request: RequestRef,
    pub name: Name,
    pub call: Call,
}

struct PeerRow {
    name: Name,
    url: DaemonUrl,
    key: PublicKey,
    status: PeerStatus,
    added_at: Millis,
    version: Option<Version>,
}

const PEER_COLUMNS: &str = "name, url, key, status, added_at, version";

fn peer_row(r: &Row) -> Result<PeerRow, Error> {
    Ok(PeerRow {
        name: r.parse(0)?,
        url: r.parse(1)?,
        key: r.parse(2)?,
        status: match r.text(3)? {
            "active" => PeerStatus::Active,
            _ => PeerStatus::Requested,
        },
        added_at: Millis(r.int(4)?),
        // An unparseable report is shown as none rather than failing reads.
        version: r
            .opt_text(5)?
            .and_then(|v| Version::try_from(v.to_string()).ok()),
    })
}

impl<S: Sql, R: Runtime> Core<S, R> {
    fn peer_view(&self, p: &PeerRow) -> Result<Peer, Error> {
        Ok(Peer {
            name: p.name.clone(),
            url: p.url.clone(),
            status: p.status,
            code: code(&p.key, &self.key)?,
            added_at: p.added_at.at(),
            version: p.version.clone(),
        })
    }

    fn load_peer(&self, name: &Name) -> Result<Option<PeerRow>, Error> {
        self.sql
            .query_one(
                &format!("SELECT {PEER_COLUMNS} FROM peers WHERE name = ?"),
                &[text(name)],
            )?
            .map(|r| peer_row(&r))
            .transpose()
    }

    fn live_op(&self) -> Result<Option<Row>, Error> {
        Ok(self.sql.query_one(
            "SELECT request_id, name, url, key, previous_key FROM peering_op WHERE expires_at > ?",
            &[self.now().0.into()],
        )?)
    }

    /// Every peer, by name.
    pub fn peers(&self) -> Result<PeerList, Error> {
        let rows = self.sql.query(
            &format!("SELECT {PEER_COLUMNS} FROM peers ORDER BY name"),
            &[],
        )?;
        let peers = rows
            .iter()
            .map(|r| self.peer_view(&peer_row(r)?))
            .collect::<Result<_, _>>()?;
        Ok(PeerList { peers })
    }

    pub fn peer(&self, name: &Name) -> Result<Peer, Error> {
        let p = self
            .load_peer(name)?
            .ok_or_else(|| err(ErrorKind::NotFound, format!("{name} is not a peer")))?;
        self.peer_view(&p)
    }

    /// Whether `name` is or was a peer: what a token or webhook may be
    /// limited to, so a typo does not limit it to nobody.
    pub(crate) fn known_peer(&self, name: &Name) -> Result<bool, Error> {
        Ok(self
            .sql
            .query_one(
                "SELECT 1 FROM peers WHERE name = ?1 UNION SELECT 1 FROM former_peers WHERE name = ?1",
                &[text(name)],
            )?
            .is_some())
    }

    /// Refuses a name for the daemon at `url` when it cannot be theirs, and
    /// returns the peer it would replace.
    fn approvable(&self, name: &Name, url: &DaemonUrl) -> Result<Option<PeerRow>, Error> {
        if *name == self.me {
            return Err(err(
                ErrorKind::Unprocessable,
                format!("{name} is this daemon's owner; choose another name"),
            ));
        }
        // Threads name their participants, so a removed peer's name stays
        // with their URL.
        if let Some(r) = self
            .sql
            .query_one("SELECT url FROM former_peers WHERE name = ?", &[text(name)])?
            && r.text(0)? != url.as_str()
        {
            return Err(err(
                ErrorKind::Conflict,
                format!(
                    "{name} was a peer at {} and threads with them are kept; give this one another name",
                    r.text(0)?
                ),
            ));
        }
        let mut previous = None;
        for r in self
            .sql
            .query(&format!("SELECT {PEER_COLUMNS} FROM peers"), &[])?
        {
            let p = peer_row(&r)?;
            match (p.name == *name, p.url == *url) {
                (true, false) => {
                    return Err(err(
                        ErrorKind::Conflict,
                        format!("another peer is called {name}; approve with another name"),
                    ));
                }
                (false, true) => {
                    return Err(err(
                        ErrorKind::Conflict,
                        format!(
                            "{} is already a peer at {url}; use that name or remove them first",
                            p.name
                        ),
                    ));
                }
                (true, true) => previous = Some(p),
                (false, false) => {}
            }
        }
        Ok(previous)
    }

    /// Checks the owner may ask the daemon at `input.url` to peer, and
    /// returns the request to send. Nothing is stored until it answers:
    /// [`Core::finish_add_peer`] records the peer with the key it sent back.
    pub fn add_peer(&mut self, input: PeerInput) -> Result<Call, Error> {
        let PeerInput { name, url, note } = input;
        self.check_new_peer(&name, &url)?;
        let own_url = self.url.clone().ok_or_else(|| {
            err(
                ErrorKind::Unprocessable,
                "this daemon has no public URL to give them; restart it with its URL",
            )
        })?;
        let request = PeeringRequest {
            name: self.me.clone(),
            url: own_url,
            key: self.key.clone(),
            note,
        };
        Ok(Call {
            url,
            path: "/api/v1/peering-requests",
            headers: Vec::new(),
            body: serde_json::to_vec(&request)
                .map_err(|e| err(ErrorKind::Internal, e.to_string()))?,
        })
    }

    fn check_new_peer(&self, name: &Name, url: &DaemonUrl) -> Result<(), Error> {
        if self.load_peer(name)?.is_some() {
            return Err(err(
                ErrorKind::Conflict,
                format!("{name} is already a peer"),
            ));
        }
        self.approvable(name, url)?;
        if let Some(r) = self.sql.query_one(
            "SELECT id, name, key FROM requests WHERE url = ?",
            &[text(url.as_str())],
        )? {
            return Err(err(
                ErrorKind::Conflict,
                format!(
                    "{url} already asked to peer as {} (request {}, code {}); approve that request instead",
                    r.text(1)?,
                    r.text(0)?,
                    code(&r.parse(2)?, &self.key)?
                ),
            ));
        }
        Ok(())
    }

    /// Records the peer the owner asked, now that its daemon has the request
    /// and sent back its key. Checks again: anything may have changed while
    /// the request was out.
    pub fn finish_add_peer(
        &mut self,
        name: &Name,
        url: &DaemonUrl,
        receipt: &RequestReceipt,
    ) -> Result<Peer, Error> {
        self.check_new_peer(name, url)?;
        if receipt.key == self.key {
            return Err(err(ErrorKind::Unprocessable, "that URL is this daemon"));
        }
        if let Some(r) = self.sql.query_one(
            "SELECT name FROM peers WHERE key = ?",
            &[text(receipt.key.as_str())],
        )? {
            return Err(err(
                ErrorKind::Conflict,
                format!("that daemon is already a peer, as {}", r.text(0)?),
            ));
        }
        self.sql.execute(
            "INSERT INTO peers (name, url, key, status, added_at) VALUES (?, ?, ?, 'requested', ?)",
            &[
                text(name),
                text(url.as_str()),
                text(receipt.key.as_str()),
                self.now().0.into(),
            ],
        )?;
        self.peer(name)
    }

    /// Stores a peering request from another daemon for the owner to
    /// approve. Public, so rate-limited and capped.
    pub fn request_received(&mut self, req: &PeeringRequest) -> Result<RequestReceipt, Error> {
        if req.key == self.key {
            return Err(err(
                ErrorKind::Unprocessable,
                "a daemon cannot peer with itself",
            ));
        }
        let now = self.now();
        self.sql.execute(
            "DELETE FROM request_times WHERE at <= ?",
            &[now.plus(-Millis::MINUTE).0.into()],
        )?;
        let recent = self
            .sql
            .query_one("SELECT count(*) FROM request_times", &[])?
            .map(|r| r.count(0))
            .transpose()?
            .unwrap_or(0);
        if recent >= REQUESTS_PER_MINUTE {
            return Err(err(
                ErrorKind::Unavailable,
                "too many peering requests; retry in a minute",
            )
            .retry_after(60));
        }
        self.sql
            .execute("INSERT INTO request_times (at) VALUES (?)", &[now.0.into()])?;

        let id = secrets::random_hex(&mut self.rt, 4);
        self.sql.execute(
            "DELETE FROM requests WHERE at < ?",
            &[now.plus(-REQUEST_TTL).0.into()],
        )?;
        self.sql.execute(
            "INSERT INTO requests (id, name, url, key, note, at) VALUES (?, ?, ?, ?, ?, ?)",
            &[
                id.clone().into(),
                text(&req.name),
                text(req.url.as_str()),
                text(req.key.as_str()),
                req.note.as_ref().map(Note::as_str).into(),
                now.0.into(),
            ],
        )?;
        self.sql.execute(
            "DELETE FROM requests WHERE id NOT IN (SELECT id FROM requests ORDER BY at DESC, rowid DESC LIMIT ?)",
            &[MAX_PENDING_REQUESTS.into()],
        )?;
        // The name is the unauthenticated requester's claim; receivers are
        // told so.
        self.notify(&crate::webhooks::Notice {
            kind: cp_api::WebhookEventType::PeeringRequested,
            origin: cp_api::WebhookEventOrigin::Incoming,
            subject: id,
            event_id: None,
            peer: req.name.clone(),
            state: None,
            my_turn: None,
        })?;
        Ok(RequestReceipt {
            status: ReceiptStatus::Pending,
            code: code(&req.key, &self.key)?,
            key: self.key.clone(),
        })
    }

    /// Pending peering requests from the last week, oldest first.
    pub fn requests(&self) -> Result<RequestList, Error> {
        let rows = self.sql.query(
            "SELECT id, name, url, key, note, at FROM requests WHERE at >= ? ORDER BY at, id",
            &[self.now().plus(-REQUEST_TTL).0.into()],
        )?;
        let mut requests = Vec::with_capacity(rows.len());
        for r in &rows {
            requests.push(Request {
                id: r.parse(0)?,
                name: r.parse(1)?,
                url: r.parse(2)?,
                code: code(&r.parse(3)?, &self.key)?,
                note: r.opt_parse(4)?,
                at: Millis(r.int(5)?).at(),
            });
        }
        Ok(RequestList { requests })
    }

    /// Starts approving a request: checks the name, records the approval as
    /// confirming, and returns the signed call telling the requester's daemon,
    /// at the URL it gave, that it is accepted. Until
    /// [`Core::commit_approval`] or [`Core::abort_approval`], or the record
    /// expires, calls signed with the requester's key (or the key of the
    /// peer it replaces) answer 503 to retry.
    pub fn begin_approval(
        &mut self,
        id: &RequestRef,
        approval: Approval,
    ) -> Result<PendingApproval, Error> {
        let now = self.now();
        let r = self
            .sql
            .query_one(
                "SELECT name, url, key FROM requests WHERE id = ? AND at >= ?",
                &[text(id.as_str()), now.plus(-REQUEST_TTL).0.into()],
            )?
            .ok_or_else(|| {
                err(
                    ErrorKind::NotFound,
                    format!("no pending request {:?}", id.as_str()),
                )
            })?;
        let name: Name = match approval.name {
            Some(n) => n,
            None => r.parse(0)?,
        };
        let url: DaemonUrl = r.parse(1)?;
        let key: PublicKey = r.parse(2)?;
        let previous = self.approvable(&name, &url)?;
        if self.live_op()?.is_some() {
            return Err(err(
                ErrorKind::Conflict,
                "another peering approval is confirming; retry in a few seconds",
            ));
        }
        self.sql.execute(
            "INSERT OR REPLACE INTO peering_op (slot, request_id, name, url, key, previous_key, expires_at) VALUES (1, ?, ?, ?, ?, ?, ?)",
            &[
                text(id.as_str()),
                text(&name),
                text(url.as_str()),
                text(key.as_str()),
                previous.as_ref().map(|p| p.key.to_string()).into(),
                now.plus(CONFIRM_TTL).0.into(),
            ],
        )?;
        let path = "/api/v1/federation/accepted";
        let headers = self.sign("POST", path, &key, b"")?;
        Ok(PendingApproval {
            request: id.clone(),
            name,
            call: Call {
                url,
                path,
                headers,
                body: Vec::new(),
            },
        })
    }

    /// Finishes an approval the requester's daemon confirmed: the requester
    /// becomes an active peer under the chosen name, replacing a previous
    /// peer of that name and URL.
    pub fn commit_approval(&mut self, pending: &PendingApproval) -> Result<Peer, Error> {
        let op = self
            .live_op()?
            .filter(|r| r.text(0).is_ok_and(|id| id == pending.request.as_str()))
            .ok_or_else(|| err(ErrorKind::Conflict, "the approval timed out; approve again"))?;
        let name: Name = op.parse(1)?;
        let url: DaemonUrl = op.parse(2)?;
        let key: PublicKey = op.parse(3)?;
        let previous = self.approvable(&name, &url)?;
        if previous.as_ref().map(|p| p.key.to_string()) != op.opt_text(4)?.map(str::to_string) {
            return Err(err(
                ErrorKind::Conflict,
                format!("{name} changed while approving; approve again"),
            ));
        }
        let now = self.now();
        self.sql.execute(
            "DELETE FROM peers WHERE name = ? OR key = ?",
            &[text(&name), text(key.as_str())],
        )?;
        self.sql.execute(
            "INSERT INTO peers (name, url, key, status, added_at) VALUES (?, ?, ?, 'active', ?)",
            &[
                text(&name),
                text(url.as_str()),
                text(key.as_str()),
                now.0.into(),
            ],
        )?;
        self.sql
            .execute("DELETE FROM requests WHERE url = ?", &[text(url.as_str())])?;
        self.sql.execute("DELETE FROM peering_op", &[])?;
        self.reschedule(&name)?;
        self.peer(&name)
    }

    /// Ends an approval the requester's daemon did not confirm. The request
    /// and any previous peer are left as they were.
    pub fn abort_approval(&mut self, pending: &PendingApproval) -> Result<(), Error> {
        self.sql.execute(
            "DELETE FROM peering_op WHERE request_id = ?",
            &[text(pending.request.as_str())],
        )?;
        Ok(())
    }

    /// Drops a peering request; nothing is sent to the requester.
    pub fn deny(&mut self, id: &RequestRef) -> Result<(), Error> {
        if self
            .live_op()?
            .is_some_and(|r| r.text(0).is_ok_and(|op| op == id.as_str()))
        {
            return Err(err(
                ErrorKind::Conflict,
                "that request is being approved; retry in a few seconds",
            ));
        }
        if self
            .sql
            .execute("DELETE FROM requests WHERE id = ?", &[text(id.as_str())])?
            == 0
        {
            return Err(err(
                ErrorKind::NotFound,
                format!("no pending request {:?}", id.as_str()),
            ));
        }
        Ok(())
    }

    /// Forgets a peer: their key stops working and events not yet
    /// delivered to them fail. Threads stay, and their name stays with their
    /// URL.
    pub fn remove_peer(&mut self, name: &Name) -> Result<(), Error> {
        let p = self
            .load_peer(name)?
            .ok_or_else(|| err(ErrorKind::NotFound, format!("{name} is not a peer")))?;
        if self
            .live_op()?
            .is_some_and(|r| r.text(1).is_ok_and(|n| n == name.as_str()))
        {
            return Err(err(
                ErrorKind::Conflict,
                format!("{name} is being approved; retry in a few seconds"),
            ));
        }
        if self
            .sql
            .query_one(
                "SELECT 1 FROM threads WHERE peer = ? LIMIT 1",
                &[text(name)],
            )?
            .is_some()
        {
            self.sql.execute(
                "INSERT OR REPLACE INTO former_peers (name, url) VALUES (?, ?)",
                &[text(name), text(p.url.as_str())],
            )?;
        }
        self.sql
            .execute("DELETE FROM peers WHERE name = ?", &[text(name)])?;
        let failed = self.sql.query(
            "SELECT thread, count(*) FROM outbox WHERE peer = ? AND status = 'pending' GROUP BY thread",
            &[text(name)],
        )?;
        self.sql.execute(
            "UPDATE outbox SET status = 'failed', last_error = 'peer removed', next_attempt_at = NULL WHERE peer = ? AND status = 'pending'",
            &[text(name)],
        )?;
        for r in &failed {
            let n = r.count(1)?;
            self.sql.execute(
                "UPDATE threads SET undelivered = max(undelivered - ?2, 0), failed = failed + ?2 WHERE id = ?1",
                &[text(r.text(0)?), n.into()],
            )?;
            if let Ok(id) = r.parse(0) {
                self.effects.changed(&id);
            }
        }
        Ok(())
    }

    /// Records the release `name`'s daemon reported, when it changed.
    pub fn note_peer_version(&self, name: &Name, version: &Version) -> Result<(), Error> {
        self.sql.execute(
            "UPDATE peers SET version = ?1 WHERE name = ?2 AND version IS NOT ?1",
            &[text(version.as_str()), text(name)],
        )?;
        Ok(())
    }

    /// The peer that signed `call`. While an approval confirms the caller's
    /// key, or the key of the peer it replaces, calls answer 503 to retry:
    /// the other daemon may act on its acceptance before this one saved it.
    pub fn authenticate_peer(&self, call: &SignedCall) -> Result<Name, Error> {
        let refused = || err(ErrorKind::Unauthorized, "unknown peer key or bad signature");
        let now = self.now().0.div_euclid(Millis::SECOND);
        if (call.date - now).abs() > MAX_SKEW_SECS {
            return Err(err(
                ErrorKind::Unauthorized,
                "signature time is more than five minutes from this daemon's clock",
            ));
        }
        if !call.verifies(&self.key) {
            return Err(refused());
        }
        if let Some(op) = self.live_op()?
            && (op.text(3)? == call.key.as_str() || op.opt_text(4)? == Some(call.key.as_str()))
        {
            return Err(err(
                ErrorKind::Unavailable,
                "this peering is being confirmed; retry shortly",
            )
            .retry_after(2));
        }
        self.peer_with_key(&call.key)?.ok_or_else(refused)
    }

    /// The peer known by `key`, if any.
    pub fn peer_with_key(&self, key: &PublicKey) -> Result<Option<Name>, Error> {
        self.sql
            .query_one(
                "SELECT name FROM peers WHERE key = ?",
                &[text(key.as_str())],
            )?
            .map(|r| r.parse(0))
            .transpose()
    }

    /// The daemon with `key`, which the host has authenticated, approved this
    /// one's request: the peer is active and events queued for them are due
    /// now.
    pub fn accepted(&mut self, key: &PublicKey) -> Result<(), Error> {
        let name = self
            .peer_with_key(key)?
            .ok_or_else(|| err(ErrorKind::Unauthorized, "unknown peer key"))?;
        self.peer_heard_from(&name)
    }

    /// A peer proved it holds its key (it delivered or confirmed), so a
    /// requested peer has been approved.
    pub(crate) fn peer_heard_from(&mut self, name: &Name) -> Result<(), Error> {
        let changed = self.sql.execute(
            "UPDATE peers SET status = 'active' WHERE name = ? AND status = 'requested'",
            &[text(name)],
        )?;
        if changed > 0 {
            if let Some(p) = self.load_peer(name)? {
                self.sql.execute(
                    "DELETE FROM requests WHERE url = ?",
                    &[text(p.url.as_str())],
                )?;
            }
            self.reschedule(name)?;
        }
        Ok(())
    }

    /// Makes a peer's queued events due now.
    fn reschedule(&mut self, name: &Name) -> Result<(), Error> {
        let n = self.sql.execute(
            "UPDATE outbox SET next_attempt_at = ? WHERE peer = ? AND status = 'pending'",
            &[self.now().0.into(), text(name)],
        )?;
        if n > 0 {
            self.effects.deliver = true;
        }
        Ok(())
    }
}
