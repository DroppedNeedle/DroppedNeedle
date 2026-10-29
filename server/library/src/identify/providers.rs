//! Recall: gather candidate releases, with their tracklists, for one album.
//!
//! The live recall follows beets and Lidarr:
//!
//! 1. When most files name one release MBID, fetch that release first. If
//!    it matches closely ([`STRONG`]) recall stops there: one request.
//!    When the files also name a release group (see
//!    [`named_group`](super::evidence::named_group)) and the release now
//!    sits in another group, the release's group is the one they name.
//! 2. When the files name a release group and step 1 found no release
//!    they name, list the group's editions and fetch those that could
//!    hold the files, as in step 5. A group id MusicBrainz merged lists
//!    nothing: it is looked up, and the group it was kept under is listed
//!    instead. One MusicBrainz no longer has, or will not list, is set
//!    aside.
//! 3. Search by album title and album artist (no artist for
//!    compilations), looser each time nothing comes back: without edition
//!    and disc words, then the title's words, then the first track as a
//!    recording.
//! 4. Fetch the tracklists of the best few hits, skipping releases too
//!    small to hold the files, at most [`PER_GROUP`] editions of one
//!    release group and [`FETCH_LIMIT`] in all. Fetched releases are
//!    kept in the release store and reused for a week.
//! 5. List every edition of the release group the files fit best (unless
//!    step 2 listed it) and
//!    fetch the ones that could hold the files, best agreeing with the
//!    tags' barcode and catalog number, disc count, track count and media first, at most
//!    [`GROUP_FETCH`]. Search alone often misses the edition that fits.
//! 6. Resolve file recording MBIDs no candidate carries, in case
//!    MusicBrainz merged them: every one, up to one per file.
//! 7. When the tags are weak, fingerprint the files and add the releases
//!    most of the prints point at.
//!
//! Every MusicBrainz call is identity-critical: an outage defers the job.
//! A release MusicBrainz answers in a form asking again will not change
//! (refused, or a document that does not read) is dropped from the
//! candidates and not asked for again in the same recall. The result says
//! so through its failure code ([`RELEASES_UNREADABLE`], or
//! [`TAGGED_RELEASE_UNREADABLE`] when the refused one is the files'
//! tagged release). When that leaves no candidate the job defers as for
//! an outage, and the service defers too when what is left matches
//! nothing: a source that refuses lookups is a setup problem, never
//! evidence that the album is unmatched.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::evidence::local_album;
use super::models::{LocalAlbumFacts, RecallResult};
use super::sources::{
    AlbumQuery, ArtistAnswer, Edition, EditionPage, EditionQuery, FingerprintSource, Prioritised,
    ReleaseAnswer, ReleaseHit, ReleaseSource, SourceError, Unanswered, group_editions,
};
use super::sqlite::SqliteIdentifyStore;
use super::stores::fresh_release;
use crate::matching::decide::STRONG;
use crate::matching::score::NAME_GATE;
use crate::matching::strings::{
    before_featuring, fold, string_dist, strip_edition_words, without_brackets,
};
use crate::matching::{
    LocalAlbum, Release, ReleaseMatch, match_release, should_fingerprint, unknown_recordings,
};
use crate::scan::pool::BlockingPool;
use droppedneedle_providers::RequestPriority;

/// Tracklists fetched per album after a search.
pub const FETCH_LIMIT: usize = 5;
/// Editions of one release group fetched per album.
pub const PER_GROUP: usize = 3;
/// Further editions of the best release group fetched per album.
pub const GROUP_FETCH: usize = 6;
/// Releases nominated by fingerprints fetched per album.
const FINGERPRINT_RELEASES: usize = 2;
/// Share of printed tracks that must point at a release to nominate it
/// (beets' chroma plugin).
const COMMON_RELEASE_SHARE: f64 = 0.6;

/// What the identify service needs from providers. The live adapter owns
/// provider clients; the fake answers from scripts. No live network in tests.
/// Recall and edition search ask MusicBrainz at the caller's `priority`:
/// user priority when a person waits on the answer, background otherwise.
pub trait IdentifyProviders: Send + Sync {
    /// Recall candidate releases for the album facts. `named_group` is
    /// the release group the files name, weighed first.
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
        named_group: Option<&str>,
        priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>>;

    /// Recall the one release an administrator named for the album. The
    /// default finds nothing, so a provider without this lane reads as "no
    /// such release".
    fn recall_release(
        &self,
        _facts: &LocalAlbumFacts,
        _release_mbid: &str,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        Box::pin(async { RecallOutcome::default() })
    }

    /// One page of releases for a curator looking for an exact edition.
    /// The default lists nothing.
    fn search_editions(
        &self,
        _query: EditionQuery,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<EditionPage, SourceError>> + Send + '_>> {
        Box::pin(async { Ok(EditionPage::default()) })
    }

    /// What MusicBrainz says about one artist id. The default cannot
    /// tell, so nothing is learned.
    fn canonical_artist(
        &self,
        _mbid: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ArtistAnswer, Unanswered>> + Send + '_>> {
        Box::pin(async { Err(Unanswered::cannot_tell()) })
    }

    /// The document of one release, asked of MusicBrainz. The default
    /// cannot tell, so nothing is learned.
    fn release_document(
        &self,
        _release_mbid: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ReleaseAnswer, Unanswered>> + Send + '_>>
    {
        Box::pin(async { Err(Unanswered::cannot_tell()) })
    }
}

/// The failure code of an exact recall whose release MusicBrainz answers
/// in a form that will not change by asking again: refused, or a document
/// that does not decode.
pub const RELEASE_UNREADABLE: &str = "release_unreadable";

/// The failure code of a recall in which MusicBrainz refused, in a form
/// asking again will not change, a release it was asked for. With no
/// readable candidate left the recall defers; otherwise it goes on, and
/// an attempt that would end matching nothing defers with this code.
pub const RELEASES_UNREADABLE: &str = "releases_unreadable";

/// The failure code of a recall that went on without the release the
/// files' tags name, because MusicBrainz refused it. Nothing can be
/// confirmed against the files' strongest edition evidence, so whatever
/// the recall picks stays an unconfirmed guess. It also says, as
/// [`RELEASES_UNREADABLE`] does, that a release was refused.
pub const TAGGED_RELEASE_UNREADABLE: &str = "TAGGED_RELEASE_UNREADABLE";

/// The failure code of a recall whose release matching failed (a job on
/// the CPU pool panicked). The job waits and retries, as for an outage.
pub const MATCHING_FAILED: &str = "matching_failed";

/// Why a recall stopped before it could answer.
enum Stopped {
    /// A provider could not answer.
    Provider(SourceError),
    /// Matching the releases against the files failed.
    Matching,
}

impl From<SourceError> for Stopped {
    fn from(error: SourceError) -> Self {
        Self::Provider(error)
    }
}

/// Recall result, with the matches recall scored on the way.
#[derive(Debug, Clone, Default)]
pub struct RecallOutcome {
    pub result: RecallResult,
    /// Releases recall already matched against the files, so they are not
    /// scored again.
    pub scored: Scored,
}

/// Matches recall worked out, by release id, against the files with no
/// fingerprint support and under `aliases`. One stands in only for a
/// scoring with those same inputs.
#[derive(Debug, Clone, Default)]
pub struct Scored {
    aliases: HashMap<String, String>,
    matches: HashMap<String, ReleaseMatch>,
}

impl Scored {
    /// Take recall's match for `release` when the scoring asked for has
    /// recall's inputs: no fingerprint support and the same aliases.
    pub fn take(
        &mut self,
        release: &Release,
        fingerprints: &HashMap<String, Vec<String>>,
        aliases: &HashMap<String, String>,
    ) -> Option<ReleaseMatch> {
        if !fingerprints.is_empty() || *aliases != self.aliases {
            return None;
        }
        self.matches.remove(&release.id)
    }
}

/// Live recall over a release source for each priority, the release
/// store, a fingerprint source, and the CPU pool releases are matched on.
pub struct LiveProviders<R, F> {
    releases: Prioritised<R>,
    store: Arc<SqliteIdentifyStore>,
    audio: F,
    cpu: BlockingPool,
}

impl<R: ReleaseSource, F: FingerprintSource> LiveProviders<R, F> {
    pub fn new(
        releases: Prioritised<R>,
        store: Arc<SqliteIdentifyStore>,
        audio: F,
        cpu: BlockingPool,
    ) -> Self {
        Self {
            releases,
            store,
            audio,
            cpu,
        }
    }

    /// Recall for `album`, asking MusicBrainz at `priority`.
    fn at<'a>(&'a self, album: &'a str, priority: RequestPriority) -> Recall<'a, R, F> {
        Recall {
            album,
            releases: self.releases.at(priority),
            store: &self.store,
            audio: &self.audio,
            cpu: &self.cpu,
            refused: Mutex::new(HashSet::new()),
        }
    }
}

/// One recall's view of the providers: the release source at the
/// caller's priority, and the releases MusicBrainz refused so far.
struct Recall<'a, R, F> {
    album: &'a str,
    releases: &'a R,
    store: &'a Arc<SqliteIdentifyStore>,
    audio: &'a F,
    cpu: &'a BlockingPool,
    /// Release ids (lowercase) MusicBrainz refused in this recall.
    refused: Mutex<HashSet<String>>,
}

impl<R: ReleaseSource, F: FingerprintSource> Recall<'_, R, F> {
    async fn recall(
        &self,
        facts: &LocalAlbumFacts,
        named: Option<&str>,
    ) -> Result<RecallOutcome, Stopped> {
        let local = local_album(facts, &HashMap::new());
        let mut named = named.map(|group| group.trim().to_ascii_lowercase());
        let mut releases: Vec<Release> = Vec::new();
        // Matches of `releases`, in order, scored without aliases until
        // the aliases are known.
        let mut scored: Vec<ReleaseMatch> = Vec::new();
        let no_aliases = HashMap::new();
        let tagged = local.tagged_release();
        if let Some(tagged) = tagged.as_deref()
            && let Some(release) = self.release(tagged, None).await?
        {
            // A release MusicBrainz moved leaves a stale group tag behind.
            if let Some(group) = named.as_mut()
                && !release.release_group_id.eq_ignore_ascii_case(group)
            {
                *group = release.release_group_id.to_ascii_lowercase();
            }
            releases.push(release);
            self.score_new(&local, &releases, &mut scored, &no_aliases)
                .await?;
            let strong = scored.first().is_some_and(|matched| {
                matched.conflicts.is_empty() && matched.library_distance() <= STRONG
            });
            if strong {
                let scored = keyed(&releases, scored, no_aliases);
                return Ok(RecallOutcome {
                    result: RecallResult {
                        releases,
                        named_group: named,
                        ..RecallResult::default()
                    },
                    scored,
                });
            }
        }

        // A release the files name outranks the group they name: its
        // group is weighed through the usual recall.
        let mut listed = None;
        if releases.is_empty()
            && let Some(group) = named.take()
        {
            named = self.named_editions(&local, &mut releases, group).await?;
            listed.clone_from(&named);
        }

        let hits = self.search(facts, local.tracks.len()).await?;
        for hit in pick_hits(&hits, local.tracks.len(), &releases) {
            if let Some(release) = self.release(&hit.id, Some(&hit.release_group_id)).await? {
                push_new(&mut releases, release);
            }
        }

        self.score_new(&local, &releases, &mut scored, &no_aliases)
            .await?;
        if let Some(group) = best_group(&releases, &scored)
            && listed.as_deref() != Some(group.as_str())
        {
            let editions = group_editions(self.releases, &group).await?;
            self.fetch_editions(&local, &mut releases, &group, &editions)
                .await?;
        }
        let recording_aliases = self.resolve_aliases(&local, &releases).await?;
        if !recording_aliases.is_empty() {
            scored.clear();
        }
        self.score_new(&local, &releases, &mut scored, &recording_aliases)
            .await?;
        let mut fingerprint_support = HashMap::new();
        if should_fingerprint(&local, &scored) {
            let heard = self.audio.identify(&facts.tracks).await;
            for id in common_releases(&heard.releases, heard.recordings.len().max(1)) {
                if releases.iter().any(|release| release.answers_to(&id)) {
                    continue;
                }
                if let Some(release) = self.release(&id, None).await? {
                    push_new(&mut releases, release);
                }
            }
            fingerprint_support = heard.recordings;
        }
        if releases.is_empty() && self.refused_any() {
            tracing::info!(
                album = self.album,
                "MusicBrainz refused every candidate release; identification waits"
            );
            return Ok(RecallOutcome {
                result: RecallResult {
                    provider_deferred: true,
                    failure_code: Some(RELEASES_UNREADABLE.to_owned()),
                    ..RecallResult::default()
                },
                scored: Scored::default(),
            });
        }
        let failure_code = if tagged.is_some_and(|tagged| self.was_refused(&tagged)) {
            Some(TAGGED_RELEASE_UNREADABLE.to_owned())
        } else if self.refused_any() {
            Some(RELEASES_UNREADABLE.to_owned())
        } else {
            None
        };
        let scored = keyed(&releases, scored, recording_aliases.clone());
        Ok(RecallOutcome {
            result: RecallResult {
                releases,
                fingerprint_support,
                recording_aliases,
                named_group: named,
                failure_code,
                ..RecallResult::default()
            },
            scored,
        })
    }

    /// Match the releases past the end of `scored` against `local` under
    /// `aliases`, on the CPU pool, and add their matches to `scored`. A
    /// matching job that panicked stops the recall.
    async fn score_new(
        &self,
        local: &LocalAlbum,
        releases: &[Release],
        scored: &mut Vec<ReleaseMatch>,
        aliases: &HashMap<String, String>,
    ) -> Result<(), Stopped> {
        let fresh: Vec<Release> = releases.iter().skip(scored.len()).cloned().collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let local = local.clone();
        let aliases = aliases.clone();
        let matches = self
            .cpu
            .run(move || {
                fresh
                    .iter()
                    .map(|release| match_release(&local, release, &aliases))
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|_| Stopped::Matching)?;
        scored.extend(matches);
        Ok(())
    }

    /// Fetch the editions of the group the files name, and say which group
    /// that is now. A group whose listing is empty is looked up: one
    /// MusicBrainz merged is followed to the group it was kept under, one
    /// it no longer has, or will not list, is set aside (`None`). When
    /// every edition fetched reports one other group, the search index lags
    /// a merge, and that group is the one named.
    async fn named_editions(
        &self,
        local: &LocalAlbum,
        releases: &mut Vec<Release>,
        mut group: String,
    ) -> Result<Option<String>, SourceError> {
        let Some(mut editions) = self.releases.named_group_editions(&group).await? else {
            return Ok(None);
        };
        if editions.is_empty() {
            match self.releases.canonical_release_group(&group).await? {
                Some(kept) if kept != group => {
                    let Some(listed) = self.releases.named_group_editions(&kept).await? else {
                        return Ok(None);
                    };
                    editions = listed;
                    group = kept;
                }
                Some(_) => {}
                None => {
                    tracing::debug!(
                        release_group = group,
                        "MusicBrainz has no release group the files name; it is set aside"
                    );
                    return Ok(None);
                }
            }
        }
        let fetched = self
            .fetch_editions(local, releases, &group, &editions)
            .await?;
        if let Some(first) = fetched.first()
            && *first != group
            && fetched.iter().all(|other| other == first)
        {
            group = first.clone();
        }
        Ok(Some(group))
    }

    /// Search hits for the album, each tier asked only when the ones
    /// before it found nothing:
    ///
    /// 1. the title as a phrase;
    /// 2. the title without edition and disc words ("Deluxe", "CD1"), when
    ///    that changes it;
    /// 3. the title's words, any trailing bracket dropped, matched loosely;
    ///    the index ORs words, so only hits whose title passes the album
    ///    title gate count;
    /// 4. the first track's title as a recording, with the artist, which
    ///    finds the releases that carry it.
    ///
    /// The artist is the album artist, none for a compilation, and, when
    /// the album row names a placeholder, the artist more than half the
    /// tracks name. Tracks of several artists with none over half search
    /// as Various Artists: no artist, and the various flag, which only
    /// ranks Various Artists releases first. The last tier needs an
    /// artist: for such an album, the first track's own. An album no tier
    /// finds costs at most four searches and no release lookups; each
    /// search goes through the source's own client and priority.
    async fn search(
        &self,
        facts: &LocalAlbumFacts,
        track_count: usize,
    ) -> Result<Vec<ReleaseHit>, SourceError> {
        let SearchArtist {
            artist,
            various,
            per_track,
        } = search_artist(facts);
        let query = |title, words| AlbumQuery {
            title,
            artist,
            track_count,
            various,
            words,
        };
        let title = facts.title.trim();
        if !title.is_empty() {
            let hits = self.releases.search_album(query(title, false)).await?;
            if !hits.is_empty() {
                return Ok(hits);
            }
            let stripped = strip_edition_words(title);
            if fold(&stripped) != fold(title) {
                let hits = self.releases.search_album(query(&stripped, false)).await?;
                if !hits.is_empty() {
                    return Ok(hits);
                }
            }
            let loose = strip_edition_words(without_trailing_bracket(title));
            let near: Vec<ReleaseHit> = self
                .releases
                .search_album(query(&loose, true))
                .await?
                .into_iter()
                .filter(|hit| {
                    let found = strip_edition_words(&hit.title);
                    [&stripped, &loose]
                        .iter()
                        .any(|wanted| string_dist(wanted, &found) <= NAME_GATE)
                })
                .collect();
            if !near.is_empty() {
                return Ok(near);
            }
        }
        if artist.is_empty() && !per_track {
            return Ok(Vec::new());
        }
        let mut tracks: Vec<_> = facts.tracks.iter().collect();
        tracks.sort_by_key(|track| (track.disc_number, track.track_number));
        let Some((recording, track)) = tracks
            .iter()
            .map(|track| (without_brackets(&track.title), track))
            .find(|(title, _)| !title.is_empty())
        else {
            return Ok(Vec::new());
        };
        let artist = if per_track {
            let own = before_featuring(track.artist_name.trim());
            if is_placeholder(own) || is_various(own) {
                return Ok(Vec::new());
            }
            own
        } else {
            artist
        };
        self.releases.search_recording(&recording, artist).await
    }

    /// Weigh more editions of `group` than search recall fetches (a few
    /// per group): the edition that fits the files is often not among
    /// them. Of `editions`, the group's editions as the release index
    /// lists them, those big enough to hold the files are fetched, the
    /// ones whose barcode, catalog number, disc count, track count and format agree with
    /// the files first, at most [`GROUP_FETCH`]. Returns the group each
    /// fetched release reports, lowercase.
    async fn fetch_editions(
        &self,
        local: &LocalAlbum,
        releases: &mut Vec<Release>,
        group: &str,
        editions: &[Edition],
    ) -> Result<Vec<String>, SourceError> {
        let files = local.tracks.len();
        let hints = &local.hints;
        let mut wanted: Vec<_> = editions
            .iter()
            .filter(|edition| edition.release_group_mbid.eq_ignore_ascii_case(group))
            .filter(|edition| {
                !releases
                    .iter()
                    .any(|known| known.answers_to(&edition.release_mbid))
            })
            .filter(|edition| edition.track_count == 0 || edition.track_count as usize + 2 >= files)
            .collect();
        wanted.sort_by_key(|edition| {
            let barcode = hints.barcode.as_deref().is_some_and(|wanted| {
                edition
                    .barcode
                    .as_deref()
                    .and_then(crate::matching::EditionHints::barcode_key)
                    .is_some_and(|key| key == wanted)
            });
            let catalog = hints.catalog_number.as_deref().is_some_and(|wanted| {
                edition
                    .catalogue_number
                    .as_deref()
                    .and_then(crate::matching::EditionHints::catalog_key)
                    .is_some_and(|key| key == wanted)
            });
            let discs = hints
                .total_discs
                .is_some_and(|total| total == edition.disc_count);
            let media = hints.media.as_deref().is_some_and(|media| {
                let media = media.to_lowercase();
                edition
                    .media_formats
                    .iter()
                    .any(|format| format.to_lowercase().contains(&media))
            });
            (
                !(barcode && catalog),
                !(barcode || catalog),
                !discs,
                (edition.track_count as usize).abs_diff(files),
                !media,
                edition.release_mbid.clone(),
            )
        });
        let mut groups = Vec::new();
        for edition in wanted.into_iter().take(GROUP_FETCH) {
            if let Some(release) = self
                .release(&edition.release_mbid, Some(&edition.release_group_mbid))
                .await?
            {
                groups.push(release.release_group_id.to_ascii_lowercase());
                push_new(releases, release);
            }
        }
        Ok(groups)
    }

    /// Recall for an exact release: that release alone, plus the merged
    /// recording ids its files need. No search and no fingerprints. An
    /// answer asking again will not change (one that does not decode,
    /// say) recalls nothing, with [`RELEASE_UNREADABLE`] as its failure
    /// code.
    async fn recall_exact(
        &self,
        facts: &LocalAlbumFacts,
        release_mbid: &str,
    ) -> Result<RecallResult, SourceError> {
        let local = local_album(facts, &HashMap::new());
        let answer = match fresh_release(self.store, release_mbid).await {
            Ok(Some(stored)) => ReleaseAnswer::Found(Box::new(stored)),
            Ok(None) => self.release_answer(release_mbid).await?,
            Err(error) => {
                tracing::warn!(
                    %error,
                    release_mbid,
                    "stored release document unreadable; asking MusicBrainz"
                );
                self.release_answer(release_mbid).await?
            }
        };
        let release = match answer {
            ReleaseAnswer::Found(release) => *release,
            ReleaseAnswer::Missing => return Ok(RecallResult::default()),
            ReleaseAnswer::Refused(reason) => {
                tracing::info!(
                    album = self.album,
                    release_mbid,
                    reason,
                    "MusicBrainz refused a release document"
                );
                return Ok(RecallResult {
                    failure_code: Some(RELEASE_UNREADABLE.to_owned()),
                    ..RecallResult::default()
                });
            }
        };
        let releases = vec![release];
        let recording_aliases = self.resolve_aliases(&local, &releases).await?;
        Ok(RecallResult {
            releases,
            recording_aliases,
            ..RecallResult::default()
        })
    }

    /// What MusicBrainz answers for release `mbid`; an error only when it
    /// gave no answer this time.
    async fn release_answer(&self, mbid: &str) -> Result<ReleaseAnswer, SourceError> {
        self.releases
            .release_answer(mbid)
            .await
            .map_err(|unanswered| unanswered.error)
    }

    /// A release from the store when fresh, else from the source. The
    /// caller keeps what recall fetched, on its own lane. A store that
    /// cannot be read is logged and the source asked: it answers the same
    /// document. One MusicBrainz refuses is dropped (`None`) and not asked
    /// for again in this recall; `group` is the release group the caller
    /// knows it by, for the log. An error only when MusicBrainz gave no
    /// answer this time.
    async fn release(
        &self,
        mbid: &str,
        group: Option<&str>,
    ) -> Result<Option<Release>, SourceError> {
        match fresh_release(self.store, mbid).await {
            Ok(Some(stored)) => return Ok(Some(stored)),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                %error,
                release_mbid = mbid,
                "stored release document unreadable; asking MusicBrainz"
            ),
        }
        if self.was_refused(mbid) {
            return Ok(None);
        }
        match self.release_answer(mbid).await? {
            ReleaseAnswer::Found(release) => Ok(Some(*release)),
            ReleaseAnswer::Missing => Ok(None),
            ReleaseAnswer::Refused(reason) => {
                tracing::info!(
                    album = self.album,
                    release_mbid = mbid,
                    release_group = group,
                    reason,
                    "MusicBrainz refused a candidate release; recall goes on without it"
                );
                self.refused_ids().insert(mbid.to_ascii_lowercase());
                Ok(None)
            }
        }
    }

    /// The ids refused so far; a poisoned lock still holds them.
    fn refused_ids(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn was_refused(&self, mbid: &str) -> bool {
        self.refused_ids().contains(&mbid.to_ascii_lowercase())
    }

    fn refused_any(&self) -> bool {
        !self.refused_ids().is_empty()
    }

    /// Map file recording MBIDs to the ids MusicBrainz merged them into,
    /// when it did. An id is looked up when no candidate carries it, or
    /// when the release its file names was fetched and lacks it (another
    /// release may still list the old id). Every such id is looked up (at
    /// most [`MERGE_LOOKUPS`](crate::matching::score::MERGE_LOOKUPS)): one
    /// left out would veto the release the files are.
    async fn resolve_aliases(
        &self,
        local: &LocalAlbum,
        releases: &[Release],
    ) -> Result<HashMap<String, String>, SourceError> {
        let mut aliases = HashMap::new();
        for mbid in unknown_recordings(local, releases) {
            if let Some(canonical) = self.releases.canonical_recording(&mbid).await?
                && canonical != mbid
            {
                aliases.insert(mbid, canonical);
            }
        }
        Ok(aliases)
    }
}

impl<R: ReleaseSource, F: FingerprintSource> IdentifyProviders for LiveProviders<R, F> {
    fn recall_candidates(
        &self,
        facts: &LocalAlbumFacts,
        named_group: Option<&str>,
        priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let facts = facts.clone();
        let named_group = named_group.map(str::to_owned);
        Box::pin(async move {
            let source = self.at(&facts.local_album_id, priority);
            let result = match source.recall(&facts, named_group.as_deref()).await {
                Ok(outcome) => return outcome,
                Err(Stopped::Provider(SourceError(error))) => {
                    tracing::info!(album = facts.local_album_id, %error, "identify recall deferred");
                    deferred()
                }
                Err(Stopped::Matching) => {
                    tracing::warn!(
                        album = facts.local_album_id,
                        "release matching failed; identification waits"
                    );
                    RecallResult {
                        provider_deferred: true,
                        failure_code: Some(MATCHING_FAILED.to_owned()),
                        ..RecallResult::default()
                    }
                }
            };
            RecallOutcome {
                result,
                scored: Scored::default(),
            }
        })
    }

    fn recall_release(
        &self,
        facts: &LocalAlbumFacts,
        release_mbid: &str,
        priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let facts = facts.clone();
        let release_mbid = release_mbid.to_owned();
        Box::pin(async move {
            let source = self.at(&facts.local_album_id, priority);
            let result = match source.recall_exact(&facts, &release_mbid).await {
                Ok(result) => result,
                Err(SourceError(error)) => {
                    tracing::info!(album = facts.local_album_id, %error, "exact release recall deferred");
                    deferred()
                }
            };
            RecallOutcome {
                result,
                scored: Scored::default(),
            }
        })
    }

    fn search_editions(
        &self,
        query: EditionQuery,
        priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<EditionPage, SourceError>> + Send + '_>> {
        Box::pin(async move { self.releases.at(priority).search_editions(&query).await })
    }

    fn canonical_artist(
        &self,
        mbid: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ArtistAnswer, Unanswered>> + Send + '_>> {
        let mbid = mbid.to_owned();
        let releases = self.releases.at(RequestPriority::BackgroundSync);
        Box::pin(async move { releases.canonical_artist(&mbid).await })
    }

    fn release_document(
        &self,
        release_mbid: &str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ReleaseAnswer, Unanswered>> + Send + '_>>
    {
        let release_mbid = release_mbid.to_owned();
        let releases = self.releases.at(RequestPriority::BackgroundSync);
        Box::pin(async move { releases.release_answer(&release_mbid).await })
    }
}

/// The recall answer when MusicBrainz could not be reached.
fn deferred() -> RecallResult {
    RecallResult {
        provider_deferred: true,
        failure_code: Some("musicbrainz_unavailable".to_owned()),
        ..RecallResult::default()
    }
}

/// Recall's matches by release id, scored under `aliases`.
fn keyed(
    releases: &[Release],
    scored: Vec<ReleaseMatch>,
    aliases: HashMap<String, String>,
) -> Scored {
    Scored {
        aliases,
        matches: releases
            .iter()
            .map(|release| release.id.clone())
            .zip(scored)
            .collect(),
    }
}

/// The release group the files fit best: the closest release that
/// nothing vetoes and whose names agree. `matches` are the releases'
/// matches, in order.
fn best_group(releases: &[Release], matches: &[ReleaseMatch]) -> Option<String> {
    releases
        .iter()
        .zip(matches)
        .filter(|(_, matched)| matched.conflicts.is_empty() && matched.names_agree)
        .min_by(|a, b| a.1.library_distance().total_cmp(&b.1.library_distance()))
        .map(|(release, _)| release.release_group_id.to_ascii_lowercase())
}

fn push_new(releases: &mut Vec<Release>, release: Release) {
    if !releases.iter().any(|known| known.id == release.id) {
        releases.push(release);
    }
}

fn is_various(name: &str) -> bool {
    matches!(fold(name).as_str(), "variousartists" | "various" | "va")
}

/// An artist name that stands for no artist: blank, "Unknown Artist",
/// MusicBrainz's "[unknown]", or junk a tagger left ("?", "????", "-",
/// "...", "_"). Any other name of symbols only ("!!!", "†††", "+/-") is
/// an artist: an artist phrase of `\!\!\!` finds the band's releases
/// (verified 2026-10-29 against the BrainzMash mirror,
/// api.brainzmash.cc/ws/2). That probe's title may be unique, so it shows
/// "!!!" works, not how the index treats every symbol.
fn is_placeholder(name: &str) -> bool {
    let folded = fold(name);
    if folded.is_empty() {
        return name.chars().all(|character| {
            character.is_whitespace()
                || matches!(
                    character,
                    '?' | '-' | '–' | '—' | '_' | '.' | '…' | '*' | '/' | '\\'
                )
        });
    }
    matches!(folded.as_str(), "unknownartist" | "unknown")
}

/// Whom an album's searches name.
#[derive(Debug, PartialEq, Eq)]
struct SearchArtist<'a> {
    /// The artist phrase for the album searches; empty for none.
    artist: &'a str,
    /// Rank Various Artists releases first. A search filter only, never
    /// proof of anything.
    various: bool,
    /// The recording search names the first track's own artist.
    per_track: bool,
}

/// The artist to search with: none for a compilation, the album artist,
/// or, when the album row names a placeholder, the artist more than half
/// the tracks name, counted by the credit before any "feat." tail. With
/// several artists and none over half, the album searches as Various
/// Artists. Searching for a placeholder finds nothing, or only other
/// placeholder albums.
fn search_artist(facts: &LocalAlbumFacts) -> SearchArtist<'_> {
    let named_various = is_various(&facts.album_artist_name);
    let none = |various: bool, per_track: bool| SearchArtist {
        artist: "",
        various,
        per_track,
    };
    if facts.is_compilation || named_various {
        return none(named_various, false);
    }
    if !is_placeholder(&facts.album_artist_name) {
        return SearchArtist {
            artist: facts.album_artist_name.as_str(),
            various: false,
            per_track: false,
        };
    }
    let mut counts: Vec<(String, usize, &str)> = Vec::new();
    for track in &facts.tracks {
        let name = before_featuring(track.artist_name.trim());
        if is_placeholder(name) {
            continue;
        }
        let key = fold(name);
        match counts.iter_mut().find(|(known, _, _)| *known == key) {
            Some((_, count, _)) => *count += 1,
            None => counts.push((key, 1, name)),
        }
    }
    let mut best: Option<(usize, &str)> = None;
    for (_, count, name) in &counts {
        if best.is_none_or(|(most, _)| *count > most) {
            best = Some((*count, name));
        }
    }
    match best {
        None => none(false, false),
        Some((_, name)) if is_various(name) => none(true, true),
        Some((count, name)) if counts.len() == 1 || count * 2 > facts.tracks.len() => {
            SearchArtist {
                artist: name,
                various: false,
                per_track: false,
            }
        }
        Some(_) => none(true, true),
    }
}

/// The title before a trailing "(...)" or "[...]", when something is left.
fn without_trailing_bracket(title: &str) -> &str {
    let trimmed = title.trim_end();
    let open = match trimmed.chars().last() {
        Some(')') => '(',
        Some(']') => '[',
        _ => return trimmed,
    };
    match trimmed.rfind(open) {
        Some(start) if !trimmed[..start].trim().is_empty() => trimmed[..start].trim_end(),
        _ => trimmed,
    }
}

/// The hits worth a tracklist fetch: big enough to hold the files, best
/// search score first, then the closest track count.
fn pick_hits<'a>(
    hits: &'a [ReleaseHit],
    local_tracks: usize,
    known: &[Release],
) -> Vec<&'a ReleaseHit> {
    let mut ranked: Vec<&ReleaseHit> = hits
        .iter()
        .filter(|hit| {
            hit.track_count
                .is_none_or(|count| count as usize + 2 >= local_tracks)
        })
        .filter(|hit| !known.iter().any(|release| release.answers_to(&hit.id)))
        .collect();
    ranked.sort_by(|a, b| {
        b.score.cmp(&a.score).then_with(|| {
            let gap = |hit: &ReleaseHit| {
                hit.track_count
                    .map_or(usize::MAX, |count| (count as usize).abs_diff(local_tracks))
            };
            gap(a).cmp(&gap(b))
        })
    });
    let mut per_group: HashMap<&str, usize> = HashMap::new();
    for release in known {
        *per_group
            .entry(release.release_group_id.as_str())
            .or_default() += 1;
    }
    let mut picked = Vec::new();
    for hit in ranked {
        if picked.len() >= FETCH_LIMIT {
            break;
        }
        let taken = per_group.entry(hit.release_group_id.as_str()).or_default();
        if *taken >= PER_GROUP {
            continue;
        }
        *taken += 1;
        picked.push(hit);
    }
    picked
}

/// Releases most printed tracks point at, most common first.
fn common_releases(heard: &HashMap<String, Vec<String>>, printed: usize) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for releases in heard.values() {
        for release in releases {
            *counts.entry(release.as_str()).or_default() += 1;
        }
    }
    let mut common: Vec<(&str, usize)> = counts
        .into_iter()
        .filter(|(_, count)| *count as f64 > COMMON_RELEASE_SHARE * printed as f64)
        .collect();
    common.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    common
        .into_iter()
        .take(FINGERPRINT_RELEASES)
        .map(|(id, _)| id.to_owned())
        .collect()
}

/// Scripted providers for tests: no network, fully deterministic.
#[derive(Debug, Default)]
pub struct FakeProviders {
    pub recall: std::sync::Mutex<Option<RecallResult>>,
    /// Scripted edition search page.
    pub editions: std::sync::Mutex<Option<EditionPage>>,
}

impl FakeProviders {
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_recall(result: RecallResult) -> Self {
        Self {
            recall: std::sync::Mutex::new(Some(result)),
            editions: std::sync::Mutex::new(None),
        }
    }

    /// Replace the scripted edition search page.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_editions(&self, page: EditionPage) {
        if let Ok(mut slot) = self.editions.lock() {
            *slot = Some(page);
        }
    }

    fn scripted(&self) -> RecallResult {
        self.recall
            .lock()
            .map(|slot| slot.clone().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Replace the scripted recall. Test wiring uses this to
    /// seed case-specific recall after scan assigns real track ids.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_recall(&self, result: RecallResult) {
        if let Ok(mut slot) = self.recall.lock() {
            *slot = Some(result);
        }
    }
}

impl IdentifyProviders for FakeProviders {
    /// The scripted recall, naming `named_group` unless the script names
    /// one.
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        named_group: Option<&str>,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let mut result = self.scripted();
        if result.named_group.is_none() {
            result.named_group = named_group.map(str::to_ascii_lowercase);
        }
        Box::pin(async move {
            RecallOutcome {
                result,
                scored: Scored::default(),
            }
        })
    }

    fn recall_release(
        &self,
        _facts: &LocalAlbumFacts,
        release_mbid: &str,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        let mut result = self.scripted();
        result
            .releases
            .retain(|release| release.answers_to(release_mbid));
        Box::pin(async move {
            RecallOutcome {
                result,
                scored: Scored::default(),
            }
        })
    }

    fn search_editions(
        &self,
        _query: EditionQuery,
        _priority: RequestPriority,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<EditionPage, SourceError>> + Send + '_>> {
        let page = self
            .editions
            .lock()
            .map(|slot| slot.clone().unwrap_or_default())
            .unwrap_or_default();
        Box::pin(async move { Ok(page) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored match stands in only for a scoring with recall's inputs:
    /// fingerprint support or other aliases would change the score.
    #[test]
    fn scored_matches_are_reused_only_for_the_same_inputs() {
        let release = Release {
            id: "release-1".to_owned(),
            ..Release::default()
        };
        let local = LocalAlbum::default();
        let aliases = HashMap::from([("old".to_owned(), "new".to_owned())]);
        let matched = match_release(&local, &release, &aliases);
        let scored = keyed(
            std::slice::from_ref(&release),
            vec![matched.clone()],
            aliases.clone(),
        );
        let none = HashMap::new();
        let no_aliases = HashMap::new();
        let heard = HashMap::from([("t1".to_owned(), vec!["rec".to_owned()])]);

        assert_eq!(scored.clone().take(&release, &heard, &aliases), None);
        assert_eq!(scored.clone().take(&release, &none, &no_aliases), None);
        assert_eq!(
            scored.clone().take(&release, &none, &aliases),
            Some(matched)
        );
        // Junk symbols stand for no artist; a band's symbols do not.
        assert!(is_placeholder("????"));
        assert!(!is_placeholder("!!!"));
    }
}
