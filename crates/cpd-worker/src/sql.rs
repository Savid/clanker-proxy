//! SQLite inside the Durable Object: transactions, and the core's `Sql`.

use std::cell::RefCell;
use std::rc::Rc;

use cp_core::{Row, Sql, SqlError, Value};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use worker::{SqlStorage, SqlStorageValue, Storage};

/// Why a transaction did not commit.
pub(crate) enum TxError<E> {
    /// `f` failed; everything it wrote was rolled back.
    Failed(E),
    /// Storage itself failed.
    Storage(String),
}

/// Runs `f` inside `ctx.storage.transactionSync`, which workers-rs does not
/// bind. An `Err` from `f` is thrown into the runtime so every write `f`
/// made is rolled back; `exec` cannot run `BEGIN` itself.
pub(crate) fn transaction<T: 'static, E: 'static>(
    storage: &Storage,
    f: impl FnOnce() -> Result<T, E> + 'static,
) -> Result<T, TxError<E>> {
    let raw: &JsValue = storage.as_raw().as_ref();
    let func: js_sys::Function = js_sys::Reflect::get(raw, &JsValue::from_str("transactionSync"))
        .map_err(|e| TxError::Storage(format!("storage.transactionSync: {e:?}")))?
        .dyn_into()
        .map_err(|_| TxError::Storage("storage.transactionSync is not a function".into()))?;

    let out: Rc<RefCell<Option<Result<T, E>>>> = Rc::default();
    let slot = Rc::clone(&out);
    let callback = Closure::once(move || -> Result<JsValue, JsValue> {
        let result = f();
        let failed = result.is_err();
        *slot.borrow_mut() = Some(result);
        if failed {
            Err(JsValue::from_str("rolled back"))
        } else {
            Ok(JsValue::UNDEFINED)
        }
    });

    let call = func.call1(raw, callback.as_ref().unchecked_ref());
    let result = out.borrow_mut().take();
    match (call, result) {
        (Ok(_), Some(Ok(value))) => Ok(value),
        (_, Some(Err(e))) => Err(TxError::Failed(e)),
        (Err(e), None | Some(Ok(_))) => Err(TxError::Storage(format!("transaction failed: {e:?}"))),
        (Ok(_), None) => Err(TxError::Storage("transaction callback did not run".into())),
    }
}

/// The Durable Object's SQLite, for the core.
pub(crate) struct DoSql(pub(crate) SqlStorage);

fn storage_error(e: &worker::Error) -> SqlError {
    SqlError(e.to_string())
}

fn bind(params: &[Value]) -> Vec<SqlStorageValue> {
    params
        .iter()
        .map(|v| match v {
            Value::Null => SqlStorageValue::Null,
            Value::Integer(i) => SqlStorageValue::Integer(*i),
            Value::Text(s) => SqlStorageValue::String(s.clone()),
        })
        .collect()
}

/// Integers cross into JavaScript as numbers; one past 2^53 comes back as a
/// float. The core stores only integers that fit (times, counts, sequence
/// numbers), so a fractional or out-of-range value is corrupt data.
fn read(v: SqlStorageValue) -> Result<Value, SqlError> {
    const SAFE: f64 = 9_007_199_254_740_991.0;
    match v {
        SqlStorageValue::Null => Ok(Value::Null),
        SqlStorageValue::Integer(i) => Ok(Value::Integer(i)),
        SqlStorageValue::Boolean(b) => Ok(Value::Integer(i64::from(b))),
        SqlStorageValue::String(s) => Ok(Value::Text(s)),
        SqlStorageValue::Float(f) if f.fract() == 0.0 && f.abs() <= SAFE => {
            #[allow(clippy::cast_possible_truncation)] // whole and within ±2^53
            Ok(Value::Integer(f as i64))
        }
        SqlStorageValue::Float(_) | SqlStorageValue::Blob(_) => {
            Err(SqlError("stored value is not an integer or text".into()))
        }
    }
}

impl Sql for DoSql {
    fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, SqlError> {
        let cursor = self
            .0
            .exec(sql, bind(params))
            .map_err(|e| storage_error(&e))?;
        for row in cursor.raw() {
            row.map_err(|e| storage_error(&e))?;
        }
        // rowsWritten counts index writes too; changes() counts rows.
        let changes = self
            .0
            .exec("SELECT changes()", None)
            .map_err(|e| storage_error(&e))?;
        let n = changes
            .raw()
            .next()
            .transpose()
            .map_err(|e| storage_error(&e))?
            .and_then(|row| row.into_iter().next())
            .map(read)
            .transpose()?;
        match n {
            Some(Value::Integer(n)) => Ok(u64::try_from(n).unwrap_or(0)),
            _ => Err(SqlError("changes() returned no count".into())),
        }
    }

    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, SqlError> {
        let cursor = self
            .0
            .exec(sql, bind(params))
            .map_err(|e| storage_error(&e))?;
        cursor
            .raw()
            .map(|row| {
                let values = row.map_err(|e| storage_error(&e))?;
                values
                    .into_iter()
                    .map(read)
                    .collect::<Result<_, _>>()
                    .map(Row)
            })
            .collect()
    }
}
