//! The store behind each person's home layout.

use std::sync::Arc;

use rusqlite::{OptionalExtension as _, params};

use super::models::{HomeLayoutUpdate, HomeLayoutWidget};
use super::sql::{self, LayoutRow};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};

/// The layout schema version this build writes.
pub const LAYOUT_VERSION: i64 = 1;

/// The most widgets one layout may name, and the most it may hide.
pub const MAX_WIDGETS: usize = 64;
/// The widest cell a widget may take, in reference columns.
pub const MAX_WIDTH: i64 = 24;
/// The tallest cell a widget may take, in rows.
pub const MAX_HEIGHT: i64 = 48;

/// One stored layout, decoded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stored {
    /// Layout schema version.
    pub version: i64,
    /// Widgets, empty when the built-in order applies.
    pub widgets: Vec<HomeLayoutWidget>,
    /// Hidden widget ids.
    pub hidden: Vec<String>,
}

impl From<LayoutRow> for Stored {
    fn from(row: LayoutRow) -> Self {
        Self {
            version: row.version,
            widgets: serde_json::from_str(&row.widgets).unwrap_or_default(),
            hidden: serde_json::from_str(&row.hidden).unwrap_or_default(),
        }
    }
}

/// Each person's home layout.
#[derive(Clone)]
pub struct HomeLayoutStore {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl HomeLayoutStore {
    /// Writes over one database.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    /// User `?1`'s layout, `None` when they never saved one.
    pub async fn load(&self, user_id: &str) -> Result<Option<Stored>, DbError> {
        let user_id = user_id.to_owned();
        self.db
            .read("settings.home_layout.read", move |conn| {
                conn.prepare_cached(sql::LAYOUT.text())?
                    .query_row([&user_id], LayoutRow::read)
                    .optional()
                    .map(|row| row.map(Stored::from))
                    .map_err(DbError::from)
            })
            .await
    }

    /// The first admin's layout: what force mode serves.
    pub async fn load_admin(&self) -> Result<Option<Stored>, DbError> {
        let admin = self
            .db
            .read("settings.home_layout.admin", |conn| {
                conn.prepare_cached(sql::FIRST_ADMIN.text())?
                    .query_row([], |row| row.get::<_, String>(0))
                    .optional()
                    .map_err(DbError::from)
            })
            .await?;
        match admin {
            Some(id) => self.load(&id).await,
            None => Ok(None),
        }
    }

    /// Replace user `?1`'s layout.
    pub async fn save(&self, user_id: &str, update: &HomeLayoutUpdate) -> Result<(), DbError> {
        let user_id = user_id.to_owned();
        let version = update.version;
        let widgets = serde_json::to_string(&update.widgets).unwrap_or_else(|_| "[]".to_owned());
        let hidden = serde_json::to_string(&update.hidden).unwrap_or_else(|_| "[]".to_owned());
        let now = self.clock.now_ms();
        self.db
            .write(Lane::Foreground, "settings.home_layout.write", move |tx| {
                tx.execute(
                    sql::PUT_LAYOUT.text(),
                    params![user_id, version, widgets, hidden, now],
                )?;
                Ok(())
            })
            .await
    }
}

/// Why a layout was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

/// Check a layout before it is stored: unique ids, sane cells, bounded lists.
pub fn check(update: &HomeLayoutUpdate) -> Result<(), Refused> {
    if update.version < 1 {
        return Err(Refused("The layout version must be at least 1.".to_owned()));
    }
    if update.widgets.len() > MAX_WIDGETS {
        return Err(Refused(format!(
            "A layout holds at most {MAX_WIDGETS} widgets."
        )));
    }
    if update.hidden.len() > MAX_WIDGETS {
        return Err(Refused(format!(
            "A layout hides at most {MAX_WIDGETS} widgets."
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for widget in &update.widgets {
        if widget.id.is_empty() || widget.id.len() > 96 {
            return Err(Refused(
                "A widget id must be 1 to 96 characters.".to_owned(),
            ));
        }
        if widget.kind.is_empty() || widget.kind.len() > 32 {
            return Err(Refused(
                "A widget kind must be 1 to 32 characters.".to_owned(),
            ));
        }
        if !seen.insert(widget.id.as_str()) {
            return Err(Refused(format!("Two widgets share the id {}.", widget.id)));
        }
        if widget.x < 0 || widget.y < 0 {
            return Err(Refused(
                "A widget cannot sit before the first cell.".to_owned(),
            ));
        }
        if widget.w < 1 || widget.w > MAX_WIDTH || widget.h < 1 || widget.h > MAX_HEIGHT {
            return Err(Refused(format!(
                "A widget must be 1 to {MAX_WIDTH} cells wide and 1 to {MAX_HEIGHT} tall."
            )));
        }
    }
    for id in &update.hidden {
        if id.is_empty() || id.len() > 96 {
            return Err(Refused(
                "A hidden id must be 1 to 96 characters.".to_owned(),
            ));
        }
        if !seen.insert(id.as_str()) {
            return Err(Refused(format!("Two widgets share the id {id}.")));
        }
    }
    Ok(())
}
