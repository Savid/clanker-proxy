//! clanker-proxy's daemon on Cloudflare: a Worker that routes every API
//! request to the single Durable Object holding the owner's state.

mod sql;

use std::time::Duration;

use futures_util::{StreamExt, stream};
use worker::{
    Context, Date, Delay, DurableObject, Env, Headers, Request, Response, Result, ScheduleContext,
    ScheduledEvent, State, durable_object, event,
};

/// Bumped on every schema change; a stored version that differs is refused,
/// since there are no migrations.
const SCHEMA_VERSION: i64 = 1;

/// The internal path the cron heartbeat uses. The Worker never forwards
/// outside requests to it.
const HEARTBEAT: &str = "/internal/heartbeat";

/// How often an open stream is pinged, below common proxy idle timeouts.
const PING_EVERY: Duration = Duration::from_secs(15);

/// The alarm the heartbeat keeps armed, so a missed or aborted alarm is
/// replaced within one cron period.
const ALARM_HORIZON: Duration = Duration::from_hours(1);

fn daemon(env: &Env) -> Result<worker::Stub> {
    env.durable_object("DAEMON")?
        .id_from_name("owner")?
        .get_stub()
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    if !req.path().starts_with("/api/v1/") {
        return Response::error("not found", 404);
    }
    daemon(&env)?.fetch_with_request(req).await
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let beat = async {
        daemon(&env)?
            .fetch_with_str(&format!("https://daemon{HEARTBEAT}"))
            .await
    };
    if let Err(e) = beat.await {
        worker::console_error!("heartbeat: {e}");
    }
}

/// Now as RFC 3339 UTC to the second, the API's time format.
fn now_rfc3339() -> String {
    let iso = String::from(js_sys::Date::new_0().to_iso_string());
    iso.get(..19).map_or(iso.clone(), |s| format!("{s}Z"))
}

#[derive(serde::Deserialize)]
struct MetaValue {
    value: String,
}

#[durable_object]
pub struct Daemon {
    state: State,
    /// Whether the schema could be created and matches this build.
    schema: std::result::Result<(), String>,
}

impl Daemon {
    fn migrate(state: &State) -> std::result::Result<(), String> {
        let storage = state.storage();
        let db = storage.sql();
        sql::transaction(&storage, move || {
            db.exec(
                "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT",
                None,
            )
            .map_err(|e| e.to_string())?;
            db.exec(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?) ON CONFLICT DO NOTHING",
                vec![SCHEMA_VERSION.to_string().into()],
            )
            .map_err(|e| e.to_string())?;
            let stored: MetaValue = db
                .exec("SELECT value FROM meta WHERE key = 'schema_version'", None)
                .and_then(|c| c.one())
                .map_err(|e| e.to_string())?;
            if stored.value == SCHEMA_VERSION.to_string() {
                Ok(())
            } else {
                Err(format!(
                    "stored schema version {} is not {SCHEMA_VERSION}; rotate the Durable Object class",
                    stored.value
                ))
            }
        })
    }

    async fn ensure_alarm(&self) -> Result<()> {
        let storage = self.state.storage();
        if storage.get_alarm().await?.is_none() {
            storage.set_alarm(ALARM_HORIZON).await?;
        }
        Ok(())
    }

    fn stream() -> Result<Response> {
        let pings = stream::unfold((), |()| async {
            Delay::from(PING_EVERY).await;
            Some((": ping\n\n".to_string(), ()))
        });
        let body = stream::once(async { "retry: 2000\n\n".to_string() })
            .chain(pings)
            .map(|frame| Ok::<Vec<u8>, worker::Error>(frame.into_bytes()));
        let headers = Headers::new();
        headers.set("content-type", "text/event-stream")?;
        headers.set("cache-control", "no-cache, no-transform")?;
        Ok(Response::from_stream(body)?.with_headers(headers))
    }
}

impl DurableObject for Daemon {
    fn new(state: State, _env: Env) -> Self {
        let schema = Self::migrate(&state);
        if let Err(e) = &schema {
            worker::console_error!("schema: {e}");
        }
        Self { state, schema }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        if self.schema.is_err() {
            return Response::error("daemon storage is unusable; see the Worker logs", 503);
        }
        match req.path().as_str() {
            HEARTBEAT => {
                self.ensure_alarm().await?;
                Response::empty()
            }
            "/api/v1/health" => Response::from_json(&serde_json::json!({
                "status": "ok",
                "at": now_rfc3339(),
            })),
            "/api/v1/stream" => Self::stream(),
            _ => Response::error("not found", 404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        worker::console_log!("alarm at {}", Date::now().as_millis());
        self.ensure_alarm().await?;
        Response::empty()
    }
}
