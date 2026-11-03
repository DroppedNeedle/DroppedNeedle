//! Merge duplicate local artists into one survivor.
//!
//! Every reference to a merged artist (album and track credits, album
//! artists, favorites, ratings, play history, credit proofs, links and
//! source links) moves to the survivor; the merged rows stay as retired
//! records with an alias, so old links keep resolving. This is
//! not undoable. Every merge, chosen or automatic, retires a row only
//! through [`retire_artist`], with the [`MergeBasis`] it rests on.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::types::Value;
use rusqlite::{OptionalExtension as _, Transaction, params, params_from_iter};
use serde::Serialize;

use super::merge_basis::MergeBasis;
use super::models::{
    ArtistAliasKind, ArtistMergeOutcome, ArtistMergeRequest, CorrectionError, MergeCandidateState,
    ProviderChoice,
};
use super::reasons;
use super::sql::{self, ReferenceKind};
use crate::identify::sqlite::retarget_references;
use droppedneedle_auth::Admin;
use droppedneedle_kernel::db::statement::ListLen;

const VARIOUS_ARTISTS_ID: &str = "00000000-0000-4000-8000-000000000001";
const UNKNOWN_ARTIST_ID: &str = "00000000-0000-4000-8000-000000000002";

/// Reference kinds a merge moves, counted for the preview.
const REFERENCE_KINDS: [(&str, ReferenceKind); 6] = [
    ("album_credits", ReferenceKind::AlbumCredits),
    ("track_credits", ReferenceKind::TrackCredits),
    ("primary_albums", ReferenceKind::PrimaryAlbums),
    ("favorites", ReferenceKind::Favorites),
    ("ratings", ReferenceKind::Ratings),
    ("history", ReferenceKind::History),
];

/// A merge as run: what it did and the material its token covers.
pub(super) struct ArtistRun {
    pub outcome: ArtistMergeOutcome,
    pub material: String,
    /// What the merge rests on, for the audit row.
    pub basis: serde_json::Value,
}

#[derive(Serialize)]
struct Material<'a> {
    kind: &'static str,
    surviving_artist_id: &'a str,
    retired: &'a [String],
    provider_choice: ProviderChoice,
    revisions: Vec<(String, i64)>,
    identities: Vec<(String, String)>,
}

pub(super) fn merge(
    tx: &Transaction<'_>,
    admin: &Admin,
    request: &ArtistMergeRequest,
    now: f64,
) -> Result<ArtistRun, CorrectionError> {
    let survivor = request.surviving_artist_id.trim().to_owned();
    let retired: Vec<String> = request
        .source_artist_ids
        .iter()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && *id != survivor)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if retired.is_empty() {
        return Err(CorrectionError::Invalid(reasons::NO_DUPLICATE_ARTIST));
    }
    if retired
        .iter()
        .any(|id| id == VARIOUS_ARTISTS_ID || id == UNKNOWN_ARTIST_ID)
    {
        return Err(CorrectionError::Invalid(reasons::RESERVED_ARTIST));
    }
    let all: Vec<String> = std::iter::once(survivor.clone())
        .chain(retired.iter().cloned())
        .collect();
    let mut revisions = Vec::with_capacity(all.len());
    for id in &all {
        let revision: i64 = tx
            .query_row(
                crate::corrections::sql::LIVE_ARTIST_REVISION.text(),
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(CorrectionError::NotFound(reasons::ARTIST_NOT_FOUND))?;
        if let Some(expected) = request.expected_revisions.get(id)
            && *expected > 0
            && *expected != revision
        {
            return Err(CorrectionError::Conflict(reasons::REVISION_STALE));
        }
        revisions.push((id.clone(), revision));
    }
    let identities = identities(tx, &all)?;
    let distinct: BTreeSet<&str> = identities.iter().map(|(_, mbid)| mbid.as_str()).collect();
    let identity_conflicts: Vec<String> = if distinct.len() > 1 {
        distinct.iter().map(|mbid| (*mbid).to_owned()).collect()
    } else {
        Vec::new()
    };
    let reference_counts = reference_counts(tx, &all)?;

    // The survivor's MusicBrainz link: kept, taken from the only merged
    // artist that has one, or dropped.
    let survivor_link = identities.iter().find(|(id, _)| *id == survivor).cloned();
    let carried = match (request.provider_choice, &survivor_link) {
        (ProviderChoice::RetainSurvivor, None) if distinct.len() == 1 => {
            identities.first().cloned()
        }
        _ => None,
    };
    let carried_row = match &carried {
        Some((from, _)) => Some(identity_row(tx, from)?),
        None => None,
    };
    let mut basis = serde_json::Value::Null;
    for id in &retired {
        let chosen = MergeBasis::person(tx, admin, &survivor, id)?
            .ok_or(CorrectionError::NotFound(reasons::ARTIST_NOT_FOUND))?;
        basis = chosen.audit();
        retire_artist(tx, chosen, now)?;
    }
    match request.provider_choice {
        ProviderChoice::Detach => {
            tx.execute(
                crate::corrections::sql::DROP_ARTIST_LINKS.text(),
                params![survivor],
            )?;
        }
        ProviderChoice::RetainSurvivor => {
            if let Some(row) = carried_row.flatten() {
                tx.execute(
                    crate::corrections::sql::WRITE_MERGED_ARTIST_LINK.text(),
                    params![survivor, row.0, row.1, crate::clock::ms(now)],
                )?;
            }
        }
    }
    tx.execute(
        crate::corrections::sql::BUMP_ARTIST_REVISION.text(),
        params![survivor, crate::clock::ms(now)],
    )?;
    let material = serde_json::to_string(&Material {
        kind: "artist_merge",
        surviving_artist_id: &survivor,
        retired: &retired,
        provider_choice: request.provider_choice,
        revisions,
        identities: identities.clone(),
    })
    .unwrap_or_default();
    Ok(ArtistRun {
        outcome: ArtistMergeOutcome {
            surviving_artist_id: survivor,
            retired_artist_ids: retired,
            identity_conflicts,
            reference_counts,
        },
        material,
        basis,
    })
}

/// (artist id, MusicBrainz artist id) for the artists that have a link.
fn identities(tx: &Transaction<'_>, ids: &[String]) -> rusqlite::Result<Vec<(String, String)>> {
    tx.prepare(&crate::corrections::sql::artist_links_of(ListLen::new(
        ids.len(),
    )))?
    .query_map(params_from_iter(ids.iter()), |row| {
        let row = sql::ArtistLink::read(row)?;
        Ok((row.local_artist_id, row.provider_artist_id))
    })?
    .collect()
}

/// The link row a carried identity copies: (MBID, decision source).
fn identity_row(
    tx: &Transaction<'_>,
    artist_id: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    tx.query_row(
        crate::corrections::sql::IDENTITY_ROW.text(),
        params![artist_id],
        |row| {
            let row = sql::IdentityRow::read(row)?;
            Ok((row.provider_artist_id, row.decision_source))
        },
    )
    .optional()
}

fn reference_counts(
    tx: &Transaction<'_>,
    ids: &[String],
) -> rusqlite::Result<BTreeMap<String, i64>> {
    let mut counts = BTreeMap::new();
    for (label, kind) in REFERENCE_KINDS {
        let mut total = 0;
        let mut stmt = tx.prepare(&sql::references_of(kind, ListLen::new(ids.len())))?;
        let rows = stmt.query_map(params_from_iter(ids.iter()), |row| {
            Ok(sql::ReferenceCount::read(row)?.count)
        })?;
        for count in rows {
            total += count?;
        }
        counts.insert(label.to_owned(), total);
    }
    Ok(counts)
}

/// The reason of an open merge candidate that records a row waiting to
/// follow its proven credits to another row (`artist_convergence`).
pub(crate) const CREDITS_MOVED: &str = "CREDITS_MOVED_TO_PROVEN_ROW";

/// The reason of an open merge candidate recording that a scan moved a
/// credit from its left row to its right one by name alone: no id in the
/// file on either side and no proof at the old credit.
pub(crate) const MOVED_BY_NAME: &str = "CREDITS_MOVED_BY_NAME";

/// As [`MOVED_BY_NAME`], for a move where the file held an id on either
/// side or a proof sat at the old credit.
pub(crate) const MOVED_WITH_ID: &str = "CREDITS_MOVED_WITH_ID";

/// The audit reason of a row merged into the one a retag moved all its
/// credits to, where the two names differ only in sort name, capitals or
/// spacing (`artist_convergence`).
pub(crate) const MERGED_BY_NAME: &str = "AUTOMATIC_SAME_NAME_CREDITS_MOVED";

/// The audit reason of a v2 "Unknown Artist" row the upgrade merged into
/// the built-in one (`artist_convergence`).
pub(crate) const V2_PLACEHOLDER_FOLDED: &str = "AUTOMATIC_V2_PLACEHOLDER_FOLDED";

/// Point every reference of the basis's retiree at its survivor and
/// retire it, recording in `converged_mbid` what proved the merge. The
/// only writer of `local_artists.retired_into_artist_id`.
pub(super) fn retire_artist(
    tx: &Transaction<'_>,
    basis: MergeBasis<'_>,
    now: f64,
) -> rusqlite::Result<()> {
    let artist = basis.retiree();
    let survivor = basis.survivor();
    // A row waiting to follow its credits to `artist`, or whose credits a
    // scan moved to it, follows them on to the survivor; every other open
    // candidate naming `artist` is resolved below.
    repoint_merge_candidates(tx, artist, survivor, now)?;
    let moves = [
        sql::REPOINT_ALBUM_CREDITS,
        sql::REPOINT_TRACK_CREDITS,
        sql::REPOINT_ALBUM_ARTIST,
        sql::DROP_SHARED_SOURCE_LINKS,
        sql::REPOINT_SOURCE_LINKS,
        sql::DROP_ARTIST_LINK,
        sql::RETIRE_ARTIST_ROW,
        sql::REPOINT_ARTIST_ALIASES,
        sql::REPOINT_CREDIT_PROOFS,
    ];
    // Favorites (with their names), ratings and plays, the same way
    // identify merges move them.
    retarget_references(tx, "artist", artist, survivor)?;
    let mark = basis
        .converged_mark()
        .map_or(Value::Null, |mark| Value::Text(mark.to_owned()));
    for statement in moves {
        let mut stmt = tx.prepare(statement.text())?;
        let values = [
            Value::Text(artist.to_owned()),
            Value::Text(survivor.to_owned()),
            Value::Real(now),
            mark.clone(),
        ];
        let wanted = stmt.parameter_count();
        stmt.execute(params_from_iter(values.into_iter().take(wanted)))?;
    }
    tx.execute(
        sql::ALIAS_RETIRED_ARTIST.text(),
        params![artist, survivor, now, ArtistAliasKind::MergedArtist],
    )?;
    resolve_merge_candidates(tx, artist, now)?;
    Ok(())
}

/// Close every open candidate naming `artist`: it was merged.
fn resolve_merge_candidates(
    tx: &Transaction<'_>,
    artist: &str,
    now: f64,
) -> rusqlite::Result<usize> {
    tx.execute(
        sql::RESOLVE_MERGE_CANDIDATES.text(),
        params![
            artist,
            now,
            MergeCandidateState::Resolved,
            MergeCandidateState::Open
        ],
    )
}

fn repoint_merge_candidates(
    tx: &Transaction<'_>,
    artist: &str,
    survivor: &str,
    now: f64,
) -> rusqlite::Result<usize> {
    tx.prepare_cached(crate::corrections::sql::REPOINT_MERGE_CANDIDATES.text())?
        .execute(params![
            artist,
            survivor,
            crate::clock::ms(now),
            CREDITS_MOVED,
            MOVED_BY_NAME,
            MOVED_WITH_ID,
            MergeCandidateState::Open,
        ])
}

/// Move on what landed on `artist` after it was retired into `survivor`:
/// favorites, ratings, plays, aliases, candidates and credits under exactly
/// `name` with no id. Links, source links and any other credit stay on the
/// retired row, since they may be another artist's. Answers how many rows
/// moved.
pub(super) fn retarget_retired(
    tx: &Transaction<'_>,
    artist: &str,
    survivor: &str,
    name: &str,
    now: f64,
) -> rusqlite::Result<u64> {
    let mut moved = repoint_merge_candidates(tx, artist, survivor, now)? as u64;
    moved += retarget_references(tx, "artist", artist, survivor)?;
    moved += tx.execute(
        sql::REPOINT_ARTIST_ALIASES.text(),
        params![artist, survivor],
    )? as u64;
    moved += resolve_merge_candidates(tx, artist, now)? as u64;
    for credits in [sql::RETARGET_ALBUM_CREDITS, sql::RETARGET_TRACK_CREDITS] {
        moved += tx.execute(credits.text(), params![artist, survivor, name])? as u64;
    }
    moved += tx.execute(
        sql::RETARGET_ALBUM_ARTIST.text(),
        params![artist, survivor, now, name],
    )? as u64;
    Ok(moved)
}
