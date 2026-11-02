//! The millisecond type refuses a REAL column.

use super::time::Millis;
use rusqlite::Connection;

fn read(sql: &str) -> rusqlite::Result<Millis> {
    Connection::open_in_memory()?.query_row(sql, [], |row| row.get(0))
}

#[test]
fn a_real_value_is_refused() {
    assert_eq!(
        read("SELECT 1700000000000").unwrap(),
        Millis(1_700_000_000_000)
    );
    assert!(read("SELECT 1700000000.5").is_err());
}
