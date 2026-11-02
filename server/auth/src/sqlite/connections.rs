//! Writes to `user_connections`, one row per user and outside service. Auth
//! owns the table, the lowest area with a link of its own, so every area
//! above it writes through here. The document is whatever the service's
//! feature stored; nothing here opens it.

use std::sync::Arc;

use super::sql;
use crate::times::to_iso;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};

/// Writes to users' links, dated by the clock it was given.
#[derive(Clone)]
pub struct UserConnections {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl UserConnections {
    /// Writes over one database.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    /// Store or replace a link. One already there stays as enabled as it was.
    pub async fn save(&self, user_id: &str, service: &str, document: &str) -> Result<(), DbError> {
        self.write_link(
            sql::SAVE_LINK.text(),
            "auth.link.save",
            user_id,
            service,
            document,
        )
        .await
        .map(|_| ())
    }

    /// Store or replace a link and enable it.
    pub async fn save_enabled(
        &self,
        user_id: &str,
        service: &str,
        document: &str,
    ) -> Result<(), DbError> {
        self.write_link(
            sql::SAVE_LINK_ENABLED.text(),
            "auth.link.save_enabled",
            user_id,
            service,
            document,
        )
        .await
        .map(|_| ())
    }

    /// Replace a link's document only when the link still exists, so a
    /// refresh racing a disconnect never brings it back. Answers whether a
    /// row changed.
    pub async fn replace_if_present(
        &self,
        user_id: &str,
        service: &str,
        document: &str,
    ) -> Result<bool, DbError> {
        self.write_link(
            sql::REPLACE_LINK_DOCUMENT.text(),
            "auth.link.replace",
            user_id,
            service,
            document,
        )
        .await
        .map(|changed| changed > 0)
    }

    /// Remove a link. Answers whether one was there.
    pub async fn delete(&self, user_id: &str, service: &str) -> Result<bool, DbError> {
        let (user_id, service) = (user_id.to_owned(), service.to_owned());
        self.db
            .write(Lane::Foreground, "auth.link.delete", move |tx| {
                Ok::<_, DbError>(
                    tx.execute(sql::DELETE_LINK.text(), rusqlite::params![user_id, service])?,
                )
            })
            .await
            .map(|changed| changed > 0)
    }

    /// Run one statement taking a user, a service, a document and the time.
    async fn write_link(
        &self,
        statement: &'static str,
        name: &'static str,
        user_id: &str,
        service: &str,
        document: &str,
    ) -> Result<usize, DbError> {
        let (user_id, service, document) =
            (user_id.to_owned(), service.to_owned(), document.to_owned());
        let now = to_iso(self.clock.now_unix());
        self.db
            .write(Lane::Foreground, name, move |tx| {
                Ok::<_, DbError>(tx.execute(
                    statement,
                    rusqlite::params![user_id, service, document, now],
                )?)
            })
            .await
    }
}
