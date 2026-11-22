//! Routing from the operation table.

use std::collections::HashMap;

use cp_api::{Method, Op};

/// Path templates mapped to their operations by method.
pub(crate) struct Router {
    paths: matchit::Router<HashMap<Method, Op>>,
}

/// What a request's method and path name.
pub(crate) enum Route {
    Found(Op, Vec<(String, String)>),
    /// The path exists, under other methods.
    WrongMethod(Vec<Method>),
    NotFound,
}

impl Router {
    pub(crate) fn new() -> Self {
        let mut by_path: HashMap<&'static str, HashMap<Method, Op>> = HashMap::new();
        for op in Op::ALL {
            by_path
                .entry(op.path())
                .or_default()
                .insert(op.method(), *op);
        }
        let mut paths = matchit::Router::new();
        for (path, methods) in by_path {
            // The table's paths are distinct and well formed; a test holds it.
            let _ = paths.insert(path, methods);
        }
        Self { paths }
    }

    pub(crate) fn route(&self, method: &http::Method, path: &str) -> Route {
        let Ok(m) = self.paths.at(path) else {
            return Route::NotFound;
        };
        let method = match *method {
            http::Method::GET => Some(Method::Get),
            http::Method::POST => Some(Method::Post),
            http::Method::PUT => Some(Method::Put),
            http::Method::PATCH => Some(Method::Patch),
            http::Method::DELETE => Some(Method::Delete),
            _ => None,
        };
        match method.and_then(|x| m.value.get(&x)) {
            Some(op) => Route::Found(
                *op,
                m.params
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            None => Route::WrongMethod(m.value.keys().copied().collect()),
        }
    }
}
