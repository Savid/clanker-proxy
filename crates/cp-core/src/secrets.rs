//! Secrets: hashing tokens for storage and lookup, and random identifiers.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::runtime::Runtime;

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

/// The hash a daemon keeps of its owner's token, so the token itself is
/// stored only where the owner keeps it.
pub fn token_hash(token: &str) -> String {
    hash(token)
}

/// Random lower-case hex, `bytes` long before encoding.
pub(crate) fn random_hex(rt: &mut impl Runtime, bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rt.random(&mut buf);
    hex(&buf)
}
