//! Signatures on calls between daemons. Each daemon has an Ed25519 key and
//! peers store only each other's public keys, so a leaked database cannot be
//! used to pose as anyone else, and no secret ever crosses the wire.
//!
//! A call signs `clanker-proxy/1`, its method, its API path, the Unix time,
//! the recipient's public key and the body's SHA-256, one per line. Naming
//! the recipient keeps a call to one peer from being replayed to another;
//! the time bounds replays to the same peer, whose calls are idempotent.
//! A peering request goes out before the recipient's key is known, so it
//! names the requester's own key instead.

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cp_api::{Code, DATE_HEADER, Error, ErrorKind, KEY_HEADER, PublicKey, SIGNATURE_HEADER};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use sha2::{Digest as _, Sha256};

use crate::runtime::Runtime;

/// How far a call's time may be from this daemon's clock, either way.
pub const MAX_SKEW_SECS: i64 = 300;

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// A body's SHA-256 in hex, as signatures cover it.
pub fn body_digest(body: &[u8]) -> String {
    hex(&Sha256::digest(body))
}

fn message(method: &str, path: &str, date: i64, recipient: &PublicKey, digest: &str) -> String {
    format!("clanker-proxy/1\n{method}\n{path}\n{date}\n{recipient}\n{digest}")
}

/// The code two owners compare before peering: the first four bytes of the
/// SHA-256 of both daemons' keys, in either order, as `xxxx-xxxx`.
pub fn code(one: &PublicKey, other: &PublicKey) -> Result<Code, Error> {
    let (first, second) = if one.as_str() <= other.as_str() {
        (one, other)
    } else {
        (other, one)
    };
    let digest = Sha256::digest(format!("clanker-proxy/peering\n{first}\n{second}").as_bytes());
    let short = hex(digest.get(..4).unwrap_or_default());
    let (left, right) = short.split_at(4.min(short.len()));
    Code::try_from(format!("{left}-{right}"))
        .map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))
}

/// An incoming call's signature and what it covers, as the host read them
/// from the request. [`crate::Core::authenticate_peer`] checks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedCall {
    pub key: PublicKey,
    pub date: i64,
    pub signature: String,
    pub method: String,
    /// The API path, such as `/api/v1/federation/events`: what the caller
    /// signed, whatever prefix a proxy in front of either daemon adds.
    pub path: String,
    pub digest: String,
}

impl SignedCall {
    /// From a request's parts; `None` when a header is missing or malformed.
    pub fn new<'a>(
        header: impl Fn(&str) -> Option<&'a str>,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Option<Self> {
        Some(Self {
            key: PublicKey::try_from(header(KEY_HEADER)?.to_string()).ok()?,
            date: header(DATE_HEADER)?.parse().ok()?,
            signature: header(SIGNATURE_HEADER)?.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            digest: body_digest(body),
        })
    }

    /// Whether the signature is the caller's, over this call to `recipient`.
    pub(crate) fn verifies(&self, recipient: &PublicKey) -> bool {
        let key = URL_SAFE_NO_PAD
            .decode(self.key.as_str())
            .ok()
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
            .and_then(|k| VerifyingKey::from_bytes(&k).ok());
        let signature = URL_SAFE_NO_PAD
            .decode(&self.signature)
            .ok()
            .and_then(|s| <[u8; 64]>::try_from(s).ok())
            .map(|s| Signature::from_bytes(&s));
        let (Some(key), Some(signature)) = (key, signature) else {
            return false;
        };
        let m = message(&self.method, &self.path, self.date, recipient, &self.digest);
        key.verify_strict(m.as_bytes(), &signature).is_ok()
    }
}

/// This daemon's key. Its private half never leaves the daemon's storage.
pub(crate) struct Identity(SigningKey);

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Identity([redacted])")
    }
}

impl Identity {
    pub(crate) fn generate(rt: &mut impl Runtime) -> Self {
        let mut seed = [0u8; 32];
        rt.random(&mut seed);
        Self(SigningKey::from_bytes(&seed))
    }

    /// The private key as stored.
    pub(crate) fn stored(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0.to_bytes())
    }

    pub(crate) fn from_stored(text: &str) -> Result<Self, Error> {
        URL_SAFE_NO_PAD
            .decode(text)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .map(|seed| Self(SigningKey::from_bytes(&seed)))
            .ok_or_else(|| Error::new(ErrorKind::Internal, "stored signing key is malformed"))
    }

    pub(crate) fn public(&self) -> Result<PublicKey, Error> {
        PublicKey::try_from(URL_SAFE_NO_PAD.encode(self.0.verifying_key().to_bytes()))
            .map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))
    }

    /// The headers that sign a call to `recipient`, made at `date`.
    pub(crate) fn sign(
        &self,
        method: &str,
        path: &str,
        date: i64,
        recipient: &PublicKey,
        body: &[u8],
    ) -> Result<Vec<(String, String)>, Error> {
        let m = message(method, path, date, recipient, &body_digest(body));
        let signature = self.0.sign(m.as_bytes());
        Ok(vec![
            (KEY_HEADER.to_string(), self.public()?.to_string()),
            (DATE_HEADER.to_string(), date.to_string()),
            (
                SIGNATURE_HEADER.to_string(),
                URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            ),
        ])
    }
}
