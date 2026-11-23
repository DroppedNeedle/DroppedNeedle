//! A person's candidate pool: what its build reads, the three tables it
//! writes, the pool read back with the gates that move applied, and
//! forgetting it.
//!
//! The pool is a result the pool job computes and writes whole, never a
//! read model kept by triggers: [`rebuild`] is the one writer, and it can be
//! thrown away and built again from its sources at any time.

use std::collections::{HashMap, HashSet};

use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::JobError;
use droppedneedle_library::identify::artist_links::placeholder;
use rusqlite::{Connection, Transaction, params};

use super::candidate::{Candidate, CandidateKind, Link, Path, PathArtist};
use super::rank;
use super::reason;
use super::select::Shown;
use super::sql;
use super::taste_store::Kept;
use crate::discover::cache::ForgetCounts;
use crate::discover::feedback::Feedback;
use crate::discover::tunings;

fn json(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_owned())
}

fn column(conn: &Connection, statement: &str, user: &str) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(statement)?
        .query_map([user], |row| row.get(0))?
        .collect()
}

/// What a person's taste build gives the pool.
pub(in crate::discover) struct Taste {
    /// The seeds, heaviest first.
    pub seeds: Vec<sql::PoolSeed>,
    /// The groups and their shares, by group number.
    pub groups: Vec<(i64, f64)>,
    /// Every artist the person's rows name, lowercased.
    pub known_artists: HashSet<String>,
    /// Every release group they played, lowercased.
    pub played: HashSet<String>,
}

/// `user_id`'s taste for the pool; `None` while they have no taste build.
/// A seed they asked for less from weighs what [`rank::tuned_weight`] says,
/// and the seeds stay heaviest first.
///
/// # Errors
///
/// A read's failure.
pub(in crate::discover) async fn taste(db: &Db, user_id: &str) -> Result<Option<Taste>, DbError> {
    let less_from = tunings::less_from(db, user_id).await?;
    let mut taste = taste_rows(db, user_id).await?;
    if let Some(taste) = taste.as_mut() {
        for seed in &mut taste.seeds {
            seed.weight = rank::tuned_weight(seed.weight, less_from.contains(&seed.mbid));
        }
        taste.seeds.sort_by(|a, b| {
            b.weight
                .total_cmp(&a.weight)
                .then_with(|| a.mbid.cmp(&b.mbid))
        });
    }
    Ok(taste)
}

/// `user_id`'s taste rows as kept.
async fn taste_rows(db: &Db, user_id: &str) -> Result<Option<Taste>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.pool.taste", move |conn| {
        // A background read whose statements must agree: the seeds, the
        // groups and the known sets are one taste build.
        let built: bool = conn.query_row(sql::POOL_HAS_TASTE.text(), [&user], |row| row.get(0))?;
        if !built {
            return Ok(None);
        }
        let seeds = conn
            .prepare_cached(sql::POOL_SEEDS.text())?
            .query_map([&user], sql::PoolSeed::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let groups = conn
            .prepare_cached(sql::POOL_GROUPS.text())?
            .query_map([&user], sql::PoolGroup::read)?
            .map(|row| row.map(|row| (row.group_no, row.share)))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let known_artists = column(conn, sql::POOL_KNOWN_ARTISTS.text(), &user)?
            .into_iter()
            .collect();
        let played = column(conn, sql::KNOWN_GROUPS.text(), &user)?
            .into_iter()
            .collect();
        Ok(Some(Taste {
            seeds,
            groups,
            known_artists,
            played,
        }))
    })
    .await
}

/// What the library holds and is fetching, as the pool's gates read it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Library {
    /// Every library artist with a MusicBrainz id, lowercased: the
    /// artists the library knows, held album or not.
    pub library_artists: HashSet<String>,
    /// Release groups the library is fetching, lowercased.
    pub fetching: HashSet<String>,
    /// How many albums the library holds of each of the asked artists.
    pub albums_owned: HashMap<String, i64>,
}

/// The library's side of the gates, and how many albums it holds of each
/// of `artists` (lowercased).
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn library(db: &Db, artists: &[String]) -> Result<Library, DbError> {
    let asked = json(artists);
    db.read("discover.pool.library", move |conn| {
        // A library background read: who it holds, what it fetches and the
        // seeds' albums are one moment of it.
        let all = |statement: &str| -> rusqlite::Result<HashSet<String>> {
            conn.prepare_cached(statement)?
                .query_map([], |row| row.get(0))?
                .collect()
        };
        // MusicBrainz's special-purpose ids never count.
        let library_artists = all(sql::POOL_LIBRARY_ARTISTS.text())?
            .into_iter()
            .filter(|mbid| !placeholder(mbid))
            .collect();
        let fetching = all(sql::POOL_FETCHING.text())?;
        let albums_owned = conn
            .prepare_cached(sql::POOL_ALBUMS_OWNED.text())?
            .query_map([&asked], sql::PoolAlbumsOwned::read)?
            .map(|row| row.map(|row| (row.mbid, row.albums)))
            .collect::<rusqlite::Result<_>>()?;
        Ok(Library {
            library_artists,
            fetching,
            albums_owned,
        })
    })
    .await
}

/// How many albums the library holds of each of `artists` (lowercased),
/// one entry an artist that has any.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn library_albums(
    db: &Db,
    artists: &[String],
) -> Result<HashMap<String, i64>, DbError> {
    let asked = json(artists);
    db.read("discover.pool.albums_owned", move |conn| {
        conn.prepare_cached(sql::POOL_ALBUMS_OWNED.text())?
            .query_map([asked], sql::PoolAlbumsOwned::read)?
            .map(|row| row.map(|row| (row.mbid, row.albums)))
            .collect::<rusqlite::Result<_>>()
            .map_err(DbError::from)
    })
    .await
}

/// The release groups among `groups` (lowercased) the library holds.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn owned_among(db: &Db, groups: &[String]) -> Result<HashSet<String>, DbError> {
    if groups.is_empty() {
        return Ok(HashSet::new());
    }
    let asked = json(groups);
    db.read("discover.pool.owned", move |conn| {
        conn.prepare_cached(sql::POOL_OWNED_AMONG.text())?
            .query_map([asked], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()
            .map_err(DbError::from)
    })
    .await
}

/// `user_id`'s picks shown at `since` or later, for memory.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn shown(db: &Db, user_id: &str, since: i64) -> Result<Vec<Shown>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.pool.shown", move |conn| {
        conn.prepare_cached(sql::POOL_SHOWN.text())?
            .query_map(params![user, Millis(since)], sql::PoolShown::read)?
            .map(|row| {
                row.map(|row| Shown {
                    release_group: row.release_group,
                    artist: row.artist_mbid,
                    last_shown_at: row.last_shown_at.0,
                    sampled: row.sampled,
                    requested: row.requested,
                })
            })
            .collect::<rusqlite::Result<_>>()
            .map_err(DbError::from)
    })
    .await
}

/// What the kept album lists say about one release group.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GroupFacts {
    /// The artist whose list holds it, lowercased.
    pub artist_mbid: String,
    /// Its primary type.
    pub primary_type: Option<String>,
    /// Its secondary types.
    pub secondary_types: Option<Vec<String>>,
    /// Its first release date.
    pub first_release_date: Option<String>,
    /// Its credited artists.
    pub credited_artist_mbids: Option<Vec<String>>,
    /// Its listeners.
    pub listeners: Option<i64>,
}

/// What the kept album lists say about each of `groups` (lowercased), by
/// release group, one entry a list holding it.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn group_facts(
    db: &Db,
    groups: &[String],
) -> Result<HashMap<String, Vec<GroupFacts>>, DbError> {
    let asked = json(groups);
    let rows = db
        .read("discover.pool.group_facts", move |conn| {
            conn.prepare_cached(sql::POOL_GROUP_FACTS.text())?
                .query_map([asked], sql::PoolGroupFacts::read)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DbError::from)
        })
        .await?;
    let list = |text: &str| serde_json::from_str::<Vec<String>>(text).ok();
    let mut facts: HashMap<String, Vec<GroupFacts>> = HashMap::new();
    for row in rows {
        facts
            .entry(row.release_group_mbid)
            .or_default()
            .push(GroupFacts {
                artist_mbid: row.artist_mbid,
                primary_type: row.primary_type,
                secondary_types: list(&row.secondary_types),
                first_release_date: row.first_release_date,
                credited_artist_mbids: row.credited_artist_mbids.as_deref().and_then(list),
                listeners: row.listeners,
            });
    }
    Ok(facts)
}

/// A path as stored.
fn path_of(row: sql::PoolPath) -> Path {
    let artist = row.artist_mbid.map(|mbid| PathArtist {
        mbid,
        name: row.artist_name.unwrap_or_default(),
        plays: row.artist_plays.unwrap_or(0),
        weight: row.artist_weight.unwrap_or(0.0),
        albums_owned: row.artist_albums_owned.unwrap_or(0),
    });
    let link = row.link_strength.map(|strength| Link {
        strength,
        rank: row.link_rank.unwrap_or(1),
        of: row.link_of.unwrap_or(1),
        labs: row.named_by_labs,
        lastfm: row.named_by_lastfm,
        closest_new: row.link_closest_new,
    });
    Path {
        kind: row.kind,
        source: row.source,
        fetched_at: row.fetched_at.map(|at| at.0),
        artist,
        link,
        known_credit: row.known_credit_mbid,
    }
}

/// A candidate as stored, with `paths`.
fn candidate_of(row: sql::PoolCandidate, paths: Vec<Path>) -> Candidate {
    Candidate {
        paths,
        credited_artist_mbids: serde_json::from_str(&row.credited_artist_mbids).unwrap_or_default(),
        release_group_mbid: row.release_group_mbid_lower,
        title: row.title,
        artist_mbid: row.artist_mbid,
        artist_name: row.artist_name,
        primary_type: row.primary_type,
        first_release_date: row.first_release_date,
        album_listeners: row.album_listeners,
        artist_listeners: row.artist_listeners,
        kind: row.kind,
        support: row.support,
        group: row.group_no,
        group_support: row.group_support,
    }
}

/// Every candidate of `user`'s pool, in its order, with its paths.
fn read_pool(conn: &Connection, user: &str) -> rusqlite::Result<Vec<Candidate>> {
    let rows = conn
        .prepare_cached(sql::POOL_CANDIDATES.text())?
        .query_map([user], sql::PoolCandidate::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut paths: HashMap<String, Vec<Path>> = HashMap::new();
    let path_rows = conn
        .prepare_cached(sql::POOL_PATHS.text())?
        .query_map([user], sql::PoolPath::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for row in path_rows {
        paths
            .entry(row.release_group_mbid_lower.clone())
            .or_default()
            .push(path_of(row));
    }
    Ok(rows
        .into_iter()
        .map(|row| {
            let own = paths
                .remove(&row.release_group_mbid_lower)
                .unwrap_or_default();
            candidate_of(row, own)
        })
        .collect())
}

/// One candidate of `user_id`'s pool, by its release group (lowercased),
/// with its paths; `None` when their pool holds no such album.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn candidate(
    db: &Db,
    user_id: &str,
    release_group: &str,
) -> Result<Option<Candidate>, DbError> {
    let (user, group) = (user_id.to_owned(), release_group.to_owned());
    db.read("discover.pool.candidate", move |conn| {
        // The row and its paths are read in one snapshot, so a pool
        // replaced between them never reads as pathless.
        let row = conn
            .prepare_cached(sql::POOL_CANDIDATE.text())?
            .query_map(params![user, group], sql::PoolCandidate::read)?
            .next()
            .transpose()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let paths = conn
            .prepare_cached(sql::POOL_CANDIDATE_PATHS.text())?
            .query_map(params![user, group], sql::PoolPath::read)?
            .map(|row| row.map(path_of))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(candidate_of(row, paths)))
    })
    .await
}

/// `user_id`'s seeds as their taste rows hold them now, lowercased.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn seed_ids(db: &Db, user_id: &str) -> Result<HashSet<String>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.pool.seed_ids", move |conn| {
        column(conn, sql::POOL_SEED_IDS.text(), &user)
            .map(|ids| ids.into_iter().collect())
            .map_err(DbError::from)
    })
    .await
}

/// `user_id`'s pool as last built, unfiltered: the rows a build may keep.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn previous(db: &Db, user_id: &str) -> Result<Vec<Candidate>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.pool.previous", move |conn| {
        // The candidates and their paths are read in one snapshot, so a
        // pool replaced between them never reads as pathless.
        read_pool(conn, &user).map_err(DbError::from)
    })
    .await
}

/// What a build counted, each where it arose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Seeds read.
    pub seeds_used: usize,
    /// Distinct artists the seeds point at, before any gate.
    pub neighbours_found: usize,
    /// Of those, the ones the person knows or the library holds.
    pub known_left_out: usize,
    /// The hubs left out; `None` when the rule did not run.
    pub hubs_left_out: Option<usize>,
    /// Artists in the ranking.
    pub artists_ranked: usize,
    /// Artists the walk found with no album left.
    pub artists_without_album: usize,
}

/// One build, ready to keep.
#[derive(Debug, Clone)]
pub(crate) struct PoolBuild {
    /// When it was built, unix ms.
    pub built_at: i64,
    /// Whether facts it needed were still to come.
    pub incomplete: bool,
    /// The artists whose album facts it waits on.
    pub waiting_for: Vec<String>,
    /// Its counts.
    pub counts: Counts,
    /// The pool, in order.
    pub candidates: Vec<Candidate>,
}

fn count(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn insert(tx: &Transaction, user: &str, build: &PoolBuild) -> rusqlite::Result<()> {
    let by_kind = |kind: CandidateKind| {
        count(
            build
                .candidates
                .iter()
                .filter(|candidate| candidate.kind == kind)
                .count(),
        )
    };
    let counts = &build.counts;
    tx.execute(
        sql::POOL_BUILD_INSERT.text(),
        params![
            user,
            Millis(build.built_at),
            build.incomplete,
            json(&build.waiting_for),
            count(counts.seeds_used),
            count(counts.neighbours_found),
            count(counts.known_left_out),
            counts.hubs_left_out.map(count),
            count(counts.artists_ranked),
            count(counts.artists_without_album),
            by_kind(CandidateKind::NewArtist),
            by_kind(CandidateKind::SourcePick),
            by_kind(CandidateKind::Gap),
            by_kind(CandidateKind::Shelf),
            by_kind(CandidateKind::Fresh),
            by_kind(CandidateKind::Thread),
        ],
    )?;
    let mut candidates = tx.prepare_cached(sql::POOL_CANDIDATE_INSERT.text())?;
    let mut paths = tx.prepare_cached(sql::POOL_PATH_INSERT.text())?;
    for (rank, candidate) in build.candidates.iter().enumerate() {
        candidates.execute(params![
            user,
            candidate.release_group_mbid,
            count(rank),
            candidate.kind,
            candidate.title,
            candidate.artist_mbid,
            candidate.artist_name,
            json(&candidate.credited_artist_mbids),
            candidate.primary_type,
            candidate.first_release_date,
            candidate.album_listeners,
            candidate.artist_listeners,
            candidate.support,
            candidate.group,
            candidate.group_support,
        ])?;
        for (no, path) in candidate.paths.iter().enumerate() {
            let artist = path.artist.as_ref();
            let link = path.link.as_ref();
            paths.execute(params![
                user,
                candidate.release_group_mbid,
                count(no),
                path.kind,
                path.source,
                path.fetched_at.map(Millis),
                artist.map(|artist| &artist.mbid),
                artist.map(|artist| &artist.name),
                artist.map(|artist| artist.plays),
                artist.map(|artist| artist.weight),
                artist.map(|artist| artist.albums_owned),
                link.map(|link| link.strength),
                link.map(|link| link.rank),
                link.map(|link| link.of),
                link.is_some_and(|link| link.labs),
                link.is_some_and(|link| link.lastfm),
                path.known_credit,
                link.is_some_and(|link| link.closest_new),
            ])?;
        }
    }
    Ok(())
}

fn delete(tx: &Transaction, user: &str) -> rusqlite::Result<usize> {
    let mut removed = 0;
    for statement in [
        sql::POOL_PATHS_DELETE,
        sql::POOL_CANDIDATES_DELETE,
        sql::POOL_BUILD_DELETE,
    ] {
        removed += tx.execute(statement.text(), [user])?;
    }
    Ok(removed)
}

/// Replace `user_id`'s pool with `build`, or with nothing when there is no
/// build (they have no taste yet), in one write that `complete` also runs
/// in. Nothing is written once a forget of theirs came after `forgets_at`
/// (their forget count when the build began), or once their account is
/// gone.
///
/// The pool is a computed result, rebuilt whole by one job: this is the one
/// writer of its three tables, and what it writes is never a read model
/// kept by triggers.
///
/// # Errors
///
/// The write's or `complete`'s failure.
pub(crate) async fn rebuild(
    db: &Db,
    forgets: &ForgetCounts,
    user_id: &str,
    forgets_at: u64,
    build: Option<PoolBuild>,
    complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
) -> Result<Kept, JobError> {
    let (forgets, user) = (forgets.clone(), user_id.to_owned());
    db.write(Lane::Background, "discover.pool.rebuild", move |tx| {
        let exists: bool = tx.query_row(sql::USER_EXISTS.text(), [&user], |row| row.get(0))?;
        if forgets.of(&user) != forgets_at || !exists {
            complete(tx)?;
            return Ok(Kept::Dropped);
        }
        delete(tx, &user)?;
        if let Some(build) = &build {
            insert(tx, &user, build)?;
        }
        complete(tx)?;
        Ok(Kept::Written)
    })
    .await
}

/// `user_id`'s pool at `now_ms`, in its order, with the gates that move
/// applied as it is read: an album the library now holds or is fetching,
/// one the person hid or whose artist they turned down, and one with no
/// reason that still holds ([`reason::reason`]: an artist it would name is
/// no longer one of their seeds), is left out.
///
/// # Errors
///
/// A read's failure; what the person told Discover not reading is one.
pub async fn ranked(db: &Db, user_id: &str, now_ms: i64) -> Result<Vec<Candidate>, DbError> {
    let user = user_id.to_owned();
    let pool = db
        .read("discover.pool.ranked", move |conn| {
            // One snapshot, as for the previous rows.
            read_pool(conn, &user).map_err(DbError::from)
        })
        .await?;
    if pool.is_empty() {
        return Ok(pool);
    }
    let groups: Vec<String> = pool
        .iter()
        .map(|candidate| candidate.release_group_mbid.clone())
        .collect();
    let owned = owned_among(db, &groups).await?;
    let fetching: HashSet<String> = db
        .read("discover.pool.fetching", |conn| {
            conn.prepare_cached(sql::POOL_FETCHING.text())?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()
                .map_err(DbError::from)
        })
        .await?;
    let feedback = Feedback::load(db, user_id, now_ms).await?;
    let seeds = seed_ids(db, user_id).await?;
    Ok(pool
        .into_iter()
        .filter(|candidate| {
            let group = candidate.release_group_mbid.as_str();
            !owned.contains(group)
                && !fetching.contains(group)
                && !feedback.hides_album(Some(group), Some(&candidate.artist_mbid))
                && reason::reason(candidate, &seeds).is_some()
        })
        .collect())
}

/// Forget `user_id`'s pool: its three tables' rows, in one write. Their
/// forget count moves on inside it, so a build of theirs that began before
/// it keeps nothing. Answers how many rows went.
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
    db.write(Lane::Foreground, "discover.pool.forget", move |tx| {
        forgets.bump(&user);
        Ok::<_, DbError>(delete(tx, &user)?)
    })
    .await
}
