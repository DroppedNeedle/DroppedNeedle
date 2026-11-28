//! The playlists Discover keeps for a person's apps: "This week's set",
//! "From your shelves" and one per Daily Mix, private to them and marked by
//! their source (`discover-list:`, compared exactly), so the app's own
//! lists never feed Discover's seeds and nobody changes their songs
//! (`made_by_app!`, `not_app_kept!`). The web UI does not list them.
//!
//! Only Discover's refresh (`crate::discover::engine::app_lists`) writes
//! them, each wholesale: a list is made once it has songs, one whose name
//! and tracks are as they were is left as it is, one whose songs all went
//! stays, empty, so an app that pinned it keeps it, and one no longer
//! wanted goes. "Forget my history" removes
//! them.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, Transaction, params};

use super::db::{CollectionsDb, StoreError};
use super::service::import_entry;
use super::sql;
use super::state::CollectionsState;
use super::store::playlists::{insert_entries, insert_playlist};
use crate::discover::cache::ForgetCounts;
use crate::local::stores::StoreError as LibraryStoreError;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};

/// The source every list Discover keeps starts with.
pub const PREFIX: &str = "discover-list:";

/// Whether a playlist's source marks it as a list Discover keeps.
pub fn is_discover_list(source_ref: &str) -> bool {
    source_ref.starts_with(PREFIX)
}

/// One list Discover keeps for a person's apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverList {
    /// The library's tracks of their newest weekly set's picks.
    WeekSet,
    /// The tracks of the albums on their own shelves.
    Shelves,
    /// The library's tracks of their Daily Mix in this place on the page.
    Mix(usize),
}

impl DiscoverList {
    /// The source that marks it.
    pub fn source_ref(self) -> String {
        match self {
            Self::WeekSet => format!("{PREFIX}week-set"),
            Self::Shelves => format!("{PREFIX}shelves"),
            Self::Mix(place) => format!("{PREFIX}mix:{place}"),
        }
    }
}

/// One list as a refresh wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// Which list.
    pub list: DiscoverList,
    /// Its name, as the apps show it.
    pub name: String,
    /// Its library tracks, in order.
    pub tracks: Vec<String>,
}

/// What one write changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Written {
    /// Lists made.
    pub created: usize,
    /// Lists whose name or songs were replaced.
    pub replaced: usize,
    /// Lists removed.
    pub removed: usize,
}

/// One list as kept: its id and name, and its library tracks in order.
struct Current {
    id: String,
    name: String,
    files: Vec<Option<String>>,
}

/// `user_id`'s lists as kept, by source, from one snapshot.
async fn current(
    db: &CollectionsDb,
    user_id: &str,
) -> Result<HashMap<String, Current>, StoreError> {
    let user = user_id.to_owned();
    db.read("discover_lists.current", move |conn| {
        // The lists and their tracks must agree.
        let lists = conn
            .prepare_cached(sql::DISCOVER_LISTS_OF.text())?
            .query_map([&user], sql::DiscoverListRow::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut kept = HashMap::new();
        for list in lists {
            let files = conn
                .prepare_cached(sql::SOURCE_LIST_FILES.text())?
                .query_map(params![user, list.source_ref], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<Option<String>>>>()?;
            kept.insert(
                list.source_ref,
                Current {
                    id: list.id,
                    name: list.name,
                    files,
                },
            );
        }
        Ok::<_, DbError>(kept)
    })
    .await
}

/// The id of `user_id`'s list from `source`, when it is there.
fn list_id(tx: &Transaction, user_id: &str, source: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        sql::FIND_BY_SOURCE.text(),
        params![user_id, source],
        |row| row.get(0),
    )
    .optional()
}

/// What a write does to one list.
enum Change {
    /// Make it, or replace its name and songs.
    Put {
        source: String,
        name: String,
        entries: Vec<super::store::playlists::NewEntry>,
    },
    /// Take it away.
    Remove { id: String },
}

/// Write `user_id`'s lists as `wanted` says, at `now_ms`, in one
/// transaction: each to its name and library tracks in order (a track no
/// longer in the library left out), made when it is missing and has songs,
/// emptied rather than removed, and left alone when it is as it was; a kept list not in `wanted` is removed. Nothing
/// is written when `fresh` answers false inside the write (the person
/// forgot their history after the refresh read what it wants), or once
/// their account is gone. `done` finishes the job in the same transaction.
///
/// # Errors
///
/// A read's or the write's failure.
pub async fn write(
    state: &CollectionsState,
    user_id: &str,
    now_ms: i64,
    wanted: Vec<Wanted>,
    fresh: impl FnOnce() -> bool + Send + 'static,
    done: impl FnOnce(&Transaction) -> Result<(), StoreError> + Send + 'static,
) -> Result<Written, StoreError> {
    let mut kept = current(&state.db, user_id).await?;
    let mut changes = Vec::new();
    let mut ids = Vec::new();
    for want in wanted {
        let source = want.list.source_ref();
        if let Some(now) = kept.remove(&source) {
            let same = now.name == want.name
                && now.files.len() == want.tracks.len()
                && now
                    .files
                    .iter()
                    .zip(&want.tracks)
                    .all(|(file, id)| file.as_deref() == Some(id.as_str()));
            if same {
                continue;
            }
        } else if want.tracks.is_empty() {
            // A list is made only once it has songs.
            continue;
        }
        ids.extend(want.tracks.iter().cloned());
        changes.push((source, want.name, want.tracks));
    }
    let mut changes: Vec<Change> = if changes.is_empty() {
        Vec::new()
    } else {
        let found = state
            .items
            .tracks_by_ids(&ids)
            .await
            .map_err(|error| match error {
                LibraryStoreError::Busy(operation) => StoreError::Busy(operation),
                LibraryStoreError::Internal(cause) => StoreError::Internal(cause),
            })?;
        let found: HashMap<&str, _> = found
            .iter()
            .map(|track| (track.id.as_str(), track))
            .collect();
        changes
            .into_iter()
            .map(|(source, name, tracks)| Change::Put {
                source,
                name,
                entries: tracks
                    .iter()
                    .filter_map(|id| found.get(id.as_str()))
                    .map(|track| import_entry(track))
                    .collect(),
            })
            .collect()
    };
    changes.extend(
        kept.into_values()
            .map(|gone| Change::Remove { id: gone.id }),
    );
    let user_id = user_id.to_owned();
    state
        .db
        .write_background("discover_lists.write", move |tx| {
            let mut written = Written::default();
            let account: bool =
                tx.query_row(sql::ACCOUNT_THERE.text(), params![user_id], |row| {
                    row.get(0)
                })?;
            if !account || !fresh() {
                done(tx)?;
                return Ok(written);
            }
            for change in changes {
                match change {
                    Change::Remove { id } => {
                        written.removed +=
                            tx.execute(sql::DISCOVER_LIST_DELETE.text(), params![id, user_id])?;
                    }
                    Change::Put {
                        source,
                        name,
                        entries,
                    } => match list_id(tx, &user_id, &source)? {
                        Some(id) => {
                            tx.execute(sql::CLEAR_ENTRIES.text(), params![id])?;
                            insert_entries(tx, &id, None, &entries, now_ms)?;
                            tx.execute(
                                sql::DISCOVER_LIST_RENAME.text(),
                                params![name, id, user_id],
                            )?;
                            tx.execute(sql::STAMP.text(), params![now_ms, id])?;
                            written.replaced += 1;
                        }
                        None if entries.is_empty() => {}
                        None => {
                            let id = uuid::Uuid::new_v4().to_string();
                            insert_playlist(tx, &id, &name, now_ms, Some(&source), &user_id)?;
                            insert_entries(tx, &id, None, &entries, now_ms)?;
                            written.created += 1;
                        }
                    },
                }
            }
            done(tx)?;
            Ok(written)
        })
        .await
}

/// Remove every list Discover keeps for `user_id`'s apps, in one write:
/// a step of "Forget my history". Their forget count moves on inside it,
/// so a refresh of theirs that began before it keeps nothing. Answers how
/// many went.
///
/// # Errors
///
/// The write's failure.
pub async fn forget(db: &Db, forgets: &ForgetCounts, user_id: &str) -> Result<usize, DbError> {
    let (forgets, user) = (forgets.clone(), user_id.to_owned());
    db.write(Lane::Foreground, "discover_lists.forget", move |tx| {
        forgets.bump(&user);
        Ok::<_, DbError>(tx.execute(sql::DISCOVER_LISTS_DELETE.text(), [&user])?)
    })
    .await
}
