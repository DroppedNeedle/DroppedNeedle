//! The check value that ties a database to its data key.
//!
//! A database stores [`Crypto::check_value`] of its key as the settings row
//! `key_check`, never the key. A start compares it with the key file, so a
//! key from another instance or another backup is refused before anything
//! is sealed under it. A database from before the check existed has none:
//! its key is accepted when a sealed value it holds opens under the key, or
//! when it holds none, and the check is then recorded.
//!
//! The row is in neither the import's replace list nor the keys the
//! one-time `config.json` copy takes, so an import keeps it and a file can
//! never bring one in.

use rusqlite::Connection;
use serde_json::Value;

use crate::db::{Db, DbError, DurableLane};

use super::crypto::{CIPHER_PREFIX, Crypto, CryptoError};
use super::error::ConfigError;
use super::sql;
use super::store::{decode_text, encode_text};
use crate::clock::Clock;

/// The settings row that holds the check value.
pub const KEY_CHECK_KEY: &str = "key_check";

/// What a database says about a key, read without writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assessment {
    /// The stored check is this key's.
    Matches,
    /// No check is stored. Of the sealed values found, `opened` open under
    /// the key and `unopened` do not; at least one opens, or there are none.
    Unchecked {
        /// Sealed values that open under the key.
        opened: usize,
        /// Sealed values that do not.
        unopened: usize,
    },
    /// The stored check is another key's, or sealed values exist and none
    /// of them opens under this key.
    Mismatch,
}

/// What [`enforce`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCheck {
    /// The stored check is this key's.
    Matches,
    /// No check was stored; the key was accepted and its check recorded.
    Recorded,
    /// The key does not belong to this database. Nothing was written.
    Mismatch,
}

/// Compare the key with the database, writing nothing. A value that is not
/// ciphertext at all (`v3:` text that does not decode) proves nothing
/// either way and is skipped.
pub fn assess(conn: &Connection, crypto: &Crypto) -> Result<Assessment, ConfigError> {
    if let Some(stored) = stored_check(conn)? {
        return Ok(if stored == crypto.check_value() {
            Assessment::Matches
        } else {
            Assessment::Mismatch
        });
    }
    let mut statement = conn
        .prepare(sql::SEALED_VALUES.text())
        .map_err(DbError::from)?;
    let values = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(DbError::from)?;
    let (mut opened, mut unopened) = (0, 0);
    for value in values {
        match crypto.decrypt(&value.map_err(DbError::from)?) {
            Ok(_) => opened += 1,
            Err(CryptoError::UnknownFormat) => {}
            Err(_) => unopened += 1,
        }
    }
    Ok(if opened == 0 && unopened > 0 {
        Assessment::Mismatch
    } else {
        Assessment::Unchecked { opened, unopened }
    })
}

/// Check the key against the database, and record its check value when
/// none is stored and the key is accepted. Run after the one-time
/// `config.json` copy, so the row never makes the settings look stored to
/// it. A normal start only reads.
pub fn enforce(db: &Db, crypto: &Crypto, clock: &dyn Clock) -> Result<KeyCheck, ConfigError> {
    let assessment = db.read_blocking("config.key_check", |conn| assess(conn, crypto))?;
    let Assessment::Unchecked { opened, unopened } = assessment else {
        return Ok(match assessment {
            Assessment::Matches => KeyCheck::Matches,
            _ => KeyCheck::Mismatch,
        });
    };
    if unopened > 0 {
        tracing::warn!(
            unopened,
            "some stored secrets do not open under the data key; they read as unset"
        );
    }
    let now = clock.now_ms();
    db.write_durable(DurableLane::Background, "config.key_check_record", |tx| {
        if stored_check(tx)?.is_none() {
            record(tx, crypto, now)?;
        }
        Ok::<_, ConfigError>(())
    })?;
    tracing::info!(
        verified_by_a_stored_secret = opened > 0,
        "recorded the data key's check value in the database"
    );
    Ok(KeyCheck::Recorded)
}

/// Store the check value of `crypto` at `now` (unix milliseconds),
/// replacing any earlier one.
pub fn record(conn: &Connection, crypto: &Crypto, now: i64) -> Result<(), ConfigError> {
    let text = encode_text(&Value::String(crypto.check_value()))?;
    conn.query_row(
        sql::PUT_SECTION.text(),
        rusqlite::params![KEY_CHECK_KEY, text, now],
        |row| row.get::<_, i64>(0),
    )
    .map_err(DbError::from)?;
    Ok(())
}

/// The stored check value, if any. A row that is not a string counts as
/// another key's.
pub fn stored_check(conn: &Connection) -> Result<Option<String>, ConfigError> {
    let text = conn
        .query_row(sql::ONE_SECTION.text(), [KEY_CHECK_KEY], |row| {
            row.get::<_, String>(0)
        })
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(DbError::from)?;
    match text {
        Some(text) => Ok(Some(match decode_text(&text)? {
            Value::String(check) => check,
            _ => String::new(),
        })),
        None => Ok(None),
    }
}

/// Empty every sealed string in every settings row, whatever the section
/// or a plugin's manifest says, each changed row at its next revision.
/// Answers the path of each value cleared (`section.field`, never a value).
/// `now` (unix milliseconds) stamps each changed row.
pub fn forget_sealed_settings(conn: &Connection, now: i64) -> Result<Vec<String>, ConfigError> {
    let rows = {
        let mut statement = conn
            .prepare(sql::ALL_SECTIONS.text())
            .map_err(DbError::from)?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(DbError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DbError::from)?
    };
    let mut cleared = Vec::new();
    for (key, text) in rows {
        let mut value = decode_text(&text)?;
        let before = cleared.len();
        clear_sealed(&mut value, &key, &mut cleared);
        if cleared.len() > before {
            conn.query_row(
                sql::PUT_SECTION.text(),
                rusqlite::params![key, encode_text(&value)?, now],
                |row| row.get::<_, i64>(0),
            )
            .map_err(DbError::from)?;
        }
    }
    cleared.sort();
    Ok(cleared)
}

fn clear_sealed(value: &mut Value, path: &str, cleared: &mut Vec<String>) {
    match value {
        Value::String(text) if text.starts_with(CIPHER_PREFIX) => {
            text.clear();
            cleared.push(path.to_owned());
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                clear_sealed(item, &format!("{path}[{index}]"), cleared);
            }
        }
        Value::Object(fields) => {
            for (name, field) in fields.iter_mut() {
                clear_sealed(field, &format!("{path}.{name}"), cleared);
            }
        }
        _ => {}
    }
}
