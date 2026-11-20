//! Agent tokens: bearer tokens the owner gives agents, limited to working
//! threads, optionally only some peers'.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cp_api::{
    AgentTokenInput, AgentTokenList, AgentTokenSummary, Error, ErrorKind, NewAgentToken, Peers,
    Token, prefix,
};
use cp_thread::Name;

use crate::core::Core;
use crate::runtime::{Millis, Runtime};
use crate::secrets;
use crate::sql::{Row, Sql, text};

/// How often a token's last use is recorded: to the minute, not every call.
const TOUCH_EVERY: i64 = Millis::MINUTE;

/// Who an agent token lets in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub name: Name,
    /// The peers whose threads it reaches; empty means every peer.
    pub peers: Peers,
}

fn peers_from(r: &Row, i: usize) -> Result<Peers, Error> {
    serde_json::from_str(r.text(i)?)
        .map_err(|_| Error::new(ErrorKind::Internal, "stored token peers"))
}

impl<S: Sql, R: Runtime> Core<S, R> {
    /// Checks a list of peers names only peers or former peers.
    pub(crate) fn known_peers(&self, peers: &Peers) -> Result<(), Error> {
        for p in peers.as_slice() {
            if !self.known_peer(p)? {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("no peer is named {p}"),
                ));
            }
        }
        Ok(())
    }

    /// Makes an agent token. The token is returned this once; only its hash
    /// is kept.
    pub fn create_agent_token(&mut self, input: AgentTokenInput) -> Result<NewAgentToken, Error> {
        let now = self.now();
        self.known_peers(&input.peers)?;
        let expires = input.expires_at.map(Millis::from_at);
        if expires.is_some_and(|t| t <= now) {
            return Err(Error::new(
                ErrorKind::Invalid,
                "expiresAt must be in the future",
            ));
        }
        // An expired token gives up its name.
        self.sql.execute(
            "DELETE FROM agent_tokens WHERE name = ? AND expires_at IS NOT NULL AND expires_at <= ?",
            &[text(&input.name), now.0.into()],
        )?;
        if self
            .sql
            .query_one(
                "SELECT 1 FROM agent_tokens WHERE name = ?",
                &[text(&input.name)],
            )?
            .is_some()
        {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "an agent token named {} exists; revoke it or choose another name",
                    input.name
                ),
            ));
        }
        let mut bytes = [0u8; 32];
        self.rt.random(&mut bytes);
        let token = format!("{}{}", prefix::AGENT, URL_SAFE_NO_PAD.encode(bytes));
        self.sql.execute(
            "INSERT INTO agent_tokens (name, hash, peers, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
            &[
                text(&input.name),
                secrets::hash(&token).into(),
                serde_json::to_string(&input.peers).unwrap_or_else(|_| "[]".into()).into(),
                now.0.into(),
                expires.map(|t| t.0).into(),
            ],
        )?;
        Ok(NewAgentToken {
            name: input.name,
            peers: input.peers,
            token: Token::new(token).map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))?,
            created_at: now.at(),
            expires_at: expires.map(Millis::at),
        })
    }

    /// Every agent token, by name, without the tokens.
    pub fn agent_tokens(&self) -> Result<AgentTokenList, Error> {
        let rows = self.sql.query(
            "SELECT name, peers, created_at, expires_at, used_at FROM agent_tokens ORDER BY name",
            &[],
        )?;
        let mut tokens = Vec::with_capacity(rows.len());
        for r in &rows {
            tokens.push(AgentTokenSummary {
                name: r.parse(0)?,
                peers: peers_from(r, 1)?,
                created_at: Millis(r.int(2)?).at(),
                expires_at: r.opt_int(3)?.map(|t| Millis(t).at()),
                used_at: r.opt_int(4)?.map(|t| Millis(t).at()),
            });
        }
        Ok(AgentTokenList { tokens })
    }

    /// Revokes an agent token.
    pub fn delete_agent_token(&mut self, name: &Name) -> Result<(), Error> {
        if self
            .sql
            .execute("DELETE FROM agent_tokens WHERE name = ?", &[text(name)])?
            == 0
        {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("no agent token named {name}"),
            ));
        }
        Ok(())
    }

    /// The agent a token lets in, recording its use at most once a minute.
    pub fn authenticate_agent(&mut self, token: &str) -> Result<Agent, Error> {
        if !token.starts_with(prefix::AGENT) {
            return Err(Error::new(ErrorKind::Unauthorized, "not an agent token"));
        }
        let now = self.now();
        let r = self
            .sql
            .query_one(
                "SELECT name, peers, expires_at, used_at FROM agent_tokens WHERE hash = ?",
                &[secrets::hash(token).into()],
            )?
            .ok_or_else(|| Error::new(ErrorKind::Unauthorized, "unknown agent token"))?;
        if r.opt_int(2)?.is_some_and(|t| now.0 >= t) {
            return Err(Error::new(ErrorKind::Unauthorized, "agent token expired"));
        }
        let name: Name = r.parse(0)?;
        if r.opt_int(3)?.is_none_or(|t| now.0 - t >= TOUCH_EVERY) {
            self.sql.execute(
                "UPDATE agent_tokens SET used_at = ? WHERE name = ?",
                &[now.0.into(), text(&name)],
            )?;
        }
        Ok(Agent {
            name,
            peers: peers_from(&r, 1)?,
        })
    }
}
