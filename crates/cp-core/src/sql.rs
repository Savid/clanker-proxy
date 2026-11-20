//! The storage the core runs on: SQLite, as each host provides it. Hosts
//! wrap every core call in one transaction.

use cp_api::{Error, ErrorKind};

/// A value bound to, or read from, SQL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Integer(i64),
    Text(String),
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Integer(v)
    }
}

impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Value::Integer(i64::try_from(v).unwrap_or(i64::MAX))
    }
}

impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Value::Integer(i64::from(v))
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Integer(i64::from(v))
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

/// Text from anything that reads as a string, such as a name or an ID.
pub(crate) fn text(v: impl AsRef<str>) -> Value {
    Value::Text(v.as_ref().to_string())
}

/// One result row, by column position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row(pub Vec<Value>);

fn corrupt(i: usize, want: &str) -> Error {
    Error::new(
        ErrorKind::Internal,
        format!("stored column {i}: want {want}"),
    )
}

impl Row {
    fn get(&self, i: usize) -> &Value {
        self.0.get(i).unwrap_or(&Value::Null)
    }

    pub fn text(&self, i: usize) -> Result<&str, Error> {
        self.opt_text(i)?.ok_or_else(|| corrupt(i, "text"))
    }

    pub fn opt_text(&self, i: usize) -> Result<Option<&str>, Error> {
        match self.get(i) {
            Value::Text(s) => Ok(Some(s)),
            Value::Null => Ok(None),
            Value::Integer(_) => Err(corrupt(i, "text")),
        }
    }

    pub fn int(&self, i: usize) -> Result<i64, Error> {
        self.opt_int(i)?.ok_or_else(|| corrupt(i, "an integer"))
    }

    pub fn opt_int(&self, i: usize) -> Result<Option<i64>, Error> {
        match self.get(i) {
            Value::Integer(v) => Ok(Some(*v)),
            Value::Null => Ok(None),
            Value::Text(_) => Err(corrupt(i, "an integer")),
        }
    }

    pub fn uint(&self, i: usize) -> Result<u64, Error> {
        u64::try_from(self.int(i)?).map_err(|_| corrupt(i, "a count"))
    }

    pub fn count(&self, i: usize) -> Result<u32, Error> {
        u32::try_from(self.int(i)?).map_err(|_| corrupt(i, "a count"))
    }

    pub fn flag(&self, i: usize) -> Result<bool, Error> {
        Ok(self.int(i)? != 0)
    }

    /// A column parsed into a validated type.
    pub fn parse<T>(&self, i: usize) -> Result<T, Error>
    where
        T: TryFrom<String>,
    {
        T::try_from(self.text(i)?.to_string()).map_err(|_| corrupt(i, "a valid value"))
    }

    pub fn opt_parse<T>(&self, i: usize) -> Result<Option<T>, Error>
    where
        T: TryFrom<String>,
    {
        self.opt_text(i)?
            .map(|s| T::try_from(s.to_string()).map_err(|_| corrupt(i, "a valid value")))
            .transpose()
    }

    /// A column holding an enum's name.
    pub fn name<T: std::str::FromStr>(&self, i: usize) -> Result<T, Error> {
        self.text(i)?
            .parse()
            .map_err(|_| corrupt(i, "a known name"))
    }
}

/// A storage failure. Reported as an internal error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("storage: {0}")]
pub struct SqlError(pub String);

impl From<SqlError> for Error {
    fn from(e: SqlError) -> Self {
        Error::new(ErrorKind::Internal, e.to_string())
    }
}

/// SQLite, as a host provides it. Statements use `?` placeholders.
pub trait Sql {
    /// Runs one statement and returns how many rows it changed.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, SqlError>;

    /// Runs one query and returns its rows.
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, SqlError>;

    /// The query's first row, if any.
    fn query_one(&self, sql: &str, params: &[Value]) -> Result<Option<Row>, SqlError> {
        Ok(self.query(sql, params)?.into_iter().next())
    }
}
