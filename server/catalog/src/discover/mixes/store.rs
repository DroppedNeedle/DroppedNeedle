//! What the mixes job reads, the one write that keeps a person's mixes,
//! the mixes read back for the page, and forgetting them.
//!
//! A person's mixes are a result the mixes job computes and writes whole,
//! never a read model kept by triggers: [`keep`] is the one writer of the
//! two tables, and they can be thrown away and built again at any time.

use std::collections::{HashMap, HashSet};

use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::JobError;
use rusqlite::{Transaction, params};

use super::build::{BuiltMix, Picked};
use crate::discover::cache::ForgetCounts;
use crate::discover::engine::taste_store::Kept;
use crate::discover::models::{DailyMix, MixTrack, MixTrackKind, PickReason, PickReasonArtist};
use crate::discover::page::shelves::album;
use crate::discover::tunings::TuningKind;
use crate::discover::{read_all, sql};

/// One of a person's taste groups a mix may be built for.
#[derive(Debug, Clone)]
pub(crate) struct Group {
    /// Its number in the taste build.
    pub group_no: i64,
    /// Its anchor artist, lowercased.
    pub anchor: String,
    /// Its share of the person's seed weight.
    pub share: f64,
    /// Its artists, most played first.
    pub artists: Vec<PickReasonArtist>,
}

/// What a person said about their tastes that the mixes follow.
#[derive(Debug, Clone, Default)]
pub(crate) struct Said {
    /// The anchors of the groups they said were a phase.
    pub phase_groups: HashSet<String>,
    /// The anchors of the groups they asked for less from.
    pub less_groups: HashSet<String>,
    /// The artists they said were a phase, lowercased: their plays count
    /// for nothing.
    pub phase_artists: HashSet<String>,
}

/// `user_id`'s taste groups that have a share, largest first, each with its
/// artists, most played first with a phase artist's plays counting for
/// nothing; and what they said about them.
///
/// # Errors
///
/// A read's failure.
pub(crate) async fn groups(db: &Db, user_id: &str) -> Result<(Vec<Group>, Said), DbError> {
    let user = user_id.to_owned();
    let (rows, artists, said) = db
        .read("discover.mixes.groups", move |conn| {
            // One snapshot: the groups, their artists and the statements
            // about them must come from one taste build.
            let rows = conn
                .prepare_cached(sql::MIX_GROUPS.text())?
                .query_map([&user], sql::MixGroup::read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let artists = conn
                .prepare_cached(sql::MIX_GROUP_ARTISTS.text())?
                .query_map([&user], sql::MixGroupArtist::read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let targets = |statement: &str, kind: TuningKind| {
                conn.prepare_cached(statement)?
                    .query_map(rusqlite::params![user, kind], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<HashSet<String>>>()
            };
            let said = Said {
                phase_groups: targets(sql::MIX_GROUP_STATEMENTS.text(), TuningKind::Phase)?,
                less_groups: targets(sql::MIX_GROUP_STATEMENTS.text(), TuningKind::LessFromSeed)?,
                phase_artists: targets(sql::MIX_STATED_ARTISTS.text(), TuningKind::Phase)?,
            };
            Ok::<_, DbError>((rows, artists, said))
        })
        .await?;
    let mut by_group: HashMap<i64, Vec<(i64, PickReasonArtist)>> = HashMap::new();
    for row in artists {
        let played = if said.phase_artists.contains(&row.artist_mbid_lower) {
            0
        } else {
            row.played
        };
        by_group.entry(row.group_no).or_default().push((
            played,
            PickReasonArtist {
                artist_mbid: row.artist_mbid_lower,
                name: row.name,
            },
        ));
    }
    let groups = rows
        .into_iter()
        .map(|row| {
            let mut artists = by_group.remove(&row.group_no).unwrap_or_default();
            artists
                .sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.artist_mbid.cmp(&y.artist_mbid)));
            Group {
                artists: artists.into_iter().map(|(_, artist)| artist).collect(),
                group_no: row.group_no,
                anchor: row.anchor_mbid,
                share: row.share,
            }
        })
        .collect();
    Ok((groups, said))
}

/// Whether `user_id`'s pool was built since their taste build, so its
/// albums stand on the taste the mixes are built from.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn pool_current(db: &Db, user_id: &str) -> Result<bool, DbError> {
    let user = user_id.to_owned();
    let builds = db
        .read("discover.mixes.builds", move |conn| {
            conn.query_row(sql::MIX_BUILDS.text(), [&user], sql::MixBuilds::read)
                .map_err(DbError::from)
        })
        .await?;
    Ok(match (builds.taste_built_at, builds.pool_built_at) {
        (Some(taste), Some(pool)) => pool.0 >= taste.0,
        _ => false,
    })
}

/// Whether `user_id` has a taste group a mix could be built for.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn has_groups(db: &Db, user_id: &str) -> Result<bool, DbError> {
    let user = user_id.to_owned();
    db.read("discover.mixes.has_groups", move |conn| {
        conn.query_row(sql::MIX_HAS_GROUPS.text(), [&user], |row| row.get(0))
            .map_err(DbError::from)
    })
    .await
}

fn json(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_owned())
}

/// The owned tracks whose track or album artist is one of `artists`
/// (lowercased).
///
/// # Errors
///
/// The read's failure.
pub(in crate::discover) async fn artist_tracks(
    db: &Db,
    artists: &[String],
) -> Result<Vec<sql::MixLibraryTrack>, DbError> {
    if artists.is_empty() {
        return Ok(Vec::new());
    }
    read_all(
        db,
        "discover.mixes.artist_tracks",
        sql::MIX_ARTIST_TRACKS.text(),
        (json(artists),),
        sql::MixLibraryTrack::read,
    )
    .await
}

/// The owned tracks among `ids`.
///
/// # Errors
///
/// The read's failure.
pub(in crate::discover) async fn tracks_by_id(
    db: &Db,
    ids: &[String],
) -> Result<Vec<sql::MixLibraryTrack>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    read_all(
        db,
        "discover.mixes.tracks_by_id",
        sql::MIX_TRACKS_BY_ID.text(),
        (json(ids),),
        sql::MixLibraryTrack::read,
    )
    .await
}

/// `user_id`'s plays of library tracks since `since_ms`: each track and
/// when it was played.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn plays(
    db: &Db,
    user_id: &str,
    since_ms: i64,
) -> Result<Vec<(String, i64)>, DbError> {
    Ok(read_all(
        db,
        "discover.mixes.plays",
        sql::MIX_PLAYS.text(),
        (user_id.to_owned(), since_ms),
        sql::MixPlay::read,
    )
    .await?
    .into_iter()
    .map(|play| (play.local_track_id, play.played_at.0))
    .collect())
}

/// The id of `user_id`'s mix at place `mix_no`: the same from day to day.
fn mix_id(user_id: &str, mix_no: usize) -> String {
    format!("{user_id}:{mix_no}")
}

fn delete(tx: &Transaction, user: &str) -> rusqlite::Result<usize> {
    Ok(tx.execute(sql::MIX_TRACKS_DELETE.text(), [user])?
        + tx.execute(sql::MIXES_DELETE.text(), [user])?)
}

fn insert(
    tx: &Transaction,
    user: &str,
    day: &str,
    built_at: i64,
    mixes: &[BuiltMix],
) -> rusqlite::Result<()> {
    let mut mix_row = tx.prepare_cached(sql::MIX_INSERT.text())?;
    let mut track_row = tx.prepare_cached(sql::MIX_TRACK_INSERT.text())?;
    for (mix_no, mix) in mixes.iter().enumerate() {
        let id = mix_id(user, mix_no);
        let named = serde_json::to_string(&mix.named).unwrap_or_else(|_| "[]".to_owned());
        mix_row.execute(params![
            id,
            user,
            i64::try_from(mix_no).unwrap_or(i64::MAX),
            day,
            built_at,
            mix.name,
            mix.anchor,
            named,
            i64::try_from(mix.yours_count()).unwrap_or(i64::MAX),
            i64::try_from(mix.new_count()).unwrap_or(i64::MAX),
        ])?;
        for (position, track) in mix.tracks.iter().enumerate() {
            let position = i64::try_from(position).unwrap_or(i64::MAX);
            match track {
                Picked::Library(kind, track) => track_row.execute(params![
                    id,
                    position,
                    kind,
                    track.id,
                    track.recording_mbid,
                    track.release_group_mbid,
                    track.title,
                    track.artist_name,
                    track.artist_mbid,
                    track.album_title,
                    None::<String>,
                ])?,
                Picked::New(track) => track_row.execute(params![
                    id,
                    position,
                    MixTrackKind::New,
                    None::<String>,
                    track.recording_mbid,
                    track.release_group_mbid,
                    track.title,
                    track.artist_name,
                    track.artist_mbid,
                    track.album_title,
                    serde_json::to_string(&track.reason).unwrap_or_else(|_| "{}".to_owned()),
                ])?,
            };
        }
    }
    Ok(())
}

/// Replace `user_id`'s mixes with `mixes`, built on `day` at `built_at`,
/// in one write that `complete` also runs in. Nothing is written once a
/// forget of theirs came after `forgets_at` (their forget count when the
/// build began), or once their account is gone.
///
/// The mixes are a computed result, rebuilt whole once a day by one job:
/// this is the one writer of their two tables, and what it writes is never
/// a read model kept by triggers.
///
/// # Errors
///
/// The write's or `complete`'s failure.
pub(crate) async fn keep(
    db: &Db,
    forgets: &ForgetCounts,
    (user_id, forgets_at): (&str, u64),
    (day, built_at): (&str, i64),
    mixes: Vec<BuiltMix>,
    complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
) -> Result<Kept, JobError> {
    let (forgets, user, day) = (forgets.clone(), user_id.to_owned(), day.to_owned());
    db.write(Lane::Background, "discover.mixes.keep", move |tx| {
        let exists: bool = tx.query_row(sql::MIX_USER_EXISTS.text(), [&user], |row| row.get(0))?;
        if forgets.of(&user) != forgets_at || !exists {
            complete(tx)?;
            return Ok(Kept::Dropped);
        }
        delete(tx, &user)?;
        insert(tx, &user, &day, built_at, &mixes)?;
        complete(tx)?;
        Ok(Kept::Written)
    })
    .await
}

/// `user_id`'s mixes as kept, in page order. A library track that has left
/// the library since is left out, and the mix's counts follow its rows.
///
/// # Errors
///
/// A read's failure.
pub async fn kept(db: &Db, user_id: &str) -> Result<Vec<DailyMix>, DbError> {
    let user = user_id.to_owned();
    let (mixes, tracks) = db
        .read("discover.mixes.kept", move |conn| {
            // One snapshot: the tracks must be the mixes' own.
            let mixes = conn
                .prepare_cached(sql::MIXES_KEPT.text())?
                .query_map([&user], sql::KeptMix::read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let tracks = conn
                .prepare_cached(sql::MIX_TRACKS_KEPT.text())?
                .query_map([&user], sql::KeptMixTrack::read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, DbError>((mixes, tracks))
        })
        .await?;
    let mut by_mix: HashMap<String, Vec<MixTrack>> = HashMap::new();
    for row in tracks {
        let Some(track) = track_of(row.kind != MixTrackKind::New, &row) else {
            continue;
        };
        by_mix.entry(row.mix_id.clone()).or_default().push(track);
    }
    Ok(mixes
        .into_iter()
        .filter_map(|row| {
            let tracks = by_mix.remove(&row.id).unwrap_or_default();
            if tracks.is_empty() {
                return None;
            }
            let artists: Vec<PickReasonArtist> = serde_json::from_str(&row.named_artists)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "a kept mix's named artists do not read");
                    Vec::new()
                });
            let mut mix = DailyMix {
                id: row.id,
                name: row.name,
                artists,
                day: row.day,
                yours: 0,
                new: 0,
                tracks,
            };
            recount(&mut mix);
            Some(mix)
        })
        .collect())
}

/// Name `mix` again after some of its tracks or named artists left: the
/// named artists still on it, else the first two artists on it, by
/// [`title`](super::build::title).
pub(crate) fn rename(mix: &mut DailyMix) {
    let mut on_it: Vec<PickReasonArtist> = Vec::new();
    for track in &mix.tracks {
        if let Some(artist) = &track.artist_mbid
            && !on_it.iter().any(|known| &known.artist_mbid == artist)
        {
            on_it.push(PickReasonArtist {
                artist_mbid: artist.clone(),
                name: track.artist_name.clone(),
            });
        }
    }
    let mut named: Vec<PickReasonArtist> = mix
        .artists
        .iter()
        .filter(|artist| {
            on_it
                .iter()
                .any(|known| known.artist_mbid == artist.artist_mbid)
        })
        .cloned()
        .collect();
    if named.is_empty() {
        named = on_it.iter().take(2).cloned().collect();
    }
    named.truncate(2);
    mix.name = super::build::title(&named, on_it.len() > named.len());
    mix.artists = named;
}

/// Set `mix`'s counts from its tracks.
pub(crate) fn recount(mix: &mut DailyMix) {
    let new = mix
        .tracks
        .iter()
        .filter(|track| track.kind == MixTrackKind::New)
        .count();
    mix.new = u32::try_from(new).unwrap_or(u32::MAX);
    mix.yours = u32::try_from(mix.tracks.len() - new).unwrap_or(u32::MAX);
}

/// One kept track as the page shows it; `None` for a library track no
/// longer in the library, or a new one whose reason does not read.
fn track_of(library: bool, row: &sql::KeptMixTrack) -> Option<MixTrack> {
    if library && !row.present {
        return None;
    }
    let reason: Option<PickReason> = match (&row.reason, library) {
        (Some(reason), false) => match serde_json::from_str(reason) {
            Ok(reason) => Some(reason),
            Err(error) => {
                tracing::warn!(%error, "a kept mix track's reason does not read; left out");
                return None;
            }
        },
        _ => None,
    };
    let mut on = album(
        row.release_group_mbid.as_deref(),
        row.album_title.as_deref().unwrap_or(""),
        Some(&row.artist_name),
        row.artist_mbid.as_deref(),
        None,
    );
    if library {
        on.local_id = row.album_id.clone();
        on.in_library = true;
    }
    Some(MixTrack {
        kind: row.kind,
        title: row.title.clone(),
        artist_name: row.artist_name.clone(),
        artist_mbid: row.artist_mbid.clone(),
        recording_mbid: row.recording_mbid.clone(),
        local_track_id: row.local_track_id.clone().filter(|_| library),
        file_format: row
            .file_format
            .as_ref()
            .filter(|_| library)
            .map(|format| format.to_ascii_lowercase()),
        duration_seconds: row
            .duration_seconds
            .filter(|_| library)
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(|seconds| seconds.round().min(f64::from(u32::MAX)) as u32),
        album: on,
        reason,
    })
}

/// Forget `user_id`'s mixes, in one write. Their forget count moves on
/// inside it, so a build of theirs that began before it keeps nothing.
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
    db.write(Lane::Foreground, "discover.mixes.forget", move |tx| {
        forgets.bump(&user);
        Ok::<_, DbError>(delete(tx, &user)?)
    })
    .await
}
