//! The schema both hosts use, verbatim. Times are Unix milliseconds unless
//! a column says seconds; an event's `at` is seconds, as on the wire.

/// The version `STATEMENTS` create. Before 1.0 a storage change bumps
/// this instead of adding a migration: earlier data is refused, and its
/// owner starts again from empty storage.
pub const BASELINE: u32 = 5;

/// Changes since the baseline, in order: entry `i` moves stored data from
/// version `BASELINE + i` to the next. Append only: deployed data has
/// already run every shipped entry, so editing one would split installs.
pub const MIGRATIONS: &[&[&str]] = &[];

/// The version this build stores data at.
#[allow(clippy::cast_possible_truncation)] // a handful of migrations
pub const SCHEMA_VERSION: u32 = BASELINE + MIGRATIONS.len() as u32;

/// Statements creating the baseline schema on empty storage. Frozen: later
/// changes go in `MIGRATIONS`. The first creates `meta`, which records the
/// version.
pub const STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) STRICT",
    "CREATE TABLE IF NOT EXISTS peers (
        name TEXT PRIMARY KEY,
        url TEXT NOT NULL UNIQUE,
        key TEXT NOT NULL UNIQUE,
        status TEXT NOT NULL CHECK (status IN ('requested', 'active')),
        added_at INTEGER NOT NULL,
        first_401_at INTEGER,
        version TEXT
    ) STRICT",
    "CREATE TABLE IF NOT EXISTS former_peers (
        name TEXT PRIMARY KEY,
        url TEXT NOT NULL
    ) STRICT",
    "CREATE TABLE IF NOT EXISTS requests (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        url TEXT NOT NULL,
        key TEXT NOT NULL,
        note TEXT,
        at INTEGER NOT NULL
    ) STRICT",
    "CREATE TABLE IF NOT EXISTS request_times (
        at INTEGER NOT NULL
    ) STRICT",
    // At most one approval confirms at a time; expires_at frees it if the
    // host stops midway.
    "CREATE TABLE IF NOT EXISTS peering_op (
        slot INTEGER PRIMARY KEY CHECK (slot = 1),
        request_id TEXT NOT NULL,
        name TEXT NOT NULL,
        url TEXT NOT NULL,
        key TEXT NOT NULL,
        previous_key TEXT,
        expires_at INTEGER NOT NULL
    ) STRICT",
    "CREATE TABLE IF NOT EXISTS events (
        id TEXT PRIMARY KEY,
        thread TEXT NOT NULL,
        author TEXT NOT NULL,
        recipient TEXT NOT NULL,
        at INTEGER NOT NULL,
        clock INTEGER NOT NULL,
        action TEXT NOT NULL,
        title TEXT,
        kind TEXT,
        labels TEXT,
        body TEXT NOT NULL,
        body_bytes INTEGER NOT NULL,
        body_blank INTEGER NOT NULL,
        ignored TEXT,
        stored_at INTEGER NOT NULL
    ) STRICT",
    "CREATE INDEX IF NOT EXISTS events_replay ON events (thread, clock, id)",
    "CREATE TABLE IF NOT EXISTS outbox (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT NOT NULL UNIQUE,
        thread TEXT NOT NULL,
        peer TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending', 'delivered', 'failed')),
        attempts INTEGER NOT NULL DEFAULT 0,
        next_attempt_at INTEGER,
        last_error TEXT,
        delivered_at INTEGER
    ) STRICT",
    "CREATE INDEX IF NOT EXISTS outbox_peer ON outbox (status, peer, seq)",
    // A thread's listing row and its folded state, so a new event that
    // replays last needs nothing else.
    "CREATE TABLE IF NOT EXISTS threads (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        kind TEXT NOT NULL,
        labels TEXT NOT NULL,
        sender TEXT NOT NULL,
        recipient TEXT NOT NULL,
        peer TEXT NOT NULL,
        state TEXT NOT NULL,
        turn TEXT,
        opened_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        events INTEGER NOT NULL,
        last_from TEXT NOT NULL,
        undelivered INTEGER NOT NULL DEFAULT 0,
        failed INTEGER NOT NULL DEFAULT 0,
        overruled INTEGER NOT NULL DEFAULT 0,
        last_clock INTEGER NOT NULL,
        last_id TEXT NOT NULL,
        max_clock INTEGER NOT NULL,
        sender_events INTEGER NOT NULL,
        sender_bytes INTEGER NOT NULL,
        sender_clock INTEGER NOT NULL,
        recipient_events INTEGER NOT NULL,
        recipient_bytes INTEGER NOT NULL,
        recipient_clock INTEGER NOT NULL
    ) STRICT",
    "CREATE INDEX IF NOT EXISTS threads_updated ON threads (updated_at DESC, id)",
    "CREATE TABLE IF NOT EXISTS webhooks (
        name TEXT PRIMARY KEY,
        type TEXT NOT NULL,
        url TEXT NOT NULL,
        events TEXT NOT NULL,
        origin TEXT NOT NULL,
        peers TEXT NOT NULL,
        enabled INTEGER NOT NULL,
        secret TEXT NOT NULL,
        headers TEXT NOT NULL,
        retry_after INTEGER NOT NULL DEFAULT 0,
        failures INTEGER NOT NULL DEFAULT 0,
        agent TEXT
    ) STRICT",
    // sending marks an attempt in flight, so a newer notification for the
    // same thread does not replace it.
    "CREATE TABLE IF NOT EXISTS webhook_deliveries (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT NOT NULL UNIQUE,
        webhook TEXT NOT NULL,
        event TEXT NOT NULL,
        origin TEXT NOT NULL,
        subject TEXT NOT NULL,
        peer TEXT NOT NULL,
        payload TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending', 'delivered', 'failed')),
        attempts INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        next_attempt_at INTEGER NOT NULL,
        retry_until INTEGER NOT NULL,
        finished_at INTEGER,
        last_error TEXT NOT NULL DEFAULT '',
        sending INTEGER NOT NULL DEFAULT 0
    ) STRICT",
    "CREATE INDEX IF NOT EXISTS webhook_due ON webhook_deliveries (webhook, status, next_attempt_at, seq)",
    "CREATE TABLE IF NOT EXISTS agent_tokens (
        name TEXT PRIMARY KEY,
        hash TEXT NOT NULL UNIQUE,
        peers TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER,
        used_at INTEGER
    ) STRICT",
];
