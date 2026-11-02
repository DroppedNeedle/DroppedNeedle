//! SQLite store over `catalog_youtube_links` and `catalog_youtube_track_links`.
//!
//! Reads go through the reader pool, writes through the writer lane, one
//! transaction per call. Track counts are always counted live when read;
//! the stored `track_count` column is still kept up to date because the v2
//! export carries it.
//!
//! Times are stored as integer milliseconds, which SQLite makes inside the
//! write so a row's time is the time it was stored, and read as ISO-8601 UTC
//! text.

use rusqlite::{OptionalExtension as _, Transaction, params};

use crate::collections::db::{CollectionsDb, StoreError};
use droppedneedle_kernel::db::DbError;

use super::models::{YouTubeLink, YouTubeTrackLink};
use super::sql;

/// The embed URL for one video id.
pub fn embed_url(video_id: &str) -> String {
    format!("https://www.youtube.com/embed/{video_id}")
}

/// The album a write belongs to.
#[derive(Debug, Clone)]
pub struct AlbumRef {
    /// Album id.
    pub album_id: String,
    /// Album title.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Cover URL, when known.
    pub cover_url: Option<String>,
}

/// One track video to store.
#[derive(Debug, Clone)]
pub struct NewTrackLink {
    /// Disc number.
    pub disc_number: i64,
    /// Track position on its disc.
    pub track_number: i64,
    /// Track title.
    pub track_name: String,
    /// Video id.
    pub video_id: String,
}

/// Changes to a saved album link. `None` keeps the saved value.
#[derive(Debug, Clone, Default)]
pub struct LinkEdit {
    /// New full-album video id.
    pub video_id: Option<String>,
    /// New album title.
    pub album_name: Option<String>,
    /// New artist name.
    pub artist_name: Option<String>,
    /// New cover URL; `Some(None)` clears it.
    pub cover_url: Option<Option<String>>,
}

/// How an album link was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOrigin {
    /// YouTube search found it. A new cover only replaces a saved one.
    Search,
    /// A person pasted it. The cover is taken as sent.
    Manual,
}

/// The YouTube link store.
#[derive(Clone, Debug)]
pub struct LinkStore {
    db: CollectionsDb,
}

impl LinkStore {
    /// Store over the application database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// One album link with its live track count.
    pub async fn link(&self, album_id: &str) -> Result<Option<YouTubeLink>, StoreError> {
        let album_id = album_id.to_owned();
        self.db
            .read("youtube link", move |conn| {
                let link = conn
                    .query_row(sql::LINK.text(), params![album_id], link_from_row)
                    .optional()?;
                Ok(link)
            })
            .await
    }

    /// Every album link, newest first.
    pub async fn links(&self) -> Result<Vec<YouTubeLink>, StoreError> {
        self.db
            .read("youtube links", |conn| {
                let links = conn
                    .prepare(sql::LINKS.text())?
                    .query_map([], link_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(links)
            })
            .await
    }

    /// An album's track links, in disc then track order.
    pub async fn track_links(&self, album_id: &str) -> Result<Vec<YouTubeTrackLink>, StoreError> {
        let album_id = album_id.to_owned();
        self.db
            .read("youtube track links", move |conn| {
                let tracks = conn
                    .prepare(sql::TRACK_LINKS.text())?
                    .query_map(params![album_id], track_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(tracks)
            })
            .await
    }

    /// Save a full-album video, replacing any saved one. The creation time
    /// restarts, as in v2; the track videos stay.
    pub async fn save_link(
        &self,
        album: AlbumRef,
        video_id: String,
        origin: LinkOrigin,
    ) -> Result<YouTubeLink, StoreError> {
        self.db
            .write("youtube save link", move |tx| {
                let statement = match origin {
                    LinkOrigin::Search => sql::SAVE_FOUND_LINK,
                    LinkOrigin::Manual => sql::SAVE_PASTED_LINK,
                };
                tx.execute(
                    statement.text(),
                    params![
                        album.album_id,
                        video_id,
                        album.album_name,
                        album.artist_name,
                        embed_url(&video_id),
                        album.cover_url,
                        origin == LinkOrigin::Manual,
                    ],
                )?;
                read_link(tx, &album.album_id)?.ok_or_else(|| DbError::WriteFailed {
                    operation: "youtube save link".to_owned(),
                    cause: "saved youtube link vanished".to_owned(),
                })
            })
            .await
    }

    /// Apply `edit` to a saved link, keeping its creation time and origin.
    /// `None` when no link is saved under `album_id`.
    pub async fn edit_link(
        &self,
        album_id: String,
        edit: LinkEdit,
    ) -> Result<Option<YouTubeLink>, StoreError> {
        self.db
            .write("youtube edit link", move |tx| {
                let embed = edit.video_id.as_deref().map(embed_url);
                let (keep_cover, cover) = match edit.cover_url {
                    None => (true, None),
                    Some(cover) => (false, cover),
                };
                let changed = tx.execute(
                    sql::EDIT_LINK.text(),
                    params![
                        album_id,
                        edit.video_id,
                        embed,
                        edit.album_name,
                        edit.artist_name,
                        keep_cover,
                        cover,
                    ],
                )?;
                if changed == 0 {
                    return Ok(None);
                }
                read_link(tx, &album_id)
            })
            .await
    }

    /// Delete an album link and all of its track links.
    pub async fn delete_link(&self, album_id: String) -> Result<(), StoreError> {
        self.db
            .write("youtube delete link", move |tx| {
                tx.execute(sql::DELETE_TRACK_LINKS.text(), params![album_id])?;
                tx.execute(sql::DELETE_LINK.text(), params![album_id])?;
                Ok(())
            })
            .await
    }

    /// Save track videos and make sure the album has an entry, in one
    /// transaction. Returns the stored rows in the order given.
    pub async fn save_track_links(
        &self,
        album: AlbumRef,
        tracks: Vec<NewTrackLink>,
    ) -> Result<Vec<YouTubeTrackLink>, StoreError> {
        self.db
            .write("youtube save track links", move |tx| {
                for track in &tracks {
                    tx.execute(
                        sql::SAVE_TRACK_LINK.text(),
                        params![
                            album.album_id,
                            track.track_number,
                            track.disc_number,
                            album.album_name,
                            track.track_name,
                            track.video_id,
                            album.artist_name,
                            embed_url(&track.video_id),
                        ],
                    )?;
                }
                tx.execute(
                    sql::ENSURE_LINK.text(),
                    params![
                        album.album_id,
                        album.album_name,
                        album.artist_name,
                        album.cover_url
                    ],
                )?;
                tracks
                    .iter()
                    .map(|track| {
                        read_track(tx, &album.album_id, track.disc_number, track.track_number)?
                            .ok_or_else(|| DbError::WriteFailed {
                                operation: "youtube save track links".to_owned(),
                                cause: "saved youtube track link vanished".to_owned(),
                            })
                    })
                    .collect()
            })
            .await
    }
}

fn link_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<YouTubeLink> {
    let row = sql::LinkColumns::read(row)?;
    Ok(YouTubeLink {
        album_id: row.album_id,
        video_id: row.video_id,
        album_name: row.album_name,
        artist_name: row.artist_name,
        embed_url: row.embed_url,
        cover_url: row.cover_url,
        created_at: row.created_at,
        is_manual: row.is_manual,
        track_count: row.track_count,
    })
}

fn track_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<YouTubeTrackLink> {
    let row = sql::TrackColumns::read(row)?;
    Ok(YouTubeTrackLink {
        album_id: row.album_id,
        album_name: row.album_name,
        disc_number: row.disc_number,
        track_number: row.track_number,
        track_name: row.track_name,
        video_id: row.video_id,
        artist_name: row.artist_name,
        embed_url: row.embed_url,
        created_at: row.created_at,
    })
}

fn read_link(tx: &Transaction, album_id: &str) -> Result<Option<YouTubeLink>, DbError> {
    let link = tx
        .query_row(sql::LINK.text(), params![album_id], link_from_row)
        .optional()?;
    Ok(link)
}

fn read_track(
    tx: &Transaction,
    album_id: &str,
    disc_number: i64,
    track_number: i64,
) -> Result<Option<YouTubeTrackLink>, DbError> {
    let track = tx
        .query_row(
            sql::TRACK_LINK.text(),
            params![album_id, disc_number, track_number],
            track_from_row,
        )
        .optional()?;
    Ok(track)
}
