//! The playback store: everything reporting reads and writes in the
//! database.
//!
//! The reporting services are synchronous, so the store uses the blocking
//! forms of [`Db`]: reads go through the request read pool and a play is
//! one background write on the writer lane. Catalog reads hit
//! `local_tracks`, plays land in `library_play_history`, preferences and
//! the scrobble credentials come from the [`ListeningStore`], links read
//! `user_connections`, and display names resolve from `auth_users`. The
//! statements are in `sql.rs`.
//!
//! Every call runs through [`off_worker`], which moves a multi-threaded
//! runtime's other tasks off the current thread before it waits on the
//! database, so a busy database never stalls unrelated requests. Reads fail
//! closed (misses and defaults, never an error to the player) and log what
//! failed. A play the lane refuses is logged at error level with its
//! details, so no play disappears silently.
//!
//! A play and its scrobble jobs (one per service it goes to) are written
//! in one transaction: the jobs exist exactly when the play does.

use std::sync::Arc;

use droppedneedle_auth::times::{parse_iso, to_iso};
use droppedneedle_auth::users::stores::{LastFmKeys, LastFmSwitch};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::{JobError, JobQueue};
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_library::repairs::plays::{Matched, PlayKeys, match_play};
use rusqlite::OptionalExtension;

use super::forwarding::{ScrobblePayload, Service};
use super::listening::{LastFmSession, ListeningStore, ScrobblePrefs};
use super::ports::{PlayRecord, ProviderFailure, ScrobbleTargets, TrackInfo};
use super::sql;

/// Playback's reads and writes over the database.
pub struct PlaybackStore {
    db: Db,
    ids: Arc<dyn IdGenerator>,
    listening: ListeningStore,
    jobs: JobQueue,
}

/// One stored play, as a scrobble of it is sent: every send of a play is
/// built from this row, so each is the same request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlay {
    /// Whose play.
    pub user_id: String,
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title, when known.
    pub album_name: Option<String>,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// Track length in milliseconds (0 when unknown).
    pub duration_ms: i64,
    /// Play time, unix seconds.
    pub played_at: i64,
}

impl PlaybackStore {
    /// Playback reads and writes on `db`: plays take their ids from `ids`,
    /// links open under `crypto`, preference saves are dated by `clock`,
    /// and scrobble jobs go on `jobs`.
    pub fn new(
        db: Db,
        ids: Arc<dyn IdGenerator>,
        crypto: Arc<Crypto>,
        clock: Arc<dyn Clock>,
        jobs: JobQueue,
    ) -> Self {
        let listening = ListeningStore::new(db.clone(), crypto, clock);
        Self {
            db,
            ids,
            listening,
            jobs,
        }
    }

    /// The listening preferences and links this store reads.
    pub fn listening(&self) -> &ListeningStore {
        &self.listening
    }

    /// Resolve one track for reporting. An unknown id, or a read that
    /// failed, is `None` (the handler 404s); the catalog never fails a
    /// report.
    pub fn get_track(&self, track_id: &str) -> Option<TrackInfo> {
        off_worker(|| or_logged("playback-track", self.read_track(track_id), || None))
    }

    /// Record one play for the user, with a scrobble job for each service
    /// in `forward`. Every accepted play lands here whatever the user
    /// linked (v2 `PlayHistoryStore` rule). False when the write failed:
    /// the play and its jobs are then not kept.
    pub fn record(&self, user_id: &str, record: &PlayRecord, forward: ScrobbleTargets) -> bool {
        off_worker(|| self.write_play(user_id, record, forward))
    }

    /// One stored play by id; `None` when it is gone.
    pub async fn play(&self, play_id: &str) -> Result<Option<StoredPlay>, DbError> {
        let id = play_id.to_owned();
        self.db
            .read("playback-scrobble-play", move |conn| {
                Ok(conn
                    .query_row(sql::PLAY.text(), [id], |row| {
                        Ok((
                            StoredPlay {
                                user_id: row.get(0)?,
                                track_name: row.get(1)?,
                                artist_name: row.get(2)?,
                                album_name: row.get(3)?,
                                recording_mbid: row.get(4)?,
                                duration_ms: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                                played_at: 0,
                            },
                            row.get::<_, String>(6)?,
                        ))
                    })
                    .optional()?)
            })
            .await
            .map(|found| {
                found.and_then(|(play, played_at)| {
                    let played_at = parse_iso(&played_at)?;
                    Some(StoredPlay { played_at, ..play })
                })
            })
    }

    /// The user's ListenBrainz token, failure included: `None` is a link
    /// that is missing or does not open.
    pub fn listenbrainz_token_checked(&self, user_id: &str) -> Result<Option<String>, DbError> {
        off_worker(|| self.listening.listenbrainz_token_checked(user_id))
    }

    /// The user's Last.fm signing material, failure included, with
    /// `instance_keys` for a user without their own pair: `None` is a link
    /// that is missing or does not open.
    pub fn lastfm_session_checked(
        &self,
        user_id: &str,
        instance_keys: Option<&LastFmKeys>,
    ) -> Result<Option<LastFmSession>, DbError> {
        off_worker(|| {
            self.listening
                .lastfm_session_checked(user_id, instance_keys)
        })
    }

    /// The user's listening preferences and the scrobble services they
    /// linked, failure included: a scrobble job must not read a failed read
    /// as "switched off".
    pub fn forwarding(&self, user_id: &str) -> Result<(ScrobblePrefs, ScrobbleTargets), DbError> {
        off_worker(|| {
            Ok((
                self.listening.prefs_blocking(user_id)?,
                self.read_links(user_id)?,
            ))
        })
    }

    /// The user's listening preferences; the column defaults when the
    /// user saved none or the read failed.
    pub fn scrobble_prefs(&self, user_id: &str) -> ScrobblePrefs {
        off_worker(|| {
            or_logged(
                "playback-scrobble-prefs",
                self.listening.prefs_blocking(user_id),
                ScrobblePrefs::default,
            )
        })
    }

    /// The user's presence visibility. Fallible so a failed read redacts
    /// fail-closed instead of failing the report (v2
    /// `NowPlayingService._load_visibility` rule).
    pub fn visibility(&self, user_id: &str) -> Result<String, ProviderFailure> {
        off_worker(|| {
            self.listening
                .prefs_blocking(user_id)
                .map(|prefs| prefs.now_playing_visibility)
                .map_err(|error| {
                    tracing::warn!(read = "playback-visibility", %error, "playback read failed");
                    ProviderFailure(error.to_string())
                })
        })
    }

    /// The user's ListenBrainz token; `None` when unlinked or unreadable.
    pub fn listenbrainz_token(&self, user_id: &str) -> Option<String> {
        off_worker(|| self.listening.listenbrainz_token_blocking(user_id))
    }

    /// The user's Last.fm signing material; `None` while Last.fm is
    /// switched off or the user has no usable session.
    pub fn lastfm_session(&self, user_id: &str, lastfm: &LastFmSwitch) -> Option<LastFmSession> {
        off_worker(|| self.listening.lastfm_session_blocking(user_id, lastfm))
    }

    /// The scrobble services the user linked; a failed read reads as
    /// unlinked.
    pub fn linked(&self, user_id: &str) -> ScrobbleTargets {
        off_worker(|| {
            or_logged(
                "playback-scrobble-links",
                self.read_links(user_id),
                ScrobbleTargets::default,
            )
        })
    }

    /// The name presence entries show for the user: the display name, else
    /// the username, else the id.
    pub fn display_name(&self, user_id: &str) -> String {
        off_worker(|| {
            or_logged(
                "playback-display-name",
                self.read_display_name(user_id),
                || None,
            )
            .unwrap_or_else(|| user_id.to_owned())
        })
    }
}

/// Run blocking database work. On a multi-threaded runtime the worker hands
/// its other tasks to the rest of the pool first (`block_in_place`); on a
/// current-thread runtime (tests) or outside a runtime it runs inline.
fn off_worker<R>(work: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// The answer of a read, or `fallback` after logging why the read failed.
fn or_logged<T>(read: &'static str, result: Result<T, DbError>, fallback: impl FnOnce() -> T) -> T {
    result.unwrap_or_else(|error| {
        tracing::warn!(read, %error, "playback read failed; answering the default");
        fallback()
    })
}

impl PlaybackStore {
    /// The track a local id names now: an id that continued into another
    /// track reads as that track (retired ids stay aliases).
    fn read_track(&self, track_id: &str) -> Result<Option<TrackInfo>, DbError> {
        self.db.read_blocking("playback-track", |conn| {
            let track = conn
                .query_row(sql::TRACK.text(), [track_id], |row| {
                    let duration_ms = row
                        .get::<_, Option<f64>>("duration_seconds")?
                        .map(|secs| (secs * 1000.0) as i64)
                        .unwrap_or(0);
                    Ok(TrackInfo {
                        track_id: row.get("id")?,
                        title: row.get("title")?,
                        artist_name: row
                            .get::<_, Option<String>>("artist_name")?
                            .unwrap_or_default(),
                        album_title: row.get("album_title")?,
                        duration_ms,
                        recording_mbid: row
                            .get::<_, Option<String>>("recording_mbid")?
                            .filter(|mbid| !mbid.is_empty()),
                        // The release group the album was matched to,
                        // never an id from the file's tags: an unmatched
                        // album has none, and no cover address.
                        rg_mbid: row
                            .get::<_, Option<String>>("rg_mbid")?
                            .filter(|mbid| !mbid.is_empty()),
                    })
                })
                .optional()?;
            Ok(track)
        })
    }

    /// Insert one play as a background write. A play the lane refuses is
    /// logged with what it was, never dropped silently.
    ///
    /// A play of a library track is filed under the track its id names when
    /// the play is written (a session keeps the track it resolved at start,
    /// which may since have continued or merged into another), its album and
    /// its first credited artist (the album artist when the track credits
    /// none), all read from the catalog in the insert itself. A track that
    /// is gone leaves the three ids empty and the play is kept by name.
    ///
    /// A play made on a connected server or as a preview names no library
    /// track. It is filed the same way when it clearly matches one: see
    /// [`remote_match`], which reads on the write's own transaction.
    ///
    /// Each service in `forward` gets a scrobble job keyed by the play's id
    /// in the same transaction, so a play is forwarded only once it is kept
    /// and a kept play is never forgotten by a restart.
    fn write_play(&self, user_id: &str, record: &PlayRecord, forward: ScrobbleTargets) -> bool {
        let id = self.ids.new_id();
        let outcome = self
            .db
            .write_blocking(Lane::Background, "playback-play", |tx| {
                let named = record.track_id.clone().or_else(|| remote_match(tx, record));
                let track_id = match named {
                    Some(id) => Some(tx.query_row(sql::CURRENT_TRACK_ID.text(), [id], |row| {
                        row.get::<_, String>(0)
                    })?),
                    None => None,
                };
                tx.execute(
                    sql::RECORD_PLAY.text(),
                    rusqlite::params![
                        id,
                        user_id,
                        track_id,
                        record.track_name,
                        record.artist_name,
                        record.album_name,
                        record.recording_mbid,
                        record.release_group_mbid,
                        record.duration_ms,
                        record.source,
                        to_iso(record.played_at),
                    ],
                )?;
                let payload = ScrobblePayload {
                    play_id: id.clone(),
                };
                for service in Service::ALL {
                    if forward.wants(service) {
                        self.jobs.enqueue(tx, service.kind(), &id, &payload)?;
                    }
                }
                Ok::<_, JobError>(())
            });
        match outcome {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(
                    %error,
                    user_id,
                    track = %record.track_name,
                    artist = %record.artist_name,
                    played_at = record.played_at,
                    "play history write failed; the play is not recorded"
                );
                false
            }
        }
    }
}

/// The library track a play made on a connected server or as a preview
/// is filed under: the one playable track that holds the play's recording
/// id. Names alone never file such a play, and neither does a recording
/// several tracks share. The rule is the one the repair of older plays
/// applies to such a play, so it is filed the same way then and now. A
/// play of the library that names no track is not matched here: the
/// repair files it by its names after the next scan or start.
fn remote_match(conn: &rusqlite::Connection, record: &PlayRecord) -> Option<String> {
    let keys = PlayKeys {
        track_name: record.track_name.clone(),
        artist_name: record.artist_name.clone(),
        album_name: record.album_name.clone(),
        recording_mbid: record.recording_mbid.clone(),
        release_group_mbid: record.release_group_mbid.clone(),
        duration_ms: record.duration_ms,
        source: record.source.clone(),
    };
    if keys.is_local() {
        return None;
    }
    match match_play(conn, &keys) {
        Ok(Matched::Track { id, .. }) => Some(id),
        Ok(Matched::Left(_)) => None,
        Err(error) => {
            tracing::warn!(%error, "play could not be matched to a track; kept by name");
            None
        }
    }
}

impl PlaybackStore {
    /// Linked scrobble services from `user_connections`: a ListenBrainz
    /// row (its document is sealed whole, so presence is the signal) and
    /// a Last.fm row whose plain document carries a session key. A failed
    /// read reads as unlinked.
    fn read_links(&self, user_id: &str) -> Result<ScrobbleTargets, DbError> {
        let rows = self.db.read_blocking("playback-scrobble-links", |conn| {
            let mut query = conn.prepare(sql::SCROBBLE_LINKS.text())?;
            let rows = query
                .query_map([user_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>(rows)
        })?;
        let mut targets = ScrobbleTargets::default();
        for (service, data) in rows {
            match service.as_str() {
                "listenbrainz" => targets.listenbrainz = true,
                "lastfm" => {
                    targets.lastfm = serde_json::from_str::<serde_json::Value>(&data)
                        .ok()
                        .and_then(|doc| {
                            doc.get("session_key")
                                .and_then(serde_json::Value::as_str)
                                .map(|key| !key.is_empty())
                        })
                        .unwrap_or(false);
                }
                _ => {}
            }
        }
        Ok(targets)
    }

    /// The user's display name, else their username; `None` when neither
    /// is set or the user is gone.
    fn read_display_name(&self, user_id: &str) -> Result<Option<String>, DbError> {
        let names = self.db.read_blocking("playback-display-name", |conn| {
            let names = conn
                .query_row(sql::DISPLAY_NAMES.text(), [user_id], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                })
                .optional()?;
            Ok::<_, DbError>(names)
        })?;
        Ok(names.and_then(|(display, username)| {
            display
                .into_iter()
                .chain(username)
                .find(|name| !name.trim().is_empty())
        }))
    }
}
