//! A person's taste build: the rows the taste job writes, the seeds read
//! from them, and forgetting them.
//!
//! The four tables are results the job computes and writes whole, never a
//! read model kept by triggers: [`rebuild`] is the one writer, and they can
//! be thrown away and built again from their sources at any time.

use std::collections::HashMap;

use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::JobError;
use rusqlite::{Transaction, params};

use super::sql;
use super::taste;
use crate::discover::cache::ForgetCounts;
use crate::discover::listening::ArtistRow;
use crate::discover::queue::seeds::Seed;

/// The build row: what the build was made from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuildRow {
    /// When it was built, unix ms.
    pub built_at: i64,
    /// Plays in the app it counted.
    pub plays_counted: i64,
    /// Imported listens it counted.
    pub imported_listens_counted: i64,
    /// Artists played or listened to.
    pub artists_played: i64,
    /// Seeds.
    pub seeds: i64,
    /// The first play or listen counted, unix ms.
    pub history_from_at: Option<i64>,
    /// The latest, unix ms.
    pub history_to_at: Option<i64>,
    /// Whether something it needed was not there to read.
    pub incomplete: bool,
}

/// One known artist as kept.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ArtistOut {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// Plays in the app.
    pub plays: i64,
    /// Imported listens.
    pub imported_listens: i64,
    /// Calendar months with either.
    pub months_active: i64,
    /// The latest of either, unix ms.
    pub last_played_at: Option<i64>,
    /// Its weight.
    pub weight: f64,
    /// Whether it is a seed.
    pub is_seed: bool,
    /// Its group, when this build groups it.
    pub group_no: Option<i64>,
}

/// One known release group as kept.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct KnownGroupOut {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Plays in the app.
    pub plays: i64,
    /// The latest, unix ms.
    pub last_played_at: i64,
}

/// One build, ready to keep.
#[derive(Debug, Clone)]
pub(crate) struct Build {
    /// The build row.
    pub row: BuildRow,
    /// Whether a similar-artist answer between its seeds was missing: the
    /// groups before it are then kept, while there are any.
    pub links_missing: bool,
    /// The groups, largest share first; numbered in this order.
    pub groups: Vec<taste::Group>,
    /// Every known artist.
    pub artists: Vec<ArtistOut>,
    /// Every known release group.
    pub known: Vec<KnownGroupOut>,
}

/// What a rebuild did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kept {
    /// The rows were replaced.
    Written,
    /// Nothing written: the person forgot their history or their account
    /// went after the build began.
    Dropped,
}

/// Replace `user_id`'s taste rows with `build`, in one write that `complete`
/// also runs in. A build replaces all four tables, except that one missing
/// a similar-artist answer keeps the person's groups while they have any,
/// with the group of each seed still a seed, drops a group no seed is left
/// in, and replaces the rest. Nothing is written once a forget of theirs came
/// after `forgets_at` (their forget count when the build began), or once
/// their account is gone.
///
/// The taste groups are a computed result, rebuilt whole by one job: this
/// is the one writer of the four tables, and what it writes is never a read
/// model kept by triggers.
///
/// # Errors
///
/// The write's or `complete`'s failure.
pub(crate) async fn rebuild(
    db: &Db,
    forgets: &ForgetCounts,
    user_id: &str,
    forgets_at: u64,
    build: Build,
    complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
) -> Result<Kept, JobError> {
    let (forgets, user) = (forgets.clone(), user_id.to_owned());
    db.write(Lane::Background, "discover.taste.rebuild", move |tx| {
        let exists: bool = tx.query_row(sql::USER_EXISTS.text(), [&user], |row| row.get(0))?;
        if forgets.of(&user) != forgets_at || !exists {
            complete(tx)?;
            return Ok(Kept::Dropped);
        }
        let keep_groups = build.links_missing
            && tx.query_row(sql::TASTE_HAS_GROUPS.text(), [&user], |row| row.get(0))?;
        let previous: HashMap<String, i64> = if keep_groups {
            tx.prepare_cached(sql::TASTE_GROUP_NOS.text())?
                .query_map([&user], sql::TasteGroupNo::read)?
                .map(|row| row.map(|row| (row.artist_mbid_lower, row.group_no)))
                .collect::<rusqlite::Result<_>>()?
        } else {
            HashMap::new()
        };
        tx.execute(sql::TASTE_BUILD_DELETE.text(), [&user])?;
        tx.execute(sql::TASTE_ARTISTS_DELETE.text(), [&user])?;
        tx.execute(sql::KNOWN_GROUPS_DELETE.text(), [&user])?;
        if !keep_groups {
            tx.execute(sql::TASTE_GROUPS_DELETE.text(), [&user])?;
        }
        let row = &build.row;
        tx.execute(
            sql::TASTE_BUILD_INSERT.text(),
            params![
                user,
                Millis(row.built_at),
                row.plays_counted,
                row.imported_listens_counted,
                row.artists_played,
                row.seeds,
                row.history_from_at.map(Millis),
                row.history_to_at.map(Millis),
                row.incomplete,
            ],
        )?;
        if !keep_groups {
            let mut insert = tx.prepare_cached(sql::TASTE_GROUP_INSERT.text())?;
            for (no, group) in build.groups.iter().enumerate() {
                insert.execute(params![
                    user,
                    i64::try_from(no).unwrap_or(i64::MAX),
                    group.anchor,
                    group.anchor_name,
                    group.share,
                    group.recent_share,
                    group.trend,
                ])?;
            }
        }
        let mut insert = tx.prepare_cached(sql::TASTE_ARTIST_INSERT.text())?;
        for artist in &build.artists {
            let group_no = if keep_groups {
                artist
                    .is_seed
                    .then(|| previous.get(&artist.mbid).copied())
                    .flatten()
            } else {
                artist.group_no
            };
            insert.execute(params![
                user,
                artist.mbid,
                artist.name,
                artist.plays,
                artist.imported_listens,
                artist.months_active,
                artist.last_played_at.map(Millis),
                artist.weight,
                artist.is_seed,
                group_no,
            ])?;
        }
        if keep_groups {
            tx.execute(sql::TASTE_GROUPS_UNUSED_DELETE.text(), [&user])?;
        }
        let mut insert = tx.prepare_cached(sql::KNOWN_GROUP_INSERT.text())?;
        for group in &build.known {
            insert.execute(params![
                user,
                group.mbid,
                group.plays,
                Millis(group.last_played_at)
            ])?;
        }
        complete(tx)?;
        Ok(Kept::Written)
    })
    .await
}

/// `user_id`'s seeds from their taste build, heaviest first and taking
/// turns across their groups by weight ([`taste::spread`]); `None` while
/// they have no build.
///
/// # Errors
///
/// The read's failure.
pub(in crate::discover) async fn seeds(
    db: &Db,
    user_id: &str,
) -> Result<Option<Vec<Seed>>, DbError> {
    let user = user_id.to_owned();
    let rows: Vec<sql::TasteSeed> = db
        .read("discover.taste.seeds", move |conn| {
            conn.prepare_cached(sql::TASTE_SEEDS.text())?
                .query_map([&user], sql::TasteSeed::read)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DbError::from)
        })
        .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let seeds: Vec<(Seed, Option<i64>)> = rows
        .into_iter()
        .filter_map(|row| {
            let seed = Seed {
                artist: ArtistRow {
                    name: row.name.unwrap_or_default(),
                    mbid: Some(row.mbid?),
                    listen_count: row.plays.filter(|plays| *plays > 0),
                },
                weight: row.weight.unwrap_or(0.0),
                inputs: Vec::new(),
            };
            Some((seed, row.group_no))
        })
        .collect();
    let spread = taste::spread(seeds, |(seed, _)| seed.weight, |(_, group)| *group);
    Ok(Some(spread.into_iter().map(|(seed, _)| seed).collect()))
}

/// Forget `user_id`'s taste: their four tables' rows, in one write. Their
/// forget count moves on inside it, so a build of theirs that began before
/// it, and may have read the history the forget removed, keeps nothing.
/// Answers how many rows went.
///
/// # Errors
///
/// The write's failure.
pub(crate) async fn forget(
    db: &Db,
    forgets: &ForgetCounts,
    user_id: &str,
) -> Result<usize, DbError> {
    let (forgets, user) = (forgets.clone(), user_id.to_owned());
    db.write(Lane::Foreground, "discover.taste.forget", move |tx| {
        forgets.bump(&user);
        let mut removed = 0;
        for statement in [
            sql::TASTE_BUILD_DELETE,
            sql::TASTE_GROUPS_DELETE,
            sql::TASTE_ARTISTS_DELETE,
            sql::KNOWN_GROUPS_DELETE,
        ] {
            removed += tx.execute(statement.text(), [&user])?;
        }
        Ok::<_, DbError>(removed)
    })
    .await
}
