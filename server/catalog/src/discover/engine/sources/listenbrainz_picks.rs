//! ListenBrainz's own picks: the person's weekly exploration playlist,
//! while it was made in the last seven days, each track's release group
//! found through the recording lookup. Both are the cached reads the
//! Discover page makes, on the person's own link, at background priority
//! and only while the day's background reads have room; most answers come
//! from the cache.

use std::collections::{HashMap, HashSet};

use droppedneedle_providers::RequestPriority;

use super::super::candidate::{Candidate, CandidateKind, Path, PathKind, PathSource};
use super::super::pool_store::GroupFacts;
use super::super::rank::{self, AlbumGates};
use crate::discover::facts::plan;
use crate::discover::listening::{
    AlbumFacts, AlbumRow, Ask, ListeningData, RankedAlbum, UserMusic, gate,
};

/// How old a playlist may be and still count, ms: seven days.
pub(crate) const PLAYLIST_MAX_AGE_MS: i64 = 7 * 86_400_000;

/// One album the playlist names.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PickRow {
    /// Its release group, lowercased.
    pub release_group: String,
    /// Its title, as the playlist gives it.
    pub title: String,
    /// The track's first credited artist, lowercased.
    pub artist_mbid: String,
    /// The track's credit.
    pub artist_name: String,
    /// When the playlist was made, unix ms.
    pub made_at: Option<i64>,
}

/// What the source read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Picked {
    /// The albums of this week's playlist; none when there is no playlist
    /// this week, or the person has no ListenBrainz link.
    Rows(Vec<PickRow>),
    /// A read failed, or the day had no room: the facts are missing.
    Missing,
}

/// When a playlist dated `date` (RFC 3339, as ListenBrainz sends it) was
/// made, unix ms.
fn made_at(date: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    let made = time::OffsetDateTime::parse(date.trim(), &Rfc3339).ok()?;
    i64::try_from(made.unix_timestamp_nanos() / 1_000_000).ok()
}

/// `user_id`'s ListenBrainz picks at `now_ms`, for their accounts `user`.
pub(crate) async fn read(
    listening: &ListeningData,
    user_id: &str,
    user: &UserMusic,
    now_ms: i64,
) -> Picked {
    let Some(username) = user.listenbrainz.as_deref() else {
        return Picked::Rows(Vec::new());
    };
    if !listening.listenbrainz_on() {
        return Picked::Rows(Vec::new());
    }
    match plan::room(listening.facts()).await {
        Ok(room) if room > 0 => {}
        Ok(_) => return Picked::Missing,
        Err(error) => {
            tracing::warn!(%error, "the day's reads unreadable; ListenBrainz picks not read");
            return Picked::Missing;
        }
    }
    let ask = Ask::build(RequestPriority::BackgroundSync);
    let playlist = match listening.weekly_playlist(user_id, username, &ask).await {
        Ok(Some(playlist)) => playlist,
        Ok(None) => return Picked::Rows(Vec::new()),
        Err(error) => {
            tracing::debug!(%error, "weekly playlist unread; the pool keeps its picks");
            return Picked::Missing;
        }
    };
    let made = made_at(&playlist.date);
    if made.is_none_or(|made| now_ms - made > PLAYLIST_MAX_AGE_MS) {
        return Picked::Rows(Vec::new());
    }
    let recordings: Vec<String> = playlist
        .tracks
        .iter()
        .filter_map(|track| track.recording_mbid.clone())
        .collect();
    let groups = match listening.recording_groups(user_id, &recordings, &ask).await {
        Ok(groups) => groups,
        Err(error) => {
            tracing::debug!(%error, "weekly playlist's albums unread; the pool keeps its picks");
            return Picked::Missing;
        }
    };
    let mut seen = HashSet::new();
    let rows = playlist
        .tracks
        .into_iter()
        .filter_map(|track| {
            let recording = track.recording_mbid?.trim().to_ascii_lowercase();
            let release_group = groups.get(&recording)?.trim().to_ascii_lowercase();
            let artist_mbid = track.artist_mbid?.trim().to_ascii_lowercase();
            seen.insert(release_group.clone()).then_some(PickRow {
                release_group,
                title: track.album,
                artist_mbid,
                artist_name: track.creator,
                made_at: made,
            })
        })
        .collect();
    Picked::Rows(rows)
}

/// The candidates `rows` give through `gates`: an album the kept lists hold
/// carries their facts, and one they do not passes the type gate, as an
/// unknown fact does. `listeners` holds the counted artists' listeners.
pub(crate) fn candidates(
    rows: &[PickRow],
    facts: &HashMap<String, Vec<GroupFacts>>,
    listeners: &HashMap<String, i64>,
    gates: &AlbumGates<'_>,
) -> Vec<Candidate> {
    rows.iter()
        .filter(|row| {
            !gates.artists.knows(&row.artist_mbid) && !gates.artists.refuses(&row.artist_mbid)
        })
        .filter_map(|row| {
            let kept = facts.get(&row.release_group).and_then(|lists| {
                lists
                    .iter()
                    .find(|facts| facts.artist_mbid == row.artist_mbid)
                    .or_else(|| lists.first())
            });
            let album = RankedAlbum {
                album: AlbumRow {
                    release_group_mbid: row.release_group.clone(),
                    title: row.title.clone(),
                    artist_name: row.artist_name.clone(),
                    artist_mbid: Some(row.artist_mbid.clone()),
                    facts: kept.map_or_else(AlbumFacts::default, |kept| AlbumFacts {
                        primary_type: kept.primary_type.clone(),
                        secondary_types: kept.secondary_types.clone(),
                        first_release_date: kept.first_release_date.clone(),
                        listeners: kept.listeners,
                        credited_artist_mbids: kept.credited_artist_mbids.clone(),
                    }),
                },
                listen_count: 0,
            };
            let admitted = gate::may_offer(&album.album, &row.artist_mbid)
                && gates.admits(&row.release_group, &row.artist_mbid);
            admitted.then(|| {
                let path = Path {
                    kind: PathKind::Source,
                    source: PathSource::Listenbrainz,
                    fetched_at: row.made_at,
                    artist: None,
                    link: None,
                    known_credit: None,
                };
                rank::album_candidate(
                    &album,
                    (
                        &row.artist_mbid,
                        &row.artist_name,
                        listeners.get(&row.artist_mbid).copied(),
                    ),
                    CandidateKind::SourcePick,
                    (&[path], 0.0, None, 0.0),
                    gates.artists,
                )
            })
        })
        .collect()
}
