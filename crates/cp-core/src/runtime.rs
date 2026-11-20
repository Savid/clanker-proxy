//! What the core takes from its host instead of reading itself: the time
//! and randomness.

use cp_thread::At;

/// A time, in Unix milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Millis(pub i64);

impl Millis {
    pub const SECOND: i64 = 1000;
    pub const MINUTE: i64 = 60 * Self::SECOND;
    pub const HOUR: i64 = 60 * Self::MINUTE;
    pub const DAY: i64 = 24 * Self::HOUR;

    /// This time `ms` milliseconds later (earlier when negative).
    #[must_use]
    pub fn plus(self, ms: i64) -> Self {
        Self(self.0.saturating_add(ms))
    }

    /// The time to the second, as the API reports it. Times outside what
    /// a clock or the wire produce read as the epoch.
    pub fn at(self) -> At {
        At::from_unix_seconds(self.0.div_euclid(Self::SECOND)).unwrap_or(At::EPOCH)
    }

    pub fn from_at(at: At) -> Self {
        Self(at.as_unix_seconds().saturating_mul(Self::SECOND))
    }
}

/// The host's clock and randomness.
pub trait Runtime {
    /// The current time.
    fn now(&self) -> Millis;

    /// Fills `buf` with cryptographically secure random bytes.
    fn random(&mut self, buf: &mut [u8]);
}
