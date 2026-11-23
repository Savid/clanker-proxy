//! The copy `cpd` keeps of its data before migrating it.
#![allow(clippy::unwrap_used)]

use cp_core::Sql as _;
use cp_sqlite::Sqlite;

#[test]
fn a_copy_holds_the_data_privately_and_never_overwrites() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cp-sqlite-copy-{nanos}"));
    std::fs::create_dir(&dir).unwrap();
    let sql = Sqlite::open(&dir.join("cp.db")).unwrap();
    sql.execute("CREATE TABLE t (v TEXT NOT NULL) STRICT", &[])
        .unwrap();
    sql.execute("INSERT INTO t (v) VALUES ('kept')", &[])
        .unwrap();

    let copy = dir.join("cp.db.schema-7");
    sql.copy_to(&copy).unwrap();
    let row = Sqlite::open(&copy)
        .unwrap()
        .query_one("SELECT v FROM t", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row.text(0).unwrap(), "kept");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&copy).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(sql.copy_to(&copy).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
