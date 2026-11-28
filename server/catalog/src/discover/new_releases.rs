//! New from your artists: every release of the last 90 days by an artist
//! the person plays, the library owns, they follow, or that is among the
//! strongest artists of their pool, newest first.
//!
//! The rows come from two lists: ListenBrainz's sitewide fresh releases,
//! read once a day for the install ([`fresh`](crate::discover::listening::fresh)),
//! and what the follow poll found for the person's follows
//! (`acquire_new_release_feed`), which wins where both name a release
//! group. The feed is the same for everyone; the cut is the person's own
//! artists alone, so no one's list shows another's listening. Every type
//! is kept and shown as its type; the secondary types no Discover surface
//! offers ([`gate::never_offered`]) are left out, and so is a row with no
//! full date. Owned releases stay, marked; what the person told Discover
//! to hide does not. A build or a page of the list reads what is kept and
//! never waits on ListenBrainz.

use std::collections::{HashMap, HashSet};

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::page::{Page, PageRequest};
use time::OffsetDateTime;

use crate::discover::engine::reason;
use crate::discover::feedback::Feedback;
use crate::discover::listening::fresh::{FEED_DAYS, STALE_AFTER_MS};
use crate::discover::listening::{ListeningData, gate};
use crate::discover::models::{
    AlbumRef, MixTrackKind, NewRelease, NewReleaseSheet, PageSection, SecondaryKind, SectionContent,
};
use crate::discover::page::shelves;
use crate::discover::{read_all, sql};
use crate::metadata::mapping::year_of;
use crate::metadata::models::ReleaseType;

/// Releases on a page of the list when the caller names no limit.
pub const PAGE_DEFAULT: usize = 50;
/// The most releases on one page of the list.
pub const PAGE_MAX: usize = 100;
/// Releases the Discover page shows.
pub const ON_PAGE: usize = 12;
/// How many of the pool's strongest artists count as the person's
/// closest neighbours.
const NEIGHBOURS: i64 = 50;
/// A day, ms.
const DAY_MS: i64 = 86_400_000;

/// Where a page of the list starts: the last row's date and release group.
pub type ReleaseKey = (String, String);

/// How an artist is the person's.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tie {
    /// They play, own or follow it.
    Theirs,
    /// One of their pool's strongest artists, closest to the artist named.
    Near(String),
}

/// One of the person's artists.
#[derive(Debug, Clone)]
struct Artist {
    name: String,
    tie: Tie,
}

/// The person's list, newest first, when the feed it was cut from was read
/// (unix ms), the release groups the follow poll found, and each listed
/// release group's secondary types.
#[derive(Debug, Clone, Default)]
pub(crate) struct Listed {
    pub releases: Vec<NewRelease>,
    pub read_at: Option<i64>,
    pub followed: HashSet<String>,
    pub secondary: HashMap<String, Vec<String>>,
}

/// The secondary types of one row, as the feed or the follow poll gives
/// them (one name, or names joined by commas).
fn secondary_types(types: Option<&str>) -> Vec<String> {
    types
        .into_iter()
        .flat_map(|types| types.split(','))
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The first and last day the list covers at `now_ms`, `YYYY-MM-DD`.
fn window(now_ms: i64) -> (String, String) {
    let day = |ms: i64| {
        OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000))
            .map(|at| at.date().to_string())
            .unwrap_or_default()
    };
    (day(now_ms - i64::from(FEED_DAYS) * DAY_MS), day(now_ms))
}

/// The person's artists, lowercased: one they play, own or follow over
/// one that is only near theirs.
async fn artists(db: &Db, user_id: &str) -> Result<HashMap<String, Artist>, DbError> {
    let rows = read_all(
        db,
        "discover.new_releases.artists",
        sql::NEW_RELEASE_ARTISTS.text(),
        (user_id.to_owned(), NEIGHBOURS),
        sql::NewReleaseArtist::read,
    )
    .await?;
    let mut artists: HashMap<String, Artist> = HashMap::new();
    for row in rows {
        let tie = match (row.tie.as_str(), row.near) {
            ("near", Some(near)) if !near.trim().is_empty() => Tie::Near(near.trim().to_owned()),
            ("near", _) => continue,
            _ => Tie::Theirs,
        };
        let artist = Artist {
            name: row.name.trim().to_owned(),
            tie,
        };
        match artists.get(&row.mbid) {
            Some(held) if held.tie == Tie::Theirs || artist.tie != Tie::Theirs => {}
            _ => {
                artists.insert(row.mbid, artist);
            }
        }
    }
    Ok(artists)
}

/// Whether any of a release's secondary types keeps it off Discover.
fn left_out(secondary: Option<&str>) -> bool {
    secondary.is_some_and(|types| types.split(',').any(gate::never_offered))
}

/// One release on the list.
fn release(
    (release_group, title, artist_name, artist_mbid): (&str, &str, &str, &str),
    release_date: &str,
    primary_type: Option<&str>,
    reason: Option<String>,
) -> NewRelease {
    let mut album = shelves::album(
        Some(release_group),
        title,
        Some(artist_name),
        Some(artist_mbid),
        None,
    );
    album.release_date = Some(release_date.to_owned());
    album.year = year_of(Some(release_date));
    // A release with no primary type is shown as "other", never untyped.
    album.release_type =
        Some(ReleaseType::from_primary_type(primary_type).unwrap_or(ReleaseType::Other));
    NewRelease {
        album,
        reason,
        secondary_kind: None,
    }
}

/// `user_id`'s whole list, newest first, without what they told Discover
/// to hide, each row marked owned or requested.
///
/// # Errors
///
/// A read of the person's rows failed.
pub(crate) async fn list(
    db: &Db,
    listening: &ListeningData,
    user_id: &str,
) -> Result<Listed, DbError> {
    let now = listening.facts().now();
    let (from, to) = window(now);
    let feed = listening.fresh().kept().await;
    let artists = artists(db, user_id).await?;
    let mut rows: HashMap<String, NewRelease> = HashMap::new();
    let mut secondary: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(feed) = &feed {
        for row in &feed.rows {
            if row.release_date < from || row.release_date > to {
                continue;
            }
            if left_out(row.secondary_type.as_deref()) {
                continue;
            }
            // A credit the person plays, owns or follows comes before one
            // only near theirs.
            let found = row
                .artist_mbids
                .iter()
                .filter_map(|id| artists.get(id).map(|artist| (id, artist)))
                .min_by_key(|(_, artist)| matches!(artist.tie, Tie::Near(_)));
            let Some((artist_mbid, artist)) = found else {
                continue;
            };
            let reason = match &artist.tie {
                Tie::Theirs => None,
                Tie::Near(near) => Some(reason::fresh_line(&artist.name, Some(near))),
            };
            secondary.insert(
                row.release_group_mbid.clone(),
                secondary_types(row.secondary_type.as_deref()),
            );
            rows.insert(
                row.release_group_mbid.clone(),
                release(
                    (
                        &row.release_group_mbid,
                        &row.title,
                        &row.artist_name,
                        artist_mbid,
                    ),
                    &row.release_date,
                    row.primary_type.as_deref(),
                    reason,
                ),
            );
        }
    }
    let followed = read_all(
        db,
        "discover.new_releases.followed",
        sql::FOLLOWED_RELEASES.text(),
        (user_id.to_owned(), from, to),
        sql::FollowedReleaseColumns::read,
    )
    .await?;
    let mut from_follows = HashSet::new();
    for row in followed {
        if left_out(row.secondary_types.as_deref()) {
            continue;
        }
        from_follows.insert(row.release_group_mbid.clone());
        secondary.insert(
            row.release_group_mbid.clone(),
            secondary_types(row.secondary_types.as_deref()),
        );
        rows.insert(
            row.release_group_mbid.clone(),
            release(
                (
                    &row.release_group_mbid,
                    &row.title,
                    &row.artist_name,
                    &row.artist_mbid,
                ),
                &row.release_date,
                row.primary_type.as_deref(),
                None,
            ),
        );
    }
    let feedback = Feedback::load_or_none(db, user_id, now, "new releases").await;
    let mut releases: Vec<NewRelease> = rows
        .into_values()
        .filter(|row| !feedback.hides_album_row(&row.album))
        .collect();
    for row in &mut releases {
        row.secondary_kind = row
            .album
            .musicbrainz_id
            .as_ref()
            .and_then(|group| secondary.get(group))
            .and_then(|types| SecondaryKind::of(types));
    }
    releases.sort_by(|a, b| {
        b.album
            .release_date
            .cmp(&a.album.release_date)
            .then_with(|| a.album.musicbrainz_id.cmp(&b.album.musicbrainz_id))
    });
    mark(db, &mut releases).await?;
    Ok(Listed {
        releases,
        read_at: feed.map(|feed| feed.read_at),
        followed: from_follows,
        secondary,
    })
}

/// Mark which of `releases` the library holds and which have an open
/// request, as they stand now.
async fn mark(db: &Db, releases: &mut [NewRelease]) -> Result<(), DbError> {
    let mut albums: Vec<&mut AlbumRef> = releases.iter_mut().map(|row| &mut row.album).collect();
    mark_albums(db, &mut albums).await
}

/// Mark `albums` held by the library (with their library id) or asked for,
/// as they stand now.
async fn mark_albums(db: &Db, albums: &mut [&mut AlbumRef]) -> Result<(), DbError> {
    let ids: Vec<String> = albums
        .iter()
        .filter_map(|album| album.musicbrainz_id.clone())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let asked = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_owned());
    let marks: HashMap<String, (Option<String>, bool)> = read_all(
        db,
        "discover.new_releases.marks",
        sql::RELEASE_MARKS.text(),
        [asked],
        sql::ReleaseMark::read,
    )
    .await?
    .into_iter()
    .map(|row| (row.mbid, (row.local_id, row.requested)))
    .collect();
    for album in albums.iter_mut() {
        let (local_id, requested) = album
            .musicbrainz_id
            .as_ref()
            .and_then(|id| marks.get(id))
            .cloned()
            .unwrap_or_default();
        album.in_library = local_id.is_some();
        album.requested = !album.in_library && requested;
        album.local_id = local_id;
    }
    Ok(())
}

/// The section's rows: the newest [`ON_PAGE`] of `listed`, `None` when it
/// holds none.
pub(crate) fn sheet(listed: Listed) -> Option<SectionContent> {
    if listed.releases.is_empty() {
        return None;
    }
    let total = u64::try_from(listed.releases.len()).unwrap_or(u64::MAX);
    let mut releases = listed.releases;
    releases.truncate(ON_PAGE);
    Some(SectionContent::NewReleases {
        new_releases: NewReleaseSheet {
            releases,
            total,
            updated_at: listed.read_at.map(|at| at / 1000),
        },
    })
}

/// When the feed behind `content` was read (unix seconds), if that was so
/// long ago at `now_ms` that its daily read has missed.
pub(crate) fn stale_since(content: &SectionContent, now_ms: i64) -> Option<i64> {
    let SectionContent::NewReleases { new_releases } = content else {
        return None;
    };
    let read_at = new_releases.updated_at?;
    (now_ms - read_at * 1000 > STALE_AFTER_MS).then_some(read_at)
}

/// Mark the new releases and the mixes' new albums of the served `sections`
/// owned or requested as they stand now. A failed read leaves them as built,
/// with a log line.
pub(crate) async fn mark_served(db: &Db, sections: &mut [PageSection]) {
    for section in sections {
        match &mut section.content {
            SectionContent::NewReleases { new_releases } => {
                if let Err(error) = mark(db, &mut new_releases.releases).await {
                    tracing::warn!(%error, "new releases' marks unreadable; served as built");
                }
            }
            // A mix's new albums, so Keep and Download all leave out one
            // already asked for.
            SectionContent::Mixes { mixes } => {
                let mut albums: Vec<&mut AlbumRef> = mixes
                    .iter_mut()
                    .flat_map(|mix| mix.tracks.iter_mut())
                    .filter(|track| track.kind == MixTrackKind::New)
                    .map(|track| &mut track.album)
                    .collect();
                if let Err(error) = mark_albums(db, &mut albums).await {
                    tracing::warn!(%error, "mix albums' marks unreadable; served as built");
                }
            }
            _ => {}
        }
    }
}

/// Each person's list, as its route reads it.
#[derive(Clone)]
pub struct NewReleaseLists {
    db: Db,
    listening: ListeningData,
}

impl NewReleaseLists {
    /// The lists cut from `listening`'s kept feed and the rows in `db`.
    pub fn new(db: Db, listening: ListeningData) -> Self {
        Self { db, listening }
    }

    /// One page of `user_id`'s list.
    ///
    /// # Errors
    ///
    /// A read of the person's rows failed.
    pub async fn page(
        &self,
        user_id: &str,
        paging: PageRequest<ReleaseKey>,
    ) -> Result<Page<NewRelease>, DbError> {
        page(&self.db, &self.listening, user_id, paging).await
    }
}

/// One page of `user_id`'s whole list, newest first, with how many it
/// holds.
async fn page(
    db: &Db,
    listening: &ListeningData,
    user_id: &str,
    paging: PageRequest<ReleaseKey>,
) -> Result<Page<NewRelease>, DbError> {
    let listed = list(db, listening, user_id).await?;
    let total = u64::try_from(listed.releases.len()).unwrap_or(u64::MAX);
    let key = |row: &NewRelease| {
        (
            row.album.release_date.clone().unwrap_or_default(),
            row.album.musicbrainz_id.clone().unwrap_or_default(),
        )
    };
    let rows: Vec<NewRelease> = listed
        .releases
        .into_iter()
        .skip_while(|row| {
            paging.after.as_ref().is_some_and(|(date, group)| {
                let (row_date, row_group) = key(row);
                row_date > *date || (row_date == *date && row_group <= *group)
            })
        })
        .take(paging.limit + 1)
        .collect();
    Ok(Page::cut(rows, paging.limit, key).with_total(total))
}
