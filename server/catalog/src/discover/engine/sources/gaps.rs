//! Gaps in the artists a person collects: albums missing from the artists
//! the library holds [`MIN_ALBUMS`] or more albums of, among those the
//! person played or signalled, the [`ARTISTS`] heaviest in their taste.
//! Each artist's kept album list gives up to [`PER_ARTIST`] `gap`
//! candidates, most listened first, through the album gate
//! ([`gate::may_offer`]: an album or EP, no live set, demo, compilation or
//! the rest) and the pool's gates (not owned, not fetching, not played, not
//! turned down). Each has a `library` path holding the artist and how many
//! of their albums the library holds. A list not kept yet is wanted for the
//! kept facts' refresh, as the pool's walk wants its lists; nothing is
//! asked of a provider here.
//!
//! The section ([`section`]) reads them back from the person's pool through
//! [`pool_store::ranked`], so the gates that move apply when it is built,
//! and draws each with the artist's albums the library holds now.

use std::collections::{HashMap, HashSet};

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_library::identify::artist_links::placeholder;

use super::super::candidate::{Candidate, CandidateKind, Path, PathArtist, PathKind, PathSource};
use super::super::rank::{self, AlbumGates};
use super::super::{pool_store, reason, sql};
use crate::discover::facts::{FactKind, FactSource, Kept, KeptFacts, Notes, WantOrder};
use crate::discover::listening::{RankedAlbum, gate};
use crate::discover::models::{AlbumRef, Gap, SectionContent};
use crate::discover::read_all;
use crate::metadata::models::ReleaseType;

/// The fewest albums the library holds of an artist for its gaps to show
/// (the threshold Missing Essentials had).
pub(crate) const MIN_ALBUMS: i64 = 3;
/// How many of the person's artists give gaps, heaviest first.
pub(crate) const ARTISTS: usize = 10;
/// The most gaps one artist gives.
pub(crate) const PER_ARTIST: usize = 3;
/// The most gaps the section shows.
pub(crate) const ON_PAGE: usize = 15;

/// One artist whose gaps are looked for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GapArtist {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// Its weight in the person's taste.
    pub weight: f64,
    /// The person's plays and imported listens of it.
    pub plays: i64,
    /// Its taste group.
    pub group: Option<i64>,
    /// How many of its albums the library holds.
    pub albums: i64,
}

/// What the gaps source read.
#[derive(Debug, Clone, Default)]
pub(crate) struct Lists {
    /// Each artist with its kept album list (empty when nothing is there).
    pub found: Vec<(GapArtist, Vec<RankedAlbum>)>,
    /// The artists whose list is not kept yet.
    pub missing: Vec<String>,
}

impl Lists {
    /// Every release group the lists name, for the ownership read.
    pub(crate) fn groups(&self) -> impl Iterator<Item = String> + '_ {
        self.found
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .map(|row| row.album.release_group_mbid.to_lowercase())
    }
}

/// The artists `user_id` played or signalled that the library holds
/// [`MIN_ALBUMS`] or more albums of, the [`ARTISTS`] heaviest.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn artists(db: &Db, user_id: &str) -> Result<Vec<GapArtist>, DbError> {
    let rows = read_all(
        db,
        "discover.gaps.artists",
        sql::GAP_ARTISTS.text(),
        (user_id.to_owned(), MIN_ALBUMS),
        sql::GapArtistRow::read,
    )
    .await?;
    Ok(rows
        .into_iter()
        .filter(|row| !placeholder(&row.mbid))
        .take(ARTISTS)
        .map(|row| GapArtist {
            mbid: row.mbid,
            name: row.name,
            weight: row.weight,
            plays: row.plays,
            group: row.group_no,
            albums: row.albums,
        })
        .collect())
}

/// The kept album lists of `artists`. A list not kept yet is wanted, as
/// the pool's walk wants its own.
///
/// # Errors
///
/// A read's failure.
pub(crate) async fn lists(facts: &KeptFacts, artists: Vec<GapArtist>) -> Result<Lists, DbError> {
    let now = facts.now();
    let mut notes = Notes::default();
    let mut read = Lists::default();
    for artist in artists {
        let (list, _) = facts.albums(&artist.mbid).await?;
        notes.saw(
            now,
            (FactKind::Albums, FactSource::MusicBrainz, WantOrder::Pool),
            &artist.mbid,
            &list,
        );
        match list.kept {
            Kept::Missing => read.missing.push(artist.mbid),
            Kept::Nothing => read.found.push((artist, Vec::new())),
            Kept::Found(rows) => read.found.push((artist, rows)),
        }
    }
    if let Err(cause) = facts.note(notes).await {
        tracing::warn!(%cause, "the gaps' album wants went unnoted");
    }
    Ok(read)
}

/// The `gap` candidates `lists` gives through `gates`.
pub(crate) fn candidates(lists: &Lists, gates: &AlbumGates<'_>) -> Vec<Candidate> {
    let mut found = Vec::new();
    for (artist, rows) in &lists.found {
        if gates.artists.refuses(&artist.mbid) {
            continue;
        }
        let mut rows: Vec<&RankedAlbum> = rows
            .iter()
            .filter(|row| gate::may_offer(&row.album, &artist.mbid))
            .filter(|row| {
                let group = row.album.release_group_mbid.to_lowercase();
                !group.is_empty() && gates.admits(&group, &artist.mbid)
            })
            .collect();
        // Most listened first; an album nobody counted after those counted.
        rows.sort_by_key(|row| std::cmp::Reverse(row.album.facts.listeners.unwrap_or(-1)));
        let path = Path {
            kind: PathKind::Library,
            source: PathSource::Library,
            fetched_at: None,
            artist: Some(PathArtist {
                mbid: artist.mbid.clone(),
                name: artist.name.clone(),
                plays: artist.plays,
                weight: artist.weight,
                albums_owned: artist.albums,
            }),
            link: None,
            known_credit: None,
            fact: None,
        };
        for row in rows.into_iter().take(PER_ARTIST) {
            found.push(rank::album_candidate(
                row,
                (&artist.mbid, &artist.name, None),
                CandidateKind::Gap,
                (
                    std::slice::from_ref(&path),
                    artist.weight,
                    artist.group,
                    0.0,
                ),
                gates.artists,
            ));
        }
    }
    found
}

/// The library path of `candidate`, if it is a gap.
fn library_path(candidate: &Candidate) -> Option<&PathArtist> {
    candidate
        .paths
        .iter()
        .find(|path| path.kind == PathKind::Library)
        .and_then(|path| path.artist.as_ref())
}

/// The year a `YYYY` or finer date starts with.
fn year_of(date: Option<&str>) -> Option<i32> {
    date?.get(..4)?.parse().ok()
}

/// The section: the first [`ON_PAGE`] gaps of `user_id`'s pool at `now_ms`,
/// the person's heaviest artists first and then the most listened, each
/// with its artist's albums the library holds now, oldest first. A gap
/// whose artist the library no longer holds anything of is left out.
///
/// # Errors
///
/// A read's failure.
pub(crate) async fn section(
    db: &Db,
    user_id: &str,
    now_ms: i64,
) -> Result<Option<SectionContent>, DbError> {
    let mut gaps: Vec<Candidate> = pool_store::ranked(db, user_id, now_ms)
        .await?
        .into_iter()
        .filter(|candidate| library_path(candidate).is_some())
        .collect();
    gaps.sort_by(|a, b| {
        let weight = |candidate: &Candidate| library_path(candidate).map_or(0.0, |a| a.weight);
        weight(b)
            .total_cmp(&weight(a))
            .then_with(|| b.album_listeners.cmp(&a.album_listeners))
    });
    gaps.truncate(ON_PAGE);
    if gaps.is_empty() {
        return Ok(None);
    }
    let artists: HashSet<String> = gaps.iter().map(|gap| gap.artist_mbid.clone()).collect();
    let asked = serde_json::to_string(&artists).unwrap_or_else(|_| "[]".to_owned());
    let mut held: HashMap<String, Vec<AlbumRef>> = HashMap::new();
    for row in read_all(
        db,
        "discover.gaps.held",
        sql::GAP_HELD.text(),
        [asked],
        sql::GapHeldRow::read,
    )
    .await?
    {
        let album = AlbumRef {
            musicbrainz_id: row.release_group_mbid,
            local_id: Some(row.local_id),
            year: row.year.and_then(|year| i32::try_from(year).ok()),
            in_library: true,
            ..AlbumRef::titled(row.title)
        };
        held.entry(row.artist_mbid).or_default().push(album);
    }
    let rows: Vec<Gap> = gaps
        .into_iter()
        .filter_map(|gap| {
            let shelf = held.get(&gap.artist_mbid)?.clone();
            let count = i64::try_from(shelf.len()).unwrap_or(i64::MAX);
            Some(Gap {
                album: AlbumRef {
                    musicbrainz_id: Some(gap.release_group_mbid),
                    artist_name: Some(gap.artist_name),
                    artist_id: Some(gap.artist_mbid),
                    year: year_of(gap.first_release_date.as_deref()),
                    release_date: gap.first_release_date,
                    release_type: ReleaseType::from_primary_type(gap.primary_type.as_deref()),
                    ..AlbumRef::titled(gap.title)
                },
                reason: reason::gap_line(count),
                held: shelf,
            })
        })
        .collect();
    Ok((!rows.is_empty()).then_some(SectionContent::Gaps { gaps: rows }))
}
