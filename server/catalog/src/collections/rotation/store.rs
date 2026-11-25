//! The reads and writes behind the lists on rotation.

use std::collections::HashMap;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};

use super::sql::{self, ListRow};
use super::{DAY_MS, RotationIdea, week_hash, week_start};
use crate::collections::db::{CollectionsDb, StoreError};
use crate::collections::sql as playlist_sql;
use crate::collections::store::playlists::{NewEntry, insert_entries, insert_playlist};
use crate::local::sql::track_shuffle_values;
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::db::statement::Statement;

/// What one idea found at a look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    /// The idea.
    pub idea: RotationIdea,
    /// Its songs in the list's order, at most its size.
    pub ids: Vec<String>,
    /// How many songs match in all.
    pub matches: i64,
}

/// One look at a person's library, from one snapshot.
#[derive(Default)]
pub(crate) struct Look {
    /// False when they turned lists on rotation off; then nothing else is
    /// read.
    pub enabled: bool,
    /// Their rows, one an idea looked at before.
    pub rows: Vec<ListRow>,
    /// What each idea found, in idea order.
    pub found: Vec<Found>,
    /// The decade the decade idea shows if it is live after this look.
    pub decade: Option<i64>,
    /// The library files of each live list's entries, in order, by
    /// playlist id.
    pub entries: HashMap<String, Vec<Option<String>>>,
}

/// The statement of one idea.
fn statement(idea: RotationIdea) -> Statement {
    match idea {
        RotationIdea::AddedRecently => sql::ADDED_RECENTLY,
        RotationIdea::OnRepeat => sql::ON_REPEAT,
        RotationIdea::ForgottenFavourites => sql::FORGOTTEN_FAVOURITES,
        RotationIdea::DeepCuts => sql::DEEP_CUTS,
        RotationIdea::FiveStars => sql::FIVE_STARS,
        RotationIdea::NeverPlayed => sql::NEVER_PLAYED,
        RotationIdea::FollowedArtists => sql::FOLLOWED_ARTISTS,
        RotationIdea::Lossless => sql::LOSSLESS,
        RotationIdea::AYearAgo => sql::A_YEAR_AGO,
        RotationIdea::Decade => sql::DECADE,
        RotationIdea::LongOnes => sql::LONG_ONES,
    }
}

/// The values one idea's statement binds, in its order.
fn binds(idea: RotationIdea, user_id: &str, now_ms: i64, decade: i64) -> Vec<Value> {
    let week = week_start(now_ms);
    let user = || Value::Text(user_id.to_owned());
    let size = Value::Integer(idea.size());
    let mut values: Vec<Value> = if idea.shuffled() {
        // The low half of the week's hash seeds the shuffle.
        track_shuffle_values(week_hash(user_id, idea, week) as u32).to_vec()
    } else {
        Vec::new()
    };
    match idea {
        RotationIdea::AddedRecently => values.push(size),
        RotationIdea::OnRepeat => {
            values.extend([user(), Value::Integer(now_ms - 30 * DAY_MS), size]);
        }
        RotationIdea::AYearAgo => values.extend([
            user(),
            Value::Integer(now_ms - 395 * DAY_MS),
            Value::Integer(now_ms - 365 * DAY_MS),
            size,
        ]),
        RotationIdea::ForgottenFavourites => {
            values.extend([user(), Value::Integer(now_ms - 180 * DAY_MS), size]);
        }
        RotationIdea::FollowedArtists => {
            values.extend([user(), Value::Integer(now_ms - 30 * DAY_MS), size]);
        }
        RotationIdea::DeepCuts | RotationIdea::FiveStars | RotationIdea::NeverPlayed => {
            values.extend([user(), size]);
        }
        RotationIdea::Lossless | RotationIdea::LongOnes => values.push(size),
        RotationIdea::Decade => values.extend([Value::Integer(decade), size]),
    }
    values
}

/// The decade the decade idea shows: the one it shows while live and still
/// holding songs, else the next decade after the one it showed last (from
/// the first, round again) with enough songs. With none, the fullest one,
/// which counts how many songs it is short of.
fn decade_to_show(row: Option<&ListRow>, decades: &[sql::DecadeSongs]) -> (Option<i64>, i64) {
    let songs = |decade: i64| {
        decades
            .iter()
            .find(|one| one.decade == decade)
            .map_or(0, |one| one.songs)
    };
    if let Some(row) = row
        && row.playlist_id.is_some()
        && let Some(decade) = row.decade
        && songs(decade) > 0
    {
        return (Some(decade), songs(decade));
    }
    let bar = RotationIdea::Decade.bar();
    let last = row.and_then(|row| row.decade);
    let enough: Vec<&sql::DecadeSongs> = decades.iter().filter(|one| one.songs >= bar).collect();
    let next = enough
        .iter()
        .find(|one| last.is_none_or(|last| one.decade > last))
        .or_else(|| enough.first());
    match next {
        Some(one) => (Some(one.decade), one.songs),
        None => (None, decades.iter().map(|one| one.songs).max().unwrap_or(0)),
    }
}

/// Every row of `statement` over `values`.
fn rows<T>(
    conn: &Connection,
    statement: Statement,
    values: Vec<Value>,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, DbError> {
    Ok(conn
        .prepare_cached(statement.text())?
        .query_map(params_from_iter(values), map)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Whether person `user_id` has lists on rotation on.
fn enabled_in(conn: &Connection, user_id: &str) -> Result<bool, DbError> {
    Ok(conn
        .query_row(sql::ENABLED.text(), params![user_id], |row| {
            row.get::<_, bool>(0)
        })
        .optional()?
        .unwrap_or(true))
}

/// One look at `user_id`'s library at `now_ms`, on the long pool. Every
/// idea is read from one snapshot, so the counts the pick weighs and the
/// songs it keeps agree.
pub(crate) async fn look(
    db: &CollectionsDb,
    user_id: &str,
    now_ms: i64,
) -> Result<Look, StoreError> {
    let user_id = user_id.to_owned();
    db.read_long("rotation.look", move |conn| {
        if !enabled_in(conn, &user_id)? {
            return Ok(Look::default());
        }
        let lists = rows(
            conn,
            sql::LISTS,
            vec![Value::Text(user_id.clone())],
            ListRow::read,
        )?;
        let decades = rows(conn, sql::DECADE_SONGS, Vec::new(), sql::DecadeSongs::read)?;
        let decade_row = lists.iter().find(|row| row.idea == RotationIdea::Decade);
        let (decade, decade_matches) = decade_to_show(decade_row, &decades);
        let mut found = Vec::with_capacity(RotationIdea::ALL.len());
        for idea in RotationIdea::ALL {
            if idea == RotationIdea::Decade && decade.is_none() {
                found.push(Found {
                    idea,
                    ids: Vec::new(),
                    matches: decade_matches,
                });
                continue;
            }
            let picked = rows(
                conn,
                statement(idea),
                binds(idea, &user_id, now_ms, decade.unwrap_or(0)),
                sql::Picked::read,
            )?;
            let matches = picked.first().map_or(0, |one| one.matches);
            found.push(Found {
                idea,
                ids: picked.into_iter().map(|one| one.id).collect(),
                matches,
            });
        }
        let mut entries = HashMap::new();
        for playlist_id in lists.iter().filter_map(|row| row.playlist_id.clone()) {
            let files = rows(
                conn,
                sql::ENTRY_FILES,
                vec![Value::Text(playlist_id.clone())],
                |row| row.get::<_, Option<String>>(0),
            )?;
            entries.insert(playlist_id, files);
        }
        Ok(Look {
            enabled: true,
            rows: lists,
            found,
            decade,
            entries,
        })
    })
    .await
}

/// One change a refresh makes to a list.
#[derive(Debug, Clone)]
pub(crate) enum Change {
    /// The idea goes live in a new playlist.
    Create {
        /// The idea.
        idea: RotationIdea,
        /// The playlist's name.
        name: String,
        /// Its songs.
        entries: Vec<NewEntry>,
    },
    /// The live list's songs changed.
    Replace {
        /// The idea.
        idea: RotationIdea,
        /// Its playlist.
        playlist_id: String,
        /// Its songs now.
        entries: Vec<NewEntry>,
    },
    /// The live list stays as it is.
    Keep {
        /// The idea.
        idea: RotationIdea,
    },
    /// The idea is no longer live: its playlist goes.
    Drop {
        /// The idea.
        idea: RotationIdea,
        /// Its playlist.
        playlist_id: String,
    },
}

/// What a refresh wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Written {
    /// Lists that went live.
    pub created: usize,
    /// Live lists whose songs changed.
    pub replaced: usize,
    /// Lists that went.
    pub dropped: usize,
}

/// Write one refresh of `user_id` at `now_ms` in one transaction: every
/// idea's look (`looked`: the idea, how many songs, the decade it shows
/// when it is live), then `changes`. Nothing is written while lists on
/// rotation are off. A list that changed hands since the look (deleted,
/// or made live by another run) is left as it is. `done` finishes the job
/// in the same transaction.
pub(crate) async fn write(
    db: &CollectionsDb,
    user_id: &str,
    now_ms: i64,
    looked: Vec<(RotationIdea, i64, Option<i64>)>,
    changes: Vec<Change>,
    done: impl FnOnce(&Transaction) -> Result<(), StoreError> + Send + 'static,
) -> Result<Written, StoreError> {
    let user_id = user_id.to_owned();
    db.write_background("rotation.refresh", move |tx| {
        let mut written = Written::default();
        // An account deleted since the job was queued has nothing to refresh.
        let account: bool =
            tx.query_row(sql::ACCOUNT.text(), params![user_id], |row| row.get(0))?;
        if !account || !enabled_in(tx, &user_id)? {
            done(tx)?;
            return Ok(written);
        }
        for (idea, matches, decade) in &looked {
            tx.execute(
                sql::LOOKED.text(),
                params![user_id, idea, decade, matches, now_ms],
            )?;
        }
        for change in changes {
            match change {
                Change::Create {
                    idea,
                    name,
                    entries,
                } => {
                    if live_playlist(tx, &user_id, idea)?.is_some() {
                        continue;
                    }
                    let id = uuid::Uuid::new_v4().to_string();
                    insert_playlist(tx, &id, &name, now_ms, None, &user_id)?;
                    insert_entries(tx, &id, None, &entries, now_ms)?;
                    tx.execute(sql::GO_LIVE.text(), params![user_id, idea, id, now_ms])?;
                    written.created += 1;
                }
                Change::Replace {
                    idea,
                    playlist_id,
                    entries,
                } => {
                    if live_playlist(tx, &user_id, idea)?.as_deref() != Some(&playlist_id) {
                        continue;
                    }
                    tx.execute(playlist_sql::CLEAR_ENTRIES.text(), params![playlist_id])?;
                    insert_entries(tx, &playlist_id, None, &entries, now_ms)?;
                    tx.execute(playlist_sql::STAMP.text(), params![now_ms, playlist_id])?;
                    tx.execute(sql::STAY_LIVE.text(), params![user_id, idea, now_ms])?;
                    written.replaced += 1;
                }
                Change::Keep { idea } => {
                    tx.execute(sql::STAY_LIVE.text(), params![user_id, idea, now_ms])?;
                }
                Change::Drop { idea, playlist_id } => {
                    tx.execute(sql::DROP_LIST.text(), params![user_id, playlist_id])?;
                    tx.execute(sql::GO_OUT.text(), params![user_id, idea])?;
                    written.dropped += 1;
                }
            }
        }
        done(tx)?;
        Ok(written)
    })
    .await
}

/// The playlist `user_id`'s idea fills now.
fn live_playlist(
    tx: &Transaction,
    user_id: &str,
    idea: RotationIdea,
) -> Result<Option<String>, DbError> {
    Ok(tx
        .query_row(sql::LIVE_PLAYLIST.text(), params![user_id, idea], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

/// Whether `user_id` has lists on rotation on, and their rows.
pub(crate) async fn standing(
    db: &CollectionsDb,
    user_id: &str,
) -> Result<(bool, Vec<ListRow>), StoreError> {
    let user = user_id.to_owned();
    let enabled = db
        .read("rotation.enabled", move |conn| enabled_in(conn, &user))
        .await?;
    let user = user_id.to_owned();
    let rows = db
        .read("rotation.lists", move |conn| {
            rows(conn, sql::LISTS, vec![Value::Text(user)], ListRow::read)
        })
        .await?;
    Ok((enabled, rows))
}

/// Turn `user_id`'s lists on rotation on or off at `now_ms`. Off deletes
/// their lists and forgets which were kept on and when each idea was last
/// looked at, so turning them on again picks at once; on runs `queue` in
/// the same write.
pub(crate) async fn set_enabled(
    db: &CollectionsDb,
    user_id: &str,
    enabled: bool,
    now_ms: i64,
    queue: impl FnOnce(&Transaction) -> Result<(), StoreError> + Send + 'static,
) -> Result<(), StoreError> {
    let user_id = user_id.to_owned();
    db.write_queueing("rotation.set_enabled", move |tx| {
        tx.execute(sql::SET_ENABLED.text(), params![user_id, enabled, now_ms])?;
        if enabled {
            queue(tx)?;
        } else {
            tx.execute(sql::DROP_ALL.text(), params![user_id])?;
            tx.execute(sql::RESET_ALL.text(), params![user_id])?;
        }
        Ok(())
    })
    .await
}

/// The idea of `user_id`'s playlist, when it is a list on rotation.
pub(crate) async fn idea_of(
    db: &CollectionsDb,
    user_id: &str,
    playlist_id: &str,
) -> Result<Option<RotationIdea>, StoreError> {
    let (user_id, playlist_id) = (user_id.to_owned(), playlist_id.to_owned());
    db.read("rotation.idea_of", move |conn| {
        Ok(conn
            .query_row(sql::IDEA_OF.text(), params![user_id, playlist_id], |row| {
                row.get(0)
            })
            .optional()?)
    })
    .await
}

/// Keep `user_id`'s list on rotation on past Monday, or let it go; false
/// when it is not one of theirs.
pub(crate) async fn pin(
    db: &CollectionsDb,
    user_id: &str,
    playlist_id: &str,
    pinned: bool,
) -> Result<bool, StoreError> {
    let (user_id, playlist_id) = (user_id.to_owned(), playlist_id.to_owned());
    db.write("rotation.pin", move |tx| {
        Ok(tx.execute(sql::PIN.text(), params![user_id, playlist_id, pinned])? > 0)
    })
    .await
}

/// The people among `users` whose refresh is due: lists on rotation on,
/// and an idea not looked at since `since_ms`.
pub(crate) async fn due(
    db: &CollectionsDb,
    users: Vec<String>,
    since_ms: i64,
) -> Result<Vec<String>, StoreError> {
    let asked = serde_json::to_string(&users)
        .map_err(|error| StoreError::Internal(format!("rotation.due: {error}")))?;
    db.read("rotation.due", move |conn| {
        rows(
            conn,
            sql::DUE,
            vec![Value::Text(asked), Value::Integer(since_ms)],
            |row| row.get::<_, String>(0),
        )
    })
    .await
}
