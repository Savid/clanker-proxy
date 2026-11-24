//! The copy `cpd` keeps of its data before migrating it.
#![allow(clippy::unwrap_used)]

use cp_core::Sql as _;
use cp_sqlite::Sqlite;

#[test]
fn a_copy_holds_the_data_privately_and_a_new_one_replaces_it() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cp-sqlite-copy-{nanos}"));
    std::fs::create_dir(&dir).unwrap();
    let sql = Sqlite::open(&dir.join("cp.db")).unwrap();
    sql.execute("CREATE TABLE t (v TEXT NOT NULL) STRICT", &[])
        .unwrap();
    sql.execute("INSERT INTO t (v) VALUES ('first')", &[])
        .unwrap();
    let copy = dir.join("cp.db.schema-7");
    let rows = || {
        Sqlite::open(&copy)
            .unwrap()
            .query_one("SELECT count(*) FROM t", &[])
            .unwrap()
            .unwrap()
            .int(0)
            .unwrap()
    };

    // An interrupted copy left these; they never stand in for a copy.
    std::fs::write(dir.join("cp.db.schema-7.tmp"), b"partial").unwrap();
    std::fs::write(dir.join("cp.db.schema-7.tmp-journal"), b"partial").unwrap();
    sql.copy_to(&copy).unwrap();
    assert_eq!(rows(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&copy).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    sql.execute("INSERT INTO t (v) VALUES ('second')", &[])
        .unwrap();
    sql.copy_to(&copy).unwrap();
    assert_eq!(rows(), 2);
    let mut left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("cp.db.schema-7"))
        .collect();
    left.sort();
    assert_eq!(left, ["cp.db.schema-7"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A copy that fails leaves nothing behind to fill the disk.
#[cfg(unix)]
#[test]
fn a_failed_copy_leaves_nothing() {
    use std::os::unix::ffi::OsStrExt as _;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cp-sqlite-copy-fails-{nanos}"));
    std::fs::create_dir(&dir).unwrap();
    let sql = Sqlite::open(&dir.join("cp.db")).unwrap();
    // SQLite takes only UTF-8 paths.
    let copy = dir.join(std::ffi::OsStr::from_bytes(b"cp.db.schema-\xff"));
    assert!(sql.copy_to(&copy).is_err());
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.as_bytes().starts_with(b"cp.db.schema"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}
