//! A point in time as a table stores it: integer unix milliseconds.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

/// Unix milliseconds read from or written to an `_at` column. Reading a REAL
/// value is a decode error, so a column still holding seconds as a float
/// fails loudly instead of reading as a time near 1970.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Millis(pub i64);

impl ToSql for Millis {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0))
    }
}

impl FromSql for Millis {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Integer(ms) => Ok(Self(ms)),
            _ => Err(FromSqlError::InvalidType),
        }
    }
}
