//! cp-core's [`Sql`] on rusqlite: the native daemon's storage, and the
//! storage tests run on.

use std::path::Path;

use cp_api::Error;
use cp_core::{Core, Row, Runtime, Sql, SqlError, Value};
use rusqlite::types::{Value as SqliteValue, ValueRef};
use rusqlite::{Connection, params_from_iter};

/// A SQLite database.
pub struct Sqlite {
    conn: Connection,
}

fn sql_err(e: &rusqlite::Error) -> SqlError {
    SqlError(e.to_string())
}

impl Sqlite {
    /// Opens (creating) the database at `path`, readable only by its owner,
    /// since it holds peer secrets.
    pub fn open(path: &Path) -> Result<Self, SqlError> {
        if !path.exists() {
            create_private(path)
                .map_err(|e| SqlError(format!("create {}: {e}", path.display())))?;
        }
        let conn = Connection::open(path).map_err(|e| sql_err(&e))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000; PRAGMA foreign_keys = ON;",
        )
        .map_err(|e| sql_err(&e))?;
        Ok(Self { conn })
    }

    /// A database in memory, for tests.
    pub fn memory() -> Result<Self, SqlError> {
        Ok(Self {
            conn: Connection::open_in_memory().map_err(|e| sql_err(&e))?,
        })
    }

    /// Starts a transaction that takes the write lock at once, so a core
    /// call never interleaves with another writer.
    pub fn begin(&self) -> Result<(), SqlError> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| sql_err(&e))
    }

    pub fn commit(&self) -> Result<(), SqlError> {
        self.conn.execute_batch("COMMIT").map_err(|e| sql_err(&e))
    }

    pub fn rollback(&self) -> Result<(), SqlError> {
        self.conn.execute_batch("ROLLBACK").map_err(|e| sql_err(&e))
    }
}

/// Runs one core call in its own transaction: an error rolls back its
/// writes and drops its effects.
pub fn run<R: Runtime, T>(
    core: &mut Core<Sqlite, R>,
    f: impl FnOnce(&mut Core<Sqlite, R>) -> Result<T, Error>,
) -> Result<T, Error> {
    core.sql().begin()?;
    match f(core) {
        Ok(v) => {
            core.sql().commit()?;
            Ok(v)
        }
        Err(e) => {
            let _ = core.sql().rollback();
            core.discard_effects();
            Err(e)
        }
    }
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(drop)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
}

fn to_sqlite(v: &Value) -> SqliteValue {
    match v {
        Value::Null => SqliteValue::Null,
        Value::Integer(i) => SqliteValue::Integer(*i),
        Value::Text(s) => SqliteValue::Text(s.clone()),
    }
}

impl Sql for Sqlite {
    fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, SqlError> {
        let mut stmt = self.conn.prepare_cached(sql).map_err(|e| sql_err(&e))?;
        let n = stmt
            .execute(params_from_iter(params.iter().map(to_sqlite)))
            .map_err(|e| sql_err(&e))?;
        Ok(u64::try_from(n).unwrap_or(u64::MAX))
    }

    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, SqlError> {
        let mut stmt = self.conn.prepare_cached(sql).map_err(|e| sql_err(&e))?;
        let columns = stmt.column_count();
        let mut rows = stmt
            .query(params_from_iter(params.iter().map(to_sqlite)))
            .map_err(|e| sql_err(&e))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| sql_err(&e))? {
            let mut values = Vec::with_capacity(columns);
            for i in 0..columns {
                values.push(match row.get_ref(i).map_err(|e| sql_err(&e))? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => Value::Integer(n),
                    // The schema stores no floats or blobs; read them as text.
                    ValueRef::Real(f) => Value::Text(f.to_string()),
                    ValueRef::Text(t) | ValueRef::Blob(t) => {
                        Value::Text(String::from_utf8_lossy(t).into_owned())
                    }
                });
            }
            out.push(Row(values));
        }
        Ok(out)
    }
}
