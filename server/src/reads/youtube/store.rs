//! SQLite store over `youtube_links` and `youtube_track_links`.
//!
//! Reads go through the reader pool, writes through the writer lane, one
//! transaction per call. Track counts are always counted live when read;
//! the stored `track_count` column is still kept up to date because the v2
//! export carries it.
//!
//! Timestamps are ISO-8601 UTC text, as v2 wrote them, and SQLite makes
//! them inside the write so a row's time is the time it was stored.

use rusqlite::{OptionalExtension as _, Transaction, params};
use sqlx::Row as _;

use crate::db::OpError;
use crate::reads::collections::db::{CollectionsDb, StoreError};

use super::models::{YouTubeLink, YouTubeTrackLink};

/// Current UTC time in v2's ISO-8601 text shape.
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')";

const LINK_SELECT: &str = "SELECT l.album_id AS album_id, l.video_id AS video_id, \
    l.album_name AS album_name, l.artist_name AS artist_name, l.embed_url AS embed_url, \
    l.cover_url AS cover_url, l.created_at AS created_at, \
    COALESCE(l.is_manual, 0) AS is_manual, \
    (SELECT COUNT(*) FROM youtube_track_links t WHERE t.album_id = l.album_id) AS track_count \
    FROM youtube_links l";

const TRACK_SELECT: &str = "SELECT album_id, album_name, disc_number, track_number, \
    track_name, video_id, artist_name, embed_url, created_at FROM youtube_track_links";

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
        let pool = self.db.pool()?;
        let row = sqlx::query(&format!("{LINK_SELECT} WHERE l.album_id = ?1"))
            .bind(album_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| StoreError::read("youtube link", error))?;
        row.map(|row| link_from_row(&row))
            .transpose()
            .map_err(|error| StoreError::read("youtube link", error))
    }

    /// Every album link, newest first.
    pub async fn links(&self) -> Result<Vec<YouTubeLink>, StoreError> {
        let pool = self.db.pool()?;
        let rows = sqlx::query(&format!("{LINK_SELECT} ORDER BY l.created_at DESC"))
            .fetch_all(pool)
            .await
            .map_err(|error| StoreError::read("youtube links", error))?;
        rows.iter()
            .map(link_from_row)
            .collect::<Result<_, _>>()
            .map_err(|error| StoreError::read("youtube links", error))
    }

    /// An album's track links, in disc then track order.
    pub async fn track_links(&self, album_id: &str) -> Result<Vec<YouTubeTrackLink>, StoreError> {
        let pool = self.db.pool()?;
        let rows = sqlx::query(&format!(
            "{TRACK_SELECT} WHERE album_id = ?1 ORDER BY disc_number, track_number"
        ))
        .bind(album_id)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("youtube track links", error))?;
        rows.iter()
            .map(track_from_row)
            .collect::<Result<_, _>>()
            .map_err(|error| StoreError::read("youtube track links", error))
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
                let cover = match origin {
                    LinkOrigin::Search => "COALESCE(excluded.cover_url, youtube_links.cover_url)",
                    LinkOrigin::Manual => "excluded.cover_url",
                };
                tx.execute(
                    &format!(
                        "INSERT INTO youtube_links (album_id, video_id, album_name, artist_name, \
                         embed_url, cover_url, created_at, is_manual, track_count) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NOW}, ?7, \
                           (SELECT COUNT(*) FROM youtube_track_links WHERE album_id = ?1)) \
                         ON CONFLICT(album_id) DO UPDATE SET video_id = excluded.video_id, \
                         album_name = excluded.album_name, artist_name = excluded.artist_name, \
                         embed_url = excluded.embed_url, cover_url = {cover}, \
                         created_at = excluded.created_at, is_manual = excluded.is_manual"
                    ),
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
                read_link(tx, &album.album_id)?
                    .ok_or_else(|| OpError::Abort("saved youtube link vanished".to_owned()))
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
                    "UPDATE youtube_links SET video_id = COALESCE(?2, video_id), \
                     embed_url = COALESCE(?3, embed_url), \
                     album_name = COALESCE(?4, album_name), \
                     artist_name = COALESCE(?5, artist_name), \
                     cover_url = CASE WHEN ?6 THEN cover_url ELSE ?7 END \
                     WHERE album_id = ?1",
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
                tx.execute(
                    "DELETE FROM youtube_track_links WHERE album_id = ?1",
                    params![album_id],
                )?;
                tx.execute(
                    "DELETE FROM youtube_links WHERE album_id = ?1",
                    params![album_id],
                )?;
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
                        &format!(
                            "INSERT INTO youtube_track_links (album_id, track_number, \
                             disc_number, album_name, track_name, video_id, artist_name, \
                             embed_url, created_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, {NOW}) \
                             ON CONFLICT(album_id, disc_number, track_number) DO UPDATE SET \
                             album_name = excluded.album_name, track_name = excluded.track_name, \
                             video_id = excluded.video_id, artist_name = excluded.artist_name, \
                             embed_url = excluded.embed_url, created_at = excluded.created_at"
                        ),
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
                    &format!(
                        "INSERT INTO youtube_links (album_id, video_id, album_name, artist_name, \
                         embed_url, cover_url, created_at, is_manual, track_count) \
                         VALUES (?1, NULL, ?2, ?3, NULL, ?4, {NOW}, 0, \
                           (SELECT COUNT(*) FROM youtube_track_links WHERE album_id = ?1)) \
                         ON CONFLICT(album_id) DO UPDATE SET \
                         track_count = excluded.track_count, \
                         cover_url = COALESCE(excluded.cover_url, youtube_links.cover_url)"
                    ),
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
                            .ok_or_else(|| {
                                OpError::Abort("saved youtube track link vanished".to_owned())
                            })
                    })
                    .collect()
            })
            .await
    }

    /// Delete one track link. An album entry left with neither an album
    /// video nor track videos goes too, so the library page shows no empty
    /// card.
    pub async fn delete_track_link(
        &self,
        album_id: String,
        disc_number: i64,
        track_number: i64,
    ) -> Result<(), StoreError> {
        self.db
            .write("youtube delete track link", move |tx| {
                tx.execute(
                    "DELETE FROM youtube_track_links \
                     WHERE album_id = ?1 AND disc_number = ?2 AND track_number = ?3",
                    params![album_id, disc_number, track_number],
                )?;
                tx.execute(
                    "UPDATE youtube_links SET track_count = \
                     (SELECT COUNT(*) FROM youtube_track_links WHERE album_id = ?1) \
                     WHERE album_id = ?1",
                    params![album_id],
                )?;
                tx.execute(
                    "DELETE FROM youtube_links WHERE album_id = ?1 AND video_id IS NULL \
                     AND track_count = 0",
                    params![album_id],
                )?;
                Ok(())
            })
            .await
    }
}

fn link_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<YouTubeLink, sqlx::Error> {
    Ok(YouTubeLink {
        album_id: row.try_get("album_id")?,
        video_id: row.try_get("video_id")?,
        album_name: row.try_get("album_name")?,
        artist_name: row.try_get("artist_name")?,
        embed_url: row.try_get("embed_url")?,
        cover_url: row.try_get("cover_url")?,
        created_at: row.try_get("created_at")?,
        is_manual: row.try_get::<i64, _>("is_manual")? != 0,
        track_count: row.try_get("track_count")?,
    })
}

fn track_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<YouTubeTrackLink, sqlx::Error> {
    Ok(YouTubeTrackLink {
        album_id: row.try_get("album_id")?,
        album_name: row.try_get("album_name")?,
        disc_number: row.try_get("disc_number")?,
        track_number: row.try_get("track_number")?,
        track_name: row.try_get("track_name")?,
        video_id: row.try_get("video_id")?,
        artist_name: row.try_get("artist_name")?,
        embed_url: row.try_get("embed_url")?,
        created_at: row.try_get("created_at")?,
    })
}

fn read_link(tx: &Transaction, album_id: &str) -> Result<Option<YouTubeLink>, OpError> {
    let link = tx
        .query_row(
            &format!("{LINK_SELECT} WHERE l.album_id = ?1"),
            params![album_id],
            |row| {
                Ok(YouTubeLink {
                    album_id: row.get("album_id")?,
                    video_id: row.get("video_id")?,
                    album_name: row.get("album_name")?,
                    artist_name: row.get("artist_name")?,
                    embed_url: row.get("embed_url")?,
                    cover_url: row.get("cover_url")?,
                    created_at: row.get("created_at")?,
                    is_manual: row.get::<_, i64>("is_manual")? != 0,
                    track_count: row.get("track_count")?,
                })
            },
        )
        .optional()?;
    Ok(link)
}

fn read_track(
    tx: &Transaction,
    album_id: &str,
    disc_number: i64,
    track_number: i64,
) -> Result<Option<YouTubeTrackLink>, OpError> {
    let track = tx
        .query_row(
            &format!(
                "{TRACK_SELECT} WHERE album_id = ?1 AND disc_number = ?2 AND track_number = ?3"
            ),
            params![album_id, disc_number, track_number],
            |row| {
                Ok(YouTubeTrackLink {
                    album_id: row.get("album_id")?,
                    album_name: row.get("album_name")?,
                    disc_number: row.get("disc_number")?,
                    track_number: row.get("track_number")?,
                    track_name: row.get("track_name")?,
                    video_id: row.get("video_id")?,
                    artist_name: row.get("artist_name")?,
                    embed_url: row.get("embed_url")?,
                    created_at: row.get("created_at")?,
                })
            },
        )
        .optional()?;
    Ok(track)
}
