//! The store behind each person's appearance.

use std::sync::Arc;

use rusqlite::{OptionalExtension as _, params};

use super::ThemeChoice;
use super::sql::{self, AppearanceRow, CssSaveRow};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, Lane};

/// How many saves of a person's CSS are kept.
pub const KEPT_SAVES: i64 = 10;

/// One person's stored appearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The theme they chose, if any.
    pub theme: Option<ThemeChoice>,
    /// Their CSS, empty when none.
    pub css: String,
    /// Their "Use my CSS" switch.
    pub enabled: bool,
    /// When the CSS text last changed, unix milliseconds.
    pub css_saved_at: Option<i64>,
}

impl Default for Stored {
    fn default() -> Self {
        Self {
            theme: None,
            css: String::new(),
            enabled: true,
            css_saved_at: None,
        }
    }
}

impl From<AppearanceRow> for Stored {
    fn from(row: AppearanceRow) -> Self {
        Self {
            theme: row.theme,
            css: row.custom_css,
            enabled: row.custom_css_enabled,
            css_saved_at: row.css_saved_at.map(|at| at.0),
        }
    }
}

/// A checked change: each field left as `None` keeps its value.
#[derive(Debug, Clone, Default)]
pub struct Change {
    /// The theme to use.
    pub theme: Option<ThemeChoice>,
    /// The CSS text; empty clears it.
    pub css: Option<String>,
    /// The "Use my CSS" switch.
    pub enabled: Option<bool>,
}

/// What a write found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    /// The change was saved; the row as it is now.
    Saved(Stored),
    /// The write was only for a person with no row, and one was there; it
    /// is answered unchanged.
    AlreadyStored(Stored),
}

/// One kept save of a person's CSS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Save {
    /// The save's id.
    pub id: i64,
    /// The sheet.
    pub css: String,
    /// When it was saved, unix milliseconds.
    pub saved_at: i64,
}

/// Each person's appearance over `app_user_appearances` and
/// `app_custom_css_saves`.
#[derive(Clone)]
pub struct AppearanceStore {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl AppearanceStore {
    /// The store over one database; `clock` stamps each change.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    /// The person's stored appearance, or `None` while they have no row.
    pub async fn read(&self, user_id: &str) -> Result<Option<Stored>, DbError> {
        let user_id = user_id.to_owned();
        self.db
            .read("appearance.read", move |conn| {
                conn.prepare_cached(sql::APPEARANCE.text())?
                    .query_row([user_id], AppearanceRow::read)
                    .optional()
                    .map(|row| row.map(Stored::from))
                    .map_err(DbError::from)
            })
            .await
    }

    /// Save `change` over the person's row, or over the defaults when they
    /// have none. A change of the CSS text to one that is not empty is kept
    /// as a save, and the saves past the newest [`KEPT_SAVES`] are dropped,
    /// in the same transaction. With `only_if_new`, a person who already
    /// has a row keeps it untouched.
    pub async fn write(
        &self,
        user_id: &str,
        change: Change,
        only_if_new: bool,
    ) -> Result<Written, DbError> {
        let user_id = user_id.to_owned();
        let now = self.clock.now_ms();
        self.db
            .write(Lane::Foreground, "appearance.write", move |tx| {
                let current = tx
                    .prepare_cached(sql::APPEARANCE.text())?
                    .query_row([&user_id], AppearanceRow::read)
                    .optional()?
                    .map(Stored::from);
                if only_if_new && let Some(current) = current {
                    return Ok(Written::AlreadyStored(current));
                }
                let mut next = current.unwrap_or_default();
                if let Some(theme) = change.theme {
                    next.theme = Some(theme);
                }
                if let Some(enabled) = change.enabled {
                    next.enabled = enabled;
                }
                if let Some(css) = change.css
                    && css != next.css
                {
                    if !css.is_empty() {
                        tx.execute(sql::ADD_CSS_SAVE.text(), params![user_id, css, Millis(now)])?;
                        tx.execute(sql::TRIM_CSS_SAVES.text(), params![user_id, KEPT_SAVES])?;
                    }
                    next.css = css;
                    next.css_saved_at = Some(now);
                }
                tx.execute(
                    sql::PUT_APPEARANCE.text(),
                    params![
                        user_id,
                        next.theme,
                        next.css,
                        next.enabled,
                        next.css_saved_at.map(Millis),
                        Millis(now)
                    ],
                )?;
                Ok(Written::Saved(next))
            })
            .await
    }

    /// Switch the person's CSS off, keeping its text. A person with no row
    /// gets none. Answers the row as it is now.
    pub async fn turn_off(&self, user_id: &str) -> Result<Option<Stored>, DbError> {
        let user_id = user_id.to_owned();
        let now = self.clock.now_ms();
        self.db
            .write(Lane::Foreground, "appearance.turn_off", move |tx| {
                tx.execute(sql::TURN_OFF_CSS.text(), params![user_id, Millis(now)])?;
                tx.prepare_cached(sql::APPEARANCE.text())?
                    .query_row([&user_id], AppearanceRow::read)
                    .optional()
                    .map(|row| row.map(Stored::from))
                    .map_err(DbError::from)
            })
            .await
    }

    /// The person's kept saves, newest first.
    pub async fn saves(&self, user_id: &str) -> Result<Vec<Save>, DbError> {
        let user_id = user_id.to_owned();
        self.db
            .read("appearance.saves", move |conn| {
                conn.prepare_cached(sql::CSS_SAVES.text())?
                    .query_map([user_id], CssSaveRow::read)?
                    .map(|row| {
                        row.map(|row| Save {
                            id: row.id,
                            css: row.css,
                            saved_at: row.saved_at.0,
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await
    }
}
