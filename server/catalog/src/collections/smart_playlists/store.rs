//! Every statement takes the caller's id, including updates and deletes.
use droppedneedle_kernel::db::{Db, Lane};
use rusqlite::{OptionalExtension, params};

use super::{SmartPlaylist, SmartPlaylistInput};
use crate::collections::{db::StoreError, error::CollectionsError, sql};

impl From<sql::SmartPlaylistRow> for SmartPlaylist {
    fn from(row: sql::SmartPlaylistRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            min_rating: row.min_rating,
        }
    }
}

pub(super) struct Owned<'a> {
    pub db: &'a Db,
    pub user_id: String,
}

impl Owned<'_> {
    pub async fn list(self) -> Result<Vec<SmartPlaylist>, StoreError> {
        self.db
            .read("list smart playlists", move |conn| {
                Ok(conn
                    .prepare_cached(sql::LIST_SMART.text())?
                    .query_map([self.user_id], sql::SmartPlaylistRow::read)?
                    .map(|result| result.map(SmartPlaylist::from))
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    pub async fn get(self, id: String) -> Result<SmartPlaylist, CollectionsError> {
        self.db
            .read::<_, StoreError, _>("get smart playlist", move |conn| {
                Ok(conn
                    .query_row(
                        sql::GET_SMART.text(),
                        params![self.user_id, id],
                        sql::SmartPlaylistRow::read,
                    )
                    .optional()?)
            })
            .await?
            .map(SmartPlaylist::from)
            .ok_or(CollectionsError::NotFound)
    }

    pub async fn save(
        self,
        existing: Option<String>,
        input: SmartPlaylistInput,
    ) -> Result<SmartPlaylist, CollectionsError> {
        let name = crate::collections::service::clean_name(&input.name)?;
        if !(1..=5).contains(&input.min_rating) {
            return Err(CollectionsError::invalid(
                "Minimum rating must be between 1 and 5",
            ));
        }
        let (statement, id) = match existing {
            Some(id) => (sql::UPDATE_SMART, id),
            None => (sql::INSERT_SMART, uuid::Uuid::new_v4().to_string()),
        };
        let playlist = SmartPlaylist {
            id,
            name,
            min_rating: input.min_rating,
        };
        self.db
            .write::<_, StoreError, _>(Lane::Foreground, "save smart playlist", move |tx| {
                let changed = tx.execute(
                    statement.text(),
                    params![
                        self.user_id,
                        playlist.id,
                        playlist.name,
                        playlist.min_rating
                    ],
                )?;
                Ok((changed > 0).then_some(playlist))
            })
            .await?
            .ok_or(CollectionsError::NotFound)
    }

    pub async fn delete(self, id: String) -> Result<(), CollectionsError> {
        let changed = self
            .db
            .write::<_, StoreError, _>(Lane::Foreground, "delete smart playlist", move |tx| {
                Ok(tx.execute(sql::DELETE_SMART.text(), params![self.user_id, id])?)
            })
            .await?;
        if changed == 0 {
            return Err(CollectionsError::NotFound);
        }
        Ok(())
    }
}
