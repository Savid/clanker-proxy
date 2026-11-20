//! When an event happened, by its author's clock.

use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// A UTC instant to the second, written `YYYY-MM-DDTHH:MM:SSZ` and nothing
/// else, so two daemons encode the same event identically. It records the
/// author's wall clock only; replay order comes from Lamport clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct At(Timestamp);

/// Why a time was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0:?}: want a UTC time to the second, as YYYY-MM-DDTHH:MM:SSZ")]
pub struct AtError(String);

impl At {
    /// The instant `seconds` after the Unix epoch.
    pub fn from_unix_seconds(seconds: i64) -> Result<Self, AtError> {
        Timestamp::from_second(seconds)
            .map(Self)
            .map_err(|_| AtError(seconds.to_string()))
    }

    /// `t` with any fraction of a second dropped. Hosts pass their clock's
    /// reading through this.
    pub fn truncate(t: Timestamp) -> Self {
        let seconds = t.as_second();
        Self(Timestamp::from_second(seconds).unwrap_or(t))
    }

    pub fn as_unix_seconds(self) -> i64 {
        self.0.as_second()
    }

    pub fn timestamp(self) -> Timestamp {
        self.0
    }
}

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for At {
    type Err = AtError;

    fn from_str(s: &str) -> Result<Self, AtError> {
        let t: Timestamp = s.parse().map_err(|_| AtError(s.to_string()))?;
        let at = Self(t);
        // Only the canonical form round-trips: an offset or a lower-case
        // separator would not.
        if t.subsec_nanosecond() == 0 && at.to_string() == s {
            Ok(at)
        } else {
            Err(AtError(s.to_string()))
        }
    }
}

impl TryFrom<String> for At {
    type Error = AtError;

    fn try_from(s: String) -> Result<Self, AtError> {
        s.parse()
    }
}

impl From<At> for String {
    fn from(at: At) -> Self {
        at.to_string()
    }
}
