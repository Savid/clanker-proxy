//! Storage across releases: data migrates forward, never backward.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::{Clock, Daemon, name, peer, runtime, url};
use cp_api::{ErrorKind, Version};
use cp_core::{BASE, Config, Core, MIGRATIONS, SCHEMA_VERSION, STATEMENTS, Sql, UPGRADES_FROM};
use cp_sqlite::Sqlite;
use sha2::{Digest as _, Sha256};

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

/// Storage at schema `BASE`, with an owner and a peer.
fn oldest() -> Sqlite {
    let sql = Sqlite::memory().unwrap();
    for s in STATEMENTS {
        sql.execute(s, &[]).unwrap();
    }
    for (k, v) in [
        ("schema_version", BASE.to_string()),
        ("name", "alice".into()),
    ] {
        sql.execute(
            "INSERT INTO meta (key, value) VALUES (?, ?)",
            &[k.into(), v.into()],
        )
        .unwrap();
    }
    sql.execute(
        "INSERT INTO peers (name, url, key, status, added_at) VALUES ('bob', 'https://bob.test', 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 'active', 1)",
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
        BASE + u32::try_from(MIGRATIONS.len()).unwrap()
    );
}

#[test]
fn older_data_migrates_and_keeps_everything() {
    let clock = Clock::new();
    let core = Core::open(oldest(), runtime(&clock, 1), config()).unwrap();
    assert_eq!(stored_version(core.sql()), SCHEMA_VERSION.to_string());
    let peers = core.peers(None).unwrap().peers;
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
fn data_from_a_newer_release_or_before_the_oldest_upgrade_is_refused() {
    let clock = Clock::new();
    for (stored, says) in [
        (SCHEMA_VERSION + 1, "newer release"),
        (UPGRADES_FROM - 1, "older than this release upgrades from"),
    ] {
        let sql = oldest();
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
fn the_stored_version_reads_without_changing_anything() {
    let empty = Sqlite::memory().unwrap();
    assert_eq!(cp_core::stored_version(&empty).unwrap(), None);
    assert!(
        empty
            .query_one("SELECT 1 FROM sqlite_master", &[])
            .unwrap()
            .is_none()
    );
    assert_eq!(cp_core::stored_version(&oldest()).unwrap(), Some(BASE));
}

/// Shipped storage never changes: deployed data has run it, so an edit
/// would split installs. A storage change appends a migration, and pins it
/// here in the same change.
#[test]
fn shipped_storage_is_frozen() {
    fn hash(statements: &[&str]) -> String {
        use std::fmt::Write as _;
        Sha256::digest(statements.join("\0"))
            .iter()
            .fold(String::new(), |mut hex, b| {
                let _ = write!(hex, "{b:02x}");
                hex
            })
    }
    assert_eq!(
        BASE, 7,
        "deployed data is stamped with BASE, and MIGRATIONS count from it"
    );
    assert_eq!(
        hash(STATEMENTS),
        "3e465fb7e21654a179c9b6665632dcecd1eec289f5dfc50bf8472185561c911b",
        "STATEMENTS create schema {BASE}, which deployed data has: put the change in a new migration"
    );
    let pinned: [&str; 0] = [];
    assert_eq!(
        MIGRATIONS.len(),
        pinned.len(),
        "pin each new migration's hash here"
    );
    for (i, (m, want)) in MIGRATIONS.iter().zip(pinned).enumerate() {
        assert_eq!(
            hash(m),
            want,
            "migration {i} has shipped: put the change in a new migration"
        );
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
        alice.call(|c| c.peer(&name("bob"), None)).unwrap().version,
        Some(v.clone())
    );
    assert!(v.newer_than(&Version::try_from("v0.2.1".to_string()).unwrap()));
    assert!(!v.newer_than(&Version::try_from("v0.10.0".to_string()).unwrap()));
    assert!(!v.newer_than(&Version::try_from("vtest".to_string()).unwrap()));
}
