//! Secrets: minting, hashing for lookup, and the short code owners compare.

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cp_api::{Code, Error, ErrorKind};
use sha2::{Digest, Sha256};

use crate::runtime::Runtime;

/// A new secret: `prefix` and 32 random bytes, base64url (43 characters).
pub(crate) fn mint(rt: &mut impl Runtime, prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rt.random(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The SHA-256 of a secret, in hex: how stored secrets are looked up. A
/// secret has 256 random bits, so a fast hash is enough.
pub(crate) fn hash(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

/// A secret's short code: the first four bytes of its SHA-256, `xxxx-xxxx`.
pub(crate) fn code(secret: &str) -> Result<Code, Error> {
    let digest = Sha256::digest(secret.as_bytes());
    let h = hex(digest.get(..4).unwrap_or_default());
    let (a, b) = h.split_at(4.min(h.len()));
    Code::try_from(format!("{a}-{b}")).map_err(|e| Error::new(ErrorKind::Internal, e.to_string()))
}

/// Random lower-case hex, `bytes` long before encoding.
pub(crate) fn random_hex(rt: &mut impl Runtime, bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rt.random(&mut buf);
    hex(&buf)
}

/// Compares secrets in constant time.
pub(crate) fn equal(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq as _;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
