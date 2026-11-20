//! Checks that need no state, for a host to run before it wakes the
//! daemon: on Cloudflare, the Worker runs them so unroutable, unauthenticated
//! or oversized requests never reach the Durable Object. The daemon still
//! checks everything itself.

use bytes::Bytes;
use cp_api::{Access, Error, ErrorKind, MAX_REQUEST_BODY, Op, prefix};

use crate::route::{Route, Router};
use crate::service::{bearer_token, problem, unauthorized, wrong_method};

/// The operation table, ready to check requests against.
pub struct Gate {
    router: Router,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    pub fn new() -> Self {
        Self {
            router: Router::new(),
        }
    }

    /// The operation a request names, or the answer that refuses it: no such
    /// route, no credential of the kind the operation takes (a bearer token,
    /// or for calls between daemons a signature), or a declared body over
    /// the limit. `authorization` is the raw header value.
    pub fn check(
        &self,
        method: &http::Method,
        path: &str,
        authorization: Option<&str>,
        signed: bool,
        content_length: Option<u64>,
    ) -> Result<Op, Box<http::Response<Bytes>>> {
        let op = match self.router.route(method, path) {
            Route::Found(op, _) => op,
            Route::WrongMethod(methods) => return Err(Box::new(wrong_method(&methods))),
            Route::NotFound => {
                return Err(Box::new(problem(&Error::new(
                    ErrorKind::NotFound,
                    "no such API route",
                ))));
            }
        };
        let token = authorization.and_then(bearer_token).unwrap_or_default();
        // An agent token on an owner operation passes: the daemon answers
        // 403, which says more than 401 would.
        let fits = match op.access() {
            Access::Public => true,
            Access::Agent => token.starts_with(prefix::AGENT),
            Access::Owner | Access::OwnerOrAgent => {
                token.starts_with(prefix::OWNER) || token.starts_with(prefix::AGENT)
            }
            Access::Peer => signed,
        };
        if !fits {
            return Err(Box::new(problem(&unauthorized())));
        }
        if content_length.is_some_and(|n| n > MAX_REQUEST_BODY as u64) {
            return Err(Box::new(problem(&Error::new(
                ErrorKind::PayloadTooLarge,
                format!("body is over {MAX_REQUEST_BODY} bytes"),
            ))));
        }
        Ok(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(r: Result<Op, Box<http::Response<Bytes>>>) -> u16 {
        r.map_or_else(|e| e.status().as_u16(), |_| 200)
    }

    #[test]
    fn refuses_what_the_daemon_would() {
        let g = Gate::new();
        let get = http::Method::GET;
        let owner = Some("Bearer cpo_x");
        assert_eq!(
            status(g.check(&get, "/api/v1/health", None, false, None)),
            200
        );
        assert_eq!(
            status(g.check(&get, "/api/v1/nope", owner, false, None)),
            404
        );
        assert_eq!(
            status(g.check(&http::Method::PUT, "/api/v1/threads", owner, false, None)),
            405
        );
        assert_eq!(
            status(g.check(&get, "/api/v1/threads", None, false, None)),
            401
        );
        assert_eq!(
            status(g.check(&get, "/api/v1/threads", Some("Bearer cpp_x"), false, None)),
            401
        );
        assert_eq!(
            status(g.check(&get, "/api/v1/threads", owner, false, None)),
            200
        );
        assert_eq!(
            status(g.check(&get, "/api/v1/threads", Some("bearer cpa_x"), false, None)),
            200
        );
        assert_eq!(
            status(g.check(&get, "/api/v1/peers", Some("Bearer cpa_x"), false, None)),
            200
        );
        let post = http::Method::POST;
        assert_eq!(
            status(g.check(&post, "/api/v1/threads", owner, false, Some(2 << 20))),
            413
        );
        assert!(matches!(
            g.check(&post, "/api/v1/peering-requests", None, false, Some(10)),
            Ok(Op::RequestPeering)
        ));
        // Calls between daemons need a signature, and a bearer will not do.
        let events = "/api/v1/federation/events";
        assert_eq!(
            status(g.check(&post, events, Some("Bearer cpo_x"), false, None)),
            401
        );
        assert_eq!(status(g.check(&post, events, None, true, None)), 200);
    }
}
