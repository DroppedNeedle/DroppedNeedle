//! One pick's evidence: what "Why this?" opens, assembled from stored rows
//! alone. The pick's own paths give its line, its artists and how it was
//! found; the person's taste rows, their tunings and the library say what
//! is true of it now. Nothing here asks a provider. A pick a weekly set
//! held keeps the evidence it was built with.

use std::collections::HashSet;

use droppedneedle_kernel::db::{Db, DbError};
use rusqlite::params;

use super::candidate::{Candidate, PathKind, PathSource};
use super::reason::{fact_path, reason, seed_paths};
use super::{pool_store, sql, weekly_store};
use crate::discover::models::{
    EvidenceSeed, EvidenceSource, FoundBy, NewToYou, PickEvidence, PickReason, PickReasonKind,
};
use crate::discover::tunings;

/// The service a path's fact came from, when it is one a pick names.
fn service(source: PathSource) -> Option<EvidenceSource> {
    match source {
        PathSource::ListenbrainzLabs | PathSource::Listenbrainz => {
            Some(EvidenceSource::Listenbrainz)
        }
        PathSource::Lastfm => Some(EvidenceSource::Lastfm),
        PathSource::Musicbrainz => Some(EvidenceSource::Musicbrainz),
        PathSource::Library => None,
    }
}

/// How `candidate` was found, from which service and when that fact was
/// read, for its `reason`.
fn found(
    candidate: &Candidate,
    reason: &PickReason,
) -> Option<(FoundBy, EvidenceSource, Option<i64>)> {
    let (found_by, path) = match reason.kind {
        PickReasonKind::YourArtists => (FoundBy::CoListening, *seed_paths(candidate).first()?),
        PickReasonKind::Fact => (FoundBy::BandLink, fact_path(candidate)?),
        PickReasonKind::Source => (
            FoundBy::SourcePick,
            candidate
                .paths
                .iter()
                .find(|path| path.kind == PathKind::Source)?,
        ),
        // Sound finds only what the library holds and a pool candidate
        // never is, so no candidate has a sound path.
        PickReasonKind::Sound => return None,
    };
    Some((found_by, service(path.source)?, path.fetched_at))
}

/// Reads one pick's evidence.
#[derive(Clone)]
pub struct Evidence {
    db: Db,
}

impl Evidence {
    /// Evidence read from `db`.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// The evidence behind `release_group` (lowercased) for `user_id`: as
    /// it was built for the newest of their weekly sets that held it, else
    /// as their pool holds it now. `None` when neither does, or the pool's
    /// has no reason that still holds.
    ///
    /// # Errors
    ///
    /// A read's failure.
    pub async fn of(
        &self,
        user_id: &str,
        release_group: &str,
    ) -> Result<Option<PickEvidence>, DbError> {
        let db = &self.db;
        if let Some(kept) = weekly_store::stored_evidence(db, user_id, release_group).await? {
            return Ok(Some(kept));
        }
        let Some(candidate) = pool_store::candidate(db, user_id, release_group).await? else {
            return Ok(None);
        };
        let seeds = pool_store::seed_ids(db, user_id).await?;
        self.of_candidate(user_id, &candidate, &seeds).await
    }

    /// The evidence behind `candidate` of `user_id`'s pool now, with
    /// `seeds` their seeds now (lowercased); `None` when it has no reason
    /// that still holds.
    ///
    /// # Errors
    ///
    /// A read's failure.
    pub(crate) async fn of_candidate(
        &self,
        user_id: &str,
        candidate: &Candidate,
        seeds: &HashSet<String>,
    ) -> Result<Option<PickEvidence>, DbError> {
        let db = &self.db;
        let Some(reason) = reason(candidate, seeds) else {
            return Ok(None);
        };
        let Some((found_by, source, source_read_at)) = found(candidate, &reason) else {
            return Ok(None);
        };
        let turned_down = tunings::less_from(db, user_id).await?;
        let evidence_seeds = seed_paths(candidate)
            .into_iter()
            .filter_map(|path| Some((path.artist.as_ref()?, path.link.as_ref()?)))
            .filter(|(artist, _)| seeds.contains(&artist.mbid))
            .map(|(artist, link)| EvidenceSeed {
                artist_mbid: artist.mbid.clone(),
                name: artist.name.clone(),
                your_plays: artist.plays,
                albums_owned: artist.albums_owned,
                rank: link.rank,
                of: link.of,
                turned_down: turned_down.contains(&artist.mbid),
            })
            .collect();
        let new_to_you = self.new_to_you(user_id, candidate).await?;
        Ok(Some(PickEvidence {
            release_group_mbid: candidate.release_group_mbid.clone(),
            title: candidate.title.clone(),
            artist_name: candidate.artist_name.clone(),
            reason,
            seeds: evidence_seeds,
            new_to_you,
            listeners: candidate.album_listeners,
            listeners_source: candidate
                .album_listeners
                .map(|_| EvidenceSource::Listenbrainz),
            found_by,
            source,
            source_read_at,
            sound_seed: None,
        }))
    }

    /// Whether `candidate`'s artist is new to `user_id`: no plays of it or
    /// the album in their taste rows, and no album of it in the library.
    async fn new_to_you(&self, user_id: &str, candidate: &Candidate) -> Result<NewToYou, DbError> {
        let (user, artist, group) = (
            user_id.to_owned(),
            candidate.artist_mbid.clone(),
            candidate.release_group_mbid.clone(),
        );
        let no_plays = self
            .db
            .read("discover.evidence.plays", move |conn| {
                conn.query_row(
                    sql::EVIDENCE_NO_PLAYS.text(),
                    params![user, artist, group],
                    |row| row.get(0),
                )
                .map_err(DbError::from)
            })
            .await?;
        let library =
            pool_store::library_albums(&self.db, std::slice::from_ref(&candidate.artist_mbid))
                .await?;
        let owned = pool_store::owned_among(
            &self.db,
            std::slice::from_ref(&candidate.release_group_mbid),
        )
        .await?;
        Ok(NewToYou {
            no_plays,
            not_in_library: library.values().all(|albums| *albums == 0) && owned.is_empty(),
        })
    }
}
