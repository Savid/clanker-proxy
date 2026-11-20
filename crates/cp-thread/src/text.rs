//! Bounded text: names, IDs, titles, bodies and labels. Each type can only
//! hold a valid value, so code past the boundary need not check again.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Longest participant name, in bytes (names are ASCII).
pub const MAX_NAME: usize = 39;
/// Longest title, in characters (Unicode code points).
pub const MAX_TITLE: usize = 200;
/// Longest body, in characters.
pub const MAX_BODY: usize = 64 * 1024;
/// Most labels on a thread.
pub const MAX_LABELS: usize = 10;
/// Longest label, in bytes (labels are ASCII).
pub const MAX_LABEL: usize = 40;

/// Why a piece of text was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    #[error(
        "{0:?}: a name is lower-case letters, digits and single inner hyphens, at most {MAX_NAME}"
    )]
    Name(String),
    #[error("{0:?}: want a lower-case UUID")]
    Id(String),
    #[error("title must be one trimmed line of 1 to {MAX_TITLE} characters")]
    Title,
    #[error(
        "body: want at most {MAX_BODY} characters, without control characters but tab and line breaks"
    )]
    Body,
    #[error(
        "label {0:?}: want lower-case [a-z0-9._/-], starting with a letter or digit, at most {MAX_LABEL}"
    )]
    Label(String),
    #[error("at most {MAX_LABELS} labels")]
    TooManyLabels,
    #[error("labels must be sorted and unique")]
    LabelOrder,
}

/// Reports a character that changes how the text around it reads without
/// showing itself: a line or paragraph separator, or a bidirectional
/// control, which can break or reorder a line so a peer's words pass for
/// something else.
pub fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{2028}' | '\u{2029}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// A character a body may not hold: a control character other than tab and
/// line breaks, which can rewrite a reader's terminal, or a hidden one.
fn forbidden_in_body(c: char) -> bool {
    (c.is_control() && !matches!(c, '\t' | '\n' | '\r')) || is_hidden(c)
}

macro_rules! string_type {
    ($(#[$doc:meta])* $name:ident, $check:expr) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// The text, already valid.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = TextError;

            fn try_from(s: String) -> Result<Self, TextError> {
                let check: fn(String) -> Result<String, TextError> = $check;
                check(s).map(Self)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = TextError;

            fn try_from(s: &str) -> Result<Self, TextError> {
                Self::try_from(s.to_string())
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    };
}

fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    let alnum = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty()
        && b.len() <= MAX_NAME
        && b.first().is_some_and(alnum)
        && b.last().is_some_and(alnum)
        && b.iter().all(|c| alnum(c) || *c == b'-')
        && !s.contains("--")
}

string_type!(
    /// A participant's name: lower-case letters and digits with single inner
    /// hyphens, as the owner and their peers are known.
    Name,
    |s| if valid_name(&s) {
        Ok(s)
    } else {
        Err(TextError::Name(s))
    }
);

impl Name {
    /// Lower-cases and trims `s`, then checks it.
    pub fn normalize(s: &str) -> Result<Self, TextError> {
        Self::try_from(s.trim().to_lowercase())
    }
}

fn valid_id(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}

string_type!(
    /// A lower-case UUID: the form of event and thread IDs.
    Id,
    |s| if valid_id(&s) {
        Ok(s)
    } else {
        Err(TextError::Id(s))
    }
);

impl Id {
    /// A version 4 UUID from 16 random bytes the caller supplies, since this
    /// crate never reads randomness itself.
    pub fn from_random(mut bytes: [u8; 16]) -> Self {
        use fmt::Write as _;

        if let Some(b) = bytes.get_mut(6) {
            *b = (*b & 0x0f) | 0x40;
        }
        if let Some(b) = bytes.get_mut(8) {
            *b = (*b & 0x3f) | 0x80;
        }
        let mut s = String::with_capacity(36);
        for (i, b) in bytes.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                s.push('-');
            }
            let _ = write!(s, "{b:02x}");
        }
        Self(s)
    }
}

fn valid_title(s: &str) -> bool {
    !s.is_empty()
        && s.trim() == s
        && s.chars().count() <= MAX_TITLE
        && !s.chars().any(|c| c.is_control() || is_hidden(c))
}

string_type!(
    /// A thread's title: one trimmed line.
    Title,
    |s| if valid_title(&s) {
        Ok(s)
    } else {
        Err(TextError::Title)
    }
);

fn valid_body(s: &str) -> bool {
    s.chars().count() <= MAX_BODY && !s.chars().any(forbidden_in_body)
}

string_type!(
    /// The text an event carries. May be empty.
    #[derive(Default)]
    Body,
    |s| if valid_body(&s) {
        Ok(s)
    } else {
        Err(TextError::Body)
    }
);

impl Body {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the body has no text but whitespace.
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }

    /// The body's size in bytes, which a thread's room is measured in.
    pub fn bytes(&self) -> u64 {
        u64::try_from(self.0.len()).unwrap_or(u64::MAX)
    }
}

fn valid_label(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() <= MAX_LABEL
        && b.first()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'/' | b'-')
        })
}

string_type!(
    /// A thread label.
    Label,
    |s| if valid_label(&s) {
        Ok(s)
    } else {
        Err(TextError::Label(s))
    }
);

/// A thread's labels: at most [`MAX_LABELS`], sorted and unique.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<Label>", into = "Vec<Label>")]
pub struct Labels(Vec<Label>);

impl Labels {
    /// Sorts `labels`, refusing duplicates or too many.
    pub fn sorted(mut labels: Vec<Label>) -> Result<Self, TextError> {
        labels.sort();
        Self::try_from(labels)
    }

    pub fn as_slice(&self) -> &[Label] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<Vec<Label>> for Labels {
    type Error = TextError;

    fn try_from(labels: Vec<Label>) -> Result<Self, TextError> {
        if labels.len() > MAX_LABELS {
            return Err(TextError::TooManyLabels);
        }
        if labels.windows(2).any(|w| matches!(w, [a, b] if a >= b)) {
            return Err(TextError::LabelOrder);
        }
        Ok(Self(labels))
    }
}

impl From<Labels> for Vec<Label> {
    fn from(l: Labels) -> Self {
        l.0
    }
}
