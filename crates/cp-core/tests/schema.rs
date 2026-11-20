//! Storage across releases: data migrates forward, never backward.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::{Clock, Daemon, name, peer, runtime, url};
use cp_api::{ErrorKind, Version};
use cp_core::{BASELINE, Config, Core, MIGRATIONS, SCHEMA_VERSION, STATEMENTS, Sql};
use cp_sqlite::Sqlite;

fn config() -> Config {
    Config {
        name: Some(name("alice")),
        url: Some(url("https://alice.test")),
    }
}

fn stored_version(sql: &Sqlite) -> String {
    sql.query_one("SELECT value FROM meta WHERE key = 'schema_version'", &[])
        .unwrap()
        .unwrap()
        .text(0)
        .unwrap()
        .to_string()
}

/// Storage as the first release left it: the baseline schema, an owner and
/// a peer.
fn baseline() -> Sqlite {
    let sql = Sqlite::memory().unwrap();
    for s in STATEMENTS {
        sql.execute(s, &[]).unwrap();
    }
    for (k, v) in [
        ("schema_version", BASELINE.to_string()),
        ("name", "alice".into()),
    ] {
        sql.execute(
            "INSERT INTO meta (key, value) VALUES (?, ?)",
            &[k.into(), v.into()],
        )
        .unwrap();
    }
    sql.execute(
        "INSERT INTO peers (name, url, secret, secret_hash, status, added_at) VALUES ('bob', 'https://bob.test', 'cpp_x', 'h', 'active', 1)",
        &[],
    )
    .unwrap();
    sql
}

#[test]
fn empty_storage_starts_at_the_latest_version() {
    let clock = Clock::new();
    let core = Core::open(Sqlite::memory().unwrap(), runtime(&clock, 1), config()).unwrap();
    assert_eq!(stored_version(core.sql()), SCHEMA_VERSION.to_string());
    assert_eq!(
        SCHEMA_VERSION,
        BASELINE + u32::try_from(MIGRATIONS.len()).unwrap()
    );
}

#[test]
fn older_data_migrates_and_keeps_everything() {
    let clock = Clock::new();
    let core = Core::open(baseline(), runtime(&clock, 1), config()).unwrap();
    assert_eq!(stored_version(core.sql()), SCHEMA_VERSION.to_string());
    let peers = core.peers().unwrap().peers;
    assert_eq!(
        (
            peers.len(),
            peers[0].name.as_str(),
            peers[0].version.as_ref()
        ),
        (1, "bob", None)
    );
}

#[test]
fn data_from_a_newer_release_or_a_pre_release_build_is_refused() {
    let clock = Clock::new();
    for (stored, says) in [
        (SCHEMA_VERSION + 1, "newer release"),
        (BASELINE - 1, "before the first release"),
    ] {
        let sql = baseline();
        sql.execute(
            "UPDATE meta SET value = ? WHERE key = 'schema_version'",
            &[stored.to_string().into()],
        )
        .unwrap();
        let Err(e) = Core::open(sql, runtime(&clock, 1), config()) else {
            panic!("schema {stored} opened");
        };
        assert_eq!(e.kind, ErrorKind::Internal);
        assert!(e.detail.contains(says), "{}", e.detail);
    }
}

#[test]
fn peers_report_their_release() {
    let clock = Clock::new();
    let (mut alice, mut bob) = (
        Daemon::new("alice", &clock, 1),
        Daemon::new("bob", &clock, 2),
    );
    peer(&mut alice, &mut bob);
    let v = Version::try_from("v0.2.2".to_string()).unwrap();
    alice
        .call(|c| c.note_peer_version(&name("bob"), &v))
        .unwrap();
    assert_eq!(
        alice.call(|c| c.peer(&name("bob"))).unwrap().version,
        Some(v.clone())
    );
    assert!(v.newer_than(&Version::try_from("v0.2.1".to_string()).unwrap()));
    assert!(!v.newer_than(&Version::try_from("v0.10.0".to_string()).unwrap()));
    assert!(!v.newer_than(&Version::try_from("vtest".to_string()).unwrap()));
}
