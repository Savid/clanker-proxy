//! SQLite access inside the Durable Object.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use worker::Storage;

/// Runs `f` inside `ctx.storage.transactionSync`, which workers-rs does not
/// bind. An `Err` from `f` is thrown into the runtime, so every write `f`
/// made is rolled back; `exec` cannot run `BEGIN` itself.
pub(crate) fn transaction<T: 'static>(
    storage: &Storage,
    f: impl FnOnce() -> Result<T, String> + 'static,
) -> Result<T, String> {
    let raw: &JsValue = storage.as_raw().as_ref();
    let func: js_sys::Function = js_sys::Reflect::get(raw, &JsValue::from_str("transactionSync"))
        .map_err(|e| format!("storage.transactionSync: {e:?}"))?
        .dyn_into()
        .map_err(|_| "storage.transactionSync is not a function".to_string())?;

    let out: Rc<RefCell<Option<Result<T, String>>>> = Rc::default();
    let slot = Rc::clone(&out);
    let callback = Closure::once(move || -> Result<JsValue, JsValue> {
        let result = f();
        let thrown = result.as_ref().err().map(|e| JsValue::from_str(e));
        *slot.borrow_mut() = Some(result);
        thrown.map_or(Ok(JsValue::UNDEFINED), Err)
    });

    let call = func.call1(raw, callback.as_ref().unchecked_ref());
    let result = out.borrow_mut().take();
    match (call, result) {
        (Ok(_), Some(Ok(value))) => Ok(value),
        (_, Some(Err(e))) => Err(e),
        (Err(e), None | Some(Ok(_))) => Err(format!("transaction failed: {e:?}")),
        (Ok(_), None) => Err("transaction callback did not run".to_string()),
    }
}
