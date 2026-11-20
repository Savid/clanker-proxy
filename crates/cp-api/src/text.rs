//! Bounded text the API adds to the thread types: URLs, codes, references,
//! notes, peer lists, and secrets that are written but never read back.

use std::fmt;
use std::net::IpAddr;

use cp_thread::Name;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Type};
use utoipa::openapi::{RefOr, Schema};
use utoipa::{PartialSchema, ToSchema};

/// Why a value was refused. The message never echoes a secret.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

/// The schema of a bounded string.
struct Bounds {
    min: Option<usize>,
    max: usize,
    pattern: Option<&'static str>,
    write_only: bool,
    description: &'static str,
    example: Option<&'static str>,
}

impl Bounds {
    fn schema(&self) -> RefOr<Schema> {
        let mut s = ObjectBuilder::new()
            .schema_type(Type::String)
            .description(Some(self.description))
            .min_length(self.min)
            .max_length(Some(self.max))
            .pattern(self.pattern)
            .write_only(self.write_only);
        if let Some(example) = self.example {
            s = s.examples([example]);
        }
        s.build().into()
    }
}

macro_rules! bounded {
    ($(#[$doc:meta])* $name:ident, $check:expr, $bounds:expr) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = Invalid;

            fn try_from(s: String) -> Result<Self, Invalid> {
                let check: fn(String) -> Result<String, Invalid> = $check;
                check(s).map(Self)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = Invalid;

            fn try_from(s: &str) -> Result<Self, Invalid> {
                Self::try_from(s.to_string())
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl PartialSchema for $name {
            fn schema() -> RefOr<Schema> {
                let bounds: Bounds = $bounds;
                bounds.schema()
            }
        }

        impl ToSchema for $name {}
    };
}

/// Longest daemon URL, in bytes.
pub const MAX_URL: usize = 512;

fn loopback(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        Some(url::Host::Domain(_)) | None => false,
    }
}

fn daemon_url(s: String) -> Result<String, Invalid> {
    let refuse = || {
        Invalid(format!(
            "url {s:?}: want https://host[:port][/path], or http on a loopback address"
        ))
    };
    if s.len() > MAX_URL {
        return Err(Invalid(format!("url is {} bytes, over {MAX_URL}", s.len())));
    }
    let u = url::Url::parse(&s).map_err(|_| refuse())?;
    let scheme_ok = match u.scheme() {
        "https" => true,
        // A peer's secret travels in every request, so plain HTTP is only
        // for daemons on the same machine.
        "http" => loopback(&u),
        _ => false,
    };
    if !scheme_ok
        || u.host().is_none()
        || u.cannot_be_a_base()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(refuse());
    }
    let mut s = s;
    while s.ends_with('/') {
        s.pop();
    }
    Ok(s)
}

bounded!(
    /// A daemon's base URL: https, or http on a loopback address; no
    /// userinfo, query or fragment; a trailing slash is dropped.
    DaemonUrl,
    daemon_url,
    Bounds {
        min: Some(1),
        max: MAX_URL,
        pattern: None,
        write_only: false,
        description: "A daemon's base URL: https, or http on a literal loopback address, without userinfo, query or fragment. A trailing slash is dropped.",
        example: Some("https://cp.example.com"),
    }
);

impl DaemonUrl {
    /// The URL of an API path on this daemon.
    pub fn join(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }
}

/// Longest peering note, in characters.
pub const MAX_NOTE: usize = 500;

bounded!(
    /// A one-line note a peering request carries for the other owner.
    Note,
    |s| {
        if s.chars().count() <= MAX_NOTE
            && !s.chars().any(|c| c.is_control() || cp_thread::is_hidden(c))
        {
            Ok(s)
        } else {
            Err(Invalid(format!(
                "note: want one line of at most {MAX_NOTE} characters"
            )))
        }
    },
    Bounds {
        min: None,
        max: MAX_NOTE,
        pattern: None,
        write_only: false,
        description: "A note for the other owner: one line of at most 500 characters.",
        example: Some("hey it's alice"),
    }
);

fn lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

bounded!(
    /// The short code two owners compare before peering: the first four
    /// bytes of a SHA-256 over both daemons' public keys, as `xxxx-xxxx`.
    Code,
    |s| match (s.get(..4), s.get(4..5), s.get(5..)) {
        (Some(a), Some("-"), Some(b))
            if a.len() == 4 && b.len() == 4 && lower_hex(a) && lower_hex(b) =>
            Ok(s),
        _ => Err(Invalid(format!(
            "code {s:?}: want xxxx-xxxx in lower-case hex"
        ))),
    },
    Bounds {
        min: Some(9),
        max: 9,
        pattern: Some("^[0-9a-f]{4}-[0-9a-f]{4}$"),
        write_only: false,
        description: "A secret's short code: compare it with the other owner before approving.",
        example: Some("3e7b-10c2"),
    }
);

bounded!(
    /// A peering request's reference.
    RequestRef,
    |s| if s.len() == 8 && lower_hex(&s) {
        Ok(s)
    } else {
        Err(Invalid(format!(
            "request {s:?}: want 8 lower-case hex digits"
        )))
    },
    Bounds {
        min: Some(8),
        max: 8,
        pattern: Some("^[0-9a-f]{8}$"),
        write_only: false,
        description: "A peering request's reference.",
        example: Some("3e7b10c2"),
    }
);

bounded!(
    /// A thread ID or a unique prefix of one, at least four characters.
    ThreadRef,
    |s| {
        let s = s.to_ascii_lowercase();
        if (4..=36).contains(&s.len())
            && s.bytes()
                .all(|c| c == b'-' || c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            Ok(s)
        } else {
            Err(Invalid(format!(
                "thread {s:?}: want a thread ID or a prefix of at least 4 of its characters"
            )))
        }
    },
    Bounds {
        min: Some(4),
        max: 36,
        pattern: Some("^[0-9a-fA-F-]{4,36}$"),
        write_only: false,
        description: "A thread ID, or a unique prefix of at least 4 characters.",
        example: Some("765a0b0c"),
    }
);

bounded!(
    /// A page cursor for webhook deliveries.
    Cursor,
    |s| if (1..=19).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_digit()) {
        Ok(s)
    } else {
        Err(Invalid("invalid cursor".into()))
    },
    Bounds {
        min: Some(1),
        max: 19,
        pattern: Some("^[0-9]{1,19}$"),
        write_only: false,
        description: "Pass as cursor to fetch older records.",
        example: None,
    }
);

bounded!(
    /// Unix seconds, as the Webhook-Timestamp header carries them.
    UnixTime,
    |s| if (1..=12).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_digit()) {
        Ok(s)
    } else {
        Err(Invalid("want Unix seconds".into()))
    },
    Bounds {
        min: Some(1),
        max: 12,
        pattern: Some("^[0-9]{1,12}$"),
        write_only: false,
        description: "Unix seconds.",
        example: Some("1791331200"),
    }
);

bounded!(
    /// A daemon's Ed25519 public key: 32 bytes, base64url without padding.
    /// Peers know each other by it; every call between daemons is signed by
    /// the matching private key, which never leaves its daemon.
    PublicKey,
    |s| if s.len() == 43
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        Ok(s)
    } else {
        Err(Invalid("public key: want 43 base64url characters".into()))
    },
    Bounds {
        min: Some(43),
        max: 43,
        pattern: Some("^[A-Za-z0-9_-]{43}$"),
        write_only: false,
        description: "A daemon's Ed25519 public key, base64url without padding.",
        example: Some("aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789-_aBcDe"),
    }
);

bounded!(
    /// A daemon's release, as `--version` prints it: `v` and a semantic
    /// version.
    Version,
    |s| if (2..=32).contains(&s.len())
        && s.starts_with('v')
        && s.bytes()
            .skip(1)
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+'))
    {
        Ok(s)
    } else {
        Err(Invalid(format!(
            "version {s:?}: want v and a semantic version"
        )))
    },
    Bounds {
        min: Some(2),
        max: 32,
        pattern: Some("^v[0-9A-Za-z.+-]{1,31}$"),
        write_only: false,
        description: "A daemon's release.",
        example: Some("v0.2.2"),
    }
);

impl Version {
    /// Major, minor and patch, ignoring any pre-release or build suffix;
    /// `None` for a version that is not semantic.
    pub fn triple(&self) -> Option<(u64, u64, u64)> {
        let core = self.as_str().get(1..)?.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(str::parse::<u64>);
        let v = (
            parts.next()?.ok()?,
            parts.next()?.ok()?,
            parts.next()?.ok()?,
        );
        parts.next().is_none().then_some(v)
    }

    /// Whether this is a later release than `other`; false when either is
    /// not a semantic version.
    pub fn newer_than(&self, other: &Version) -> bool {
        matches!((self.triple(), other.triple()), (Some(a), Some(b)) if a > b)
    }
}

fn header_name(s: String) -> Result<String, Invalid> {
    let token = |c: u8| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c);
    if (1..=64).contains(&s.len()) && s.bytes().all(token) {
        Ok(s)
    } else {
        Err(Invalid("invalid webhook header name".into()))
    }
}

bounded!(
    /// An HTTP header name for a webhook.
    WebhookHeaderName,
    header_name,
    Bounds {
        min: Some(1),
        max: 64,
        pattern: Some("^[A-Za-z0-9!#$%&'*+.^_`|~-]+$"),
        write_only: false,
        description: "An HTTP header name. Transport, Content-Type, User-Agent, Idempotency-Key, Proxy-* and Webhook-* are reserved.",
        example: Some("Authorization"),
    }
);

/// Most peers a list may name.
pub const MAX_PEERS: usize = 50;

/// The peers something applies to; empty means every peer, including ones
/// added later.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<Name>", into = "Vec<Name>")]
pub struct Peers(Vec<Name>);

impl Peers {
    pub fn as_slice(&self) -> &[Name] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `peer` is in reach: listed, or the list is empty.
    pub fn reaches(&self, peer: &Name) -> bool {
        self.0.is_empty() || self.0.contains(peer)
    }
}

impl TryFrom<Vec<Name>> for Peers {
    type Error = Invalid;

    fn try_from(peers: Vec<Name>) -> Result<Self, Invalid> {
        if peers.len() > MAX_PEERS {
            return Err(Invalid(format!("at most {MAX_PEERS} peers")));
        }
        for (i, p) in peers.iter().enumerate() {
            if peers.iter().take(i).any(|q| q == p) {
                return Err(Invalid(format!("{p} is listed twice")));
            }
        }
        Ok(Self(peers))
    }
}

impl From<Peers> for Vec<Name> {
    fn from(p: Peers) -> Self {
        p.0
    }
}

impl PartialSchema for Peers {
    fn schema() -> RefOr<Schema> {
        ArrayBuilder::new()
            .items(Name::schema())
            .max_items(Some(MAX_PEERS))
            .unique_items(true)
            .description(Some(
                "Peers this applies to, by name; each must be a peer or a former one. Empty: every peer, including peers added later.",
            ))
            .build()
            .into()
    }
}

impl ToSchema for Peers {}

macro_rules! secret {
    ($(#[$doc:meta])* $name:ident, $check:expr, $bounds:expr) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $name(SecretString);

        impl $name {
            pub fn new(s: String) -> Result<Self, Invalid> {
                let check: fn(&str) -> Result<(), Invalid> = $check;
                check(&s)?;
                Ok(Self(SecretString::from(s)))
            }

            /// The secret itself, for the one place that must use it.
            pub fn expose(&self) -> &str {
                self.0.expose_secret()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([redacted])"))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.expose())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
            }
        }

        impl PartialSchema for $name {
            fn schema() -> RefOr<Schema> {
                let bounds: Bounds = $bounds;
                bounds.schema()
            }
        }

        impl ToSchema for $name {}
    };
}

fn base64url_secret(prefix: &'static str) -> impl Fn(&str) -> bool {
    move |s| {
        s.strip_prefix(prefix).is_some_and(|rest| {
            rest.len() == 43
                && rest
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        })
    }
}

secret!(
    /// A bearer token handed to its holder once.
    Token,
    |s| if base64url_secret("cpa_")(s) || base64url_secret("cpo_")(s) {
        Ok(())
    } else {
        Err(Invalid(
            "token: want cpa_ or cpo_ and 43 base64url characters".into(),
        ))
    },
    Bounds {
        min: Some(47),
        max: 47,
        pattern: Some("^cp[ao]_[A-Za-z0-9_-]{43}$"),
        write_only: false,
        description: "A bearer token. Give it to the agent as CP_TOKEN; it is never shown again.",
        example: None,
    }
);

/// Longest webhook URL, in bytes.
pub const MAX_WEBHOOK_URL: usize = 2048;

secret!(
    /// Where a webhook goes. Paths and queries can hold provider
    /// credentials, so the URL is written and never read back.
    WebhookUrl,
    |s| if (1..=MAX_WEBHOOK_URL).contains(&s.len()) {
        Ok(())
    } else {
        Err(Invalid(format!(
            "webhook URL must be 1 to {MAX_WEBHOOK_URL} bytes"
        )))
    },
    Bounds {
        min: Some(1),
        max: MAX_WEBHOOK_URL,
        pattern: None,
        write_only: true,
        description: "A webhook destination: https, or http on a literal loopback address, without userinfo or fragment. Write-only, since paths and queries may hold provider credentials.",
        example: Some("https://runner.example.com/hooks"),
    }
);

secret!(
    /// A webhook signing key: the base64 of 32 bytes, or empty for none.
    WebhookSecret,
    |s| {
        let ok = s.is_empty()
            || (s.len() == 44
                && s.ends_with('=')
                && s.bytes()
                    .take(43)
                    .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/'));
        if ok {
            Ok(())
        } else {
            Err(Invalid(
                "webhook secret must be the base64 encoding of exactly 32 bytes".into(),
            ))
        }
    },
    Bounds {
        min: None,
        max: 44,
        pattern: Some("^([A-Za-z0-9+/]{43}=)?$"),
        write_only: true,
        description: "The base64 encoding of a 32-byte signing key, for generic destinations only. Empty disables signing.",
        example: None,
    }
);

secret!(
    /// A webhook header's value, which may carry credentials.
    WebhookHeaderValue,
    |s| if (1..=4096).contains(&s.len()) && !s.bytes().any(|c| c < 32 || c == 127) {
        Ok(())
    } else {
        Err(Invalid(
            "webhook header values must be 1 to 4096 bytes without control characters".into(),
        ))
    },
    Bounds {
        min: Some(1),
        max: 4096,
        pattern: None,
        write_only: true,
        description: "A single-line header value of at most 4096 bytes. Write-only.",
        example: None,
    }
);
