//! Domain logic over the discover features: clamping, validation, and
//! assembly. Services return typed domain failures; handlers map them to
//! HTTP. Provider failures stay typed; other causes are log-only strings.

use std::sync::Arc;

use droppedneedle_auth::Admin;
use droppedneedle_kernel::page::{Page, PageRequest};
use droppedneedle_providers::ProviderError;

use crate::discover::{
    content::LiveContent,
    engine::archive::WeeksFilter,
    feedback::{self, Answers, FeedbackError},
    forget::{self, HistoryStores},
    genre::WithGenrePages,
    later::{LaterError, LaterKey, LaterPile},
    models::{
        AlbumPreviewMode, AlbumPreviewResponse, AlbumRef, ArtistRef, ChartPage, ChartSection,
        DigFurtherPick, DiscoverActivityResponse, DiscoverFeedbackEntry, DiscoverFeedbackRequest,
        DiscoverFeedbackResponse, DiscoverHealthResponse, DiscoverPickEventRequest, DiscoverPrefs,
        DiscoverResponse, DiscoverTasteResponse, DiscoverTuningRequest, DiscoverTuningsResponse,
        FoundOnDiscover, GenreDetailResponse, HomeResponse, IntegrationStatus, LaterAlbum,
        LaterRequest, ListenSource, NewRelease, PastWeeksPage, PickAnswer, PickEvidence,
        PlaylistSuggestionsRequest, PlaylistSuggestionsResponse, RadioPlanRequest,
        RadioPlanResponse, RadioRequest, RadioSeedKind, StatedArtist, StatedArtistRequest,
        StatedArtistsResponse, StatsRange, TrackPreviewResponse, WeekResponse,
        YouTubeSearchResponse, YourLastfmResponse,
    },
    new_releases::{NewReleaseLists, ReleaseKey},
    ports::{ProviderFailure, RadioFailure},
    previews::LivePreviews,
    radio::LiveRadio,
    record::{EventError, PickRecord},
    scheduler::{ArtistDetail, Feature},
    stated::{self, StatedArtists, StatedError},
    taste_page::TastePage,
    tunings::{self, TuningError, Tunings},
    youtube::LiveYouTube,
};

/// Max rows on a chart page.
pub const CHART_RANGE_LIMIT_MAX: i64 = 100;
/// Max album-preview samples.
pub const ALBUM_PREVIEW_MAX: i64 = 8;
/// Max artist/track string length accepted anywhere in discover.
pub const NAME_MAX_LEN: usize = 200;
/// Fewest radio plan tracks.
pub const RADIO_PLAN_MIN: i64 = 5;
/// Max radio plan tracks.
pub const RADIO_PLAN_MAX: i64 = 50;
/// Max seed rows in one radio plan.
pub const RADIO_ITEMS_MAX: usize = 50;
/// Max station tracks one radio plan may leave out.
pub const RADIO_EXCLUDE_MAX: usize = 500;
/// Max "More like this" and max "Not this" tracks one radio plan takes.
pub const RADIO_STEERING_MAX: usize = 50;
/// Max radio shelf rows.
pub const RADIO_SHELF_MAX: i64 = 50;

/// Domain failures. `From<ServiceError> for ApiError` in `super::error` is
/// the only place HTTP statuses are chosen.
#[derive(Debug, Clone)]
pub enum ServiceError {
    /// Unknown or foreign id.
    NotFound,
    /// Bad input with a user-facing reason.
    InvalidInput(String),
    /// An outside service failed, answered by its kind.
    Provider(ProviderError),
    /// Another source failed; the cause is log-only.
    Upstream(String),
    /// The store failed; the cause is log-only.
    Internal(String),
    /// The feature needs setup first; the sentence is shown.
    NotConfigured(String),
    /// A usage limit is spent for now; the sentence is shown.
    Exhausted(String),
}

impl From<ProviderFailure> for ServiceError {
    fn from(failure: ProviderFailure) -> Self {
        match failure {
            ProviderFailure::Provider(error) => Self::Provider(error),
            ProviderFailure::Failed(cause) => Self::Upstream(cause),
            other => owned_store_failure(other),
        }
    }
}

/// Map a failure of a store this server owns: a failed read is an internal
/// fault (500), not a source being down (503).
fn owned_store_failure(failure: ProviderFailure) -> ServiceError {
    match failure {
        ProviderFailure::Provider(error) => ServiceError::Provider(error),
        ProviderFailure::Failed(cause) => ServiceError::Internal(cause),
        ProviderFailure::NotConfigured(sentence) => ServiceError::NotConfigured(sentence),
        ProviderFailure::Exhausted(sentence) => ServiceError::Exhausted(sentence),
    }
}

/// All dependencies behind discover, injected by constructor.
#[derive(Clone)]
pub struct DiscoverDeps {
    /// Shelf content.
    pub content: Arc<LiveContent>,
    /// What people told Discover.
    pub answers: Arc<Answers>,
    /// Chart pages and genre detail.
    pub charts: Arc<WithGenrePages>,
    /// 30-second previews.
    pub previews: Arc<LivePreviews>,
    /// YouTube lookups.
    pub youtube: Arc<LiveYouTube>,
    /// Radio plans and suggestions.
    pub radio: Arc<LiveRadio>,
    /// The pick record's events from the web UI.
    pub record: PickRecord,
    /// What "Forget my history" deletes from.
    pub history: HistoryStores,
    /// The kept facts' refresh job, for the jobs table to run.
    pub facts: Arc<crate::discover::facts::job::FactJobs>,
    /// The fresh releases' daily read, for the jobs table to run.
    pub fresh: Arc<crate::discover::listening::fresh::FreshFeedReads>,
    /// The taste job, for the jobs table to run.
    pub taste: Arc<crate::discover::engine::jobs::TasteJobs>,
    /// The pool job, for the jobs table to run.
    pub pool: Arc<crate::discover::engine::pool::PoolJobs>,
    /// What "Why this?" opens for a pick.
    pub evidence: crate::discover::engine::evidence::Evidence,
    /// How people tuned what Discover offers them.
    pub tunings: Tunings,
    /// Each person's taste, as their page shows it.
    pub taste_page: TastePage,
    /// Each person's Later pile.
    pub later: LaterPile,
    /// The artists each person named as ones they love.
    pub stated: StatedArtists,
    /// Each person's new releases from their artists.
    pub new_releases: NewReleaseLists,
    /// Each person's weekly sets: the job kind and the routes' answers.
    pub weekly: Arc<crate::discover::engine::weekly::WeeklySets>,
    /// Each person's past weeks and what they kept from them.
    pub past_weeks: crate::discover::engine::archive::PastWeeks,
}

/// Clamp a limit into `1..=max`, defaulting when missing.
fn clamp_limit(value: Option<i64>, default: i64, max: i64) -> i64 {
    value.unwrap_or(default).clamp(1, max)
}

/// Reject blank or overlong names with a user-facing reason.
fn check_name(value: &str, what: &str) -> Result<(), ServiceError> {
    if value.trim().is_empty() {
        return Err(ServiceError::InvalidInput(format!(
            "{what} must not be blank"
        )));
    }
    if value.len() > NAME_MAX_LEN {
        return Err(ServiceError::InvalidInput(format!(
            "{what} must be at most {NAME_MAX_LEN} characters"
        )));
    }
    Ok(())
}

/// Cached discover shelves for the user. The cache is ours, so a failed
/// read is an internal fault (500), not a source being down (503).
pub async fn discover(
    deps: &DiscoverDeps,
    user_id: &str,
) -> Result<DiscoverResponse, ServiceError> {
    deps.content
        .discover(user_id)
        .await
        .map_err(owned_store_failure)
}

/// Every user's latest discover page numbers, for an admin. The
/// rows are ours, so a failed read is an internal fault (500).
pub async fn discover_health(
    deps: &DiscoverDeps,
    _admin: &Admin,
) -> Result<DiscoverHealthResponse, ServiceError> {
    deps.content.health().await.map_err(owned_store_failure)
}

/// Record one discover interaction.
pub async fn record_activity(
    deps: &DiscoverDeps,
    user_id: &str,
    body: &crate::discover::models::DiscoverActivityRequest,
) -> Result<DiscoverActivityResponse, ServiceError> {
    let Some(feature) = Feature::parse(&body.feature) else {
        return Err(ServiceError::InvalidInput(format!(
            "Unknown feature '{}'; want home, discover, queue, or artist",
            body.feature
        )));
    };
    if let Some(section) = body.section.as_deref()
        && !matches!(section, "similar" | "top_songs" | "top_albums")
    {
        return Err(ServiceError::InvalidInput(format!(
            "Unknown section '{section}'; want similar, top_songs, or top_albums"
        )));
    }
    if feature == Feature::Artist {
        let valid_artist = body
            .artist_mbid
            .as_deref()
            .is_some_and(droppedneedle_providers::musicbrainz::is_valid_mbid);
        if !valid_artist || body.section.is_none() || body.provider.is_none() {
            return Err(ServiceError::InvalidInput(
                "Artist activity needs an artist MBID, a section and a provider".to_owned(),
            ));
        }
    } else if body.artist_mbid.is_some() || body.section.is_some() || body.provider.is_some() {
        return Err(ServiceError::InvalidInput(
            "Artist, section and provider only go with the artist feature".to_owned(),
        ));
    }
    Ok(deps
        .content
        .record_activity(
            user_id,
            feature,
            ArtistDetail {
                artist_mbid: body.artist_mbid.as_deref(),
                section: body.section.as_deref(),
                provider: body.provider.map(ListenSource::as_str),
            },
        )
        .await?)
}

/// One radio shelf.
pub async fn radio_shelf(
    deps: &DiscoverDeps,
    user_id: &str,
    body: &RadioRequest,
) -> Result<ChartSection, ServiceError> {
    if matches!(
        body.seed_type,
        RadioSeedKind::Items | RadioSeedKind::Track | RadioSeedKind::Playlist
    ) {
        return Err(ServiceError::InvalidInput(
            "A radio shelf starts from an artist, an album or a genre".to_owned(),
        ));
    }
    if body.seed_id.trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "seed_id must not be blank".to_owned(),
        ));
    }
    let request = RadioRequest {
        count: body.count.clamp(1, RADIO_SHELF_MAX),
        ..body.clone()
    };
    deps.radio
        .shelf(user_id, &request)
        .await
        .map_err(radio_failure)
}

/// One complete radio plan.
pub async fn radio_plan(
    deps: &DiscoverDeps,
    user_id: &str,
    body: &RadioPlanRequest,
) -> Result<RadioPlanResponse, ServiceError> {
    if body.seed_type != RadioSeedKind::Items
        && body.seed_id.as_deref().unwrap_or("").trim().is_empty()
    {
        return Err(ServiceError::InvalidInput(
            "seed_id must not be blank for artist, album, genre, track and playlist plans"
                .to_owned(),
        ));
    }
    if body.items.len() > RADIO_ITEMS_MAX {
        return Err(ServiceError::InvalidInput(format!(
            "A radio plan takes at most {RADIO_ITEMS_MAX} seed items"
        )));
    }
    if body.station_tracks.len() > RADIO_EXCLUDE_MAX {
        return Err(ServiceError::InvalidInput(format!(
            "A radio plan can leave out at most {RADIO_EXCLUDE_MAX} of the station's tracks"
        )));
    }
    if body.more_like.len() > RADIO_STEERING_MAX || body.not_this.len() > RADIO_STEERING_MAX {
        return Err(ServiceError::InvalidInput(format!(
            "A radio plan takes at most {RADIO_STEERING_MAX} tracks to play more like and \
             {RADIO_STEERING_MAX} to leave out"
        )));
    }
    let request = RadioPlanRequest {
        count: body.count.clamp(RADIO_PLAN_MIN, RADIO_PLAN_MAX),
        ..body.clone()
    };
    deps.radio
        .plan(user_id, &request)
        .await
        .map_err(radio_failure)
}

/// Suggestions extending one playlist.
pub async fn playlist_suggestions(
    deps: &DiscoverDeps,
    user_id: &str,
    body: &PlaylistSuggestionsRequest,
) -> Result<PlaylistSuggestionsResponse, ServiceError> {
    if body.playlist_id.trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "playlist_id must not be blank".to_owned(),
        ));
    }
    let request = PlaylistSuggestionsRequest {
        count: body.count.clamp(1, RADIO_SHELF_MAX),
        ..body.clone()
    };
    deps.radio
        .playlist_suggestions(user_id, &request)
        .await
        .map_err(radio_failure)
}

fn radio_failure(failure: RadioFailure) -> ServiceError {
    match failure {
        RadioFailure::NotFound => ServiceError::NotFound,
        RadioFailure::Invalid(message) => ServiceError::InvalidInput(message),
        RadioFailure::Provider(failure) => failure.into(),
        RadioFailure::Store(cause) => ServiceError::Internal(cause),
    }
}

/// Record one thing the user told Discover and ask discover to refresh.
/// Answers it as stored.
pub async fn save_feedback(
    deps: &DiscoverDeps,
    user_id: &str,
    body: &DiscoverFeedbackRequest,
) -> Result<DiscoverFeedbackEntry, ServiceError> {
    let stored = deps
        .answers
        .save(user_id, body)
        .await
        .map_err(|error| match error {
            FeedbackError::Invalid(reason) => ServiceError::InvalidInput(reason),
            FeedbackError::Store(error) => {
                ServiceError::Internal(format!("feedback save: {error}"))
            }
        })?;
    deps.content.trigger_refresh(user_id).await?;
    Ok(stored)
}

/// Stamp what the web UI saw happen to one of the user's picks.
pub async fn record_pick_event(
    deps: &DiscoverDeps,
    user_id: &str,
    body: &DiscoverPickEventRequest,
) -> Result<(), ServiceError> {
    deps.record
        .event(user_id, &body.release_group_mbid, body.event)
        .await
        .map_err(|error| match error {
            EventError::Invalid(reason) => ServiceError::InvalidInput(reason),
            EventError::Store(error) => ServiceError::Internal(format!("pick event: {error}")),
        })
}

/// Take back one thing the user told Discover. What it hid can come up
/// again in later builds.
pub async fn undo_feedback(
    deps: &DiscoverDeps,
    user_id: &str,
    target: &str,
    mbid: &str,
) -> Result<(), ServiceError> {
    let (target, mbid) =
        feedback::checked_target(target, mbid).map_err(ServiceError::InvalidInput)?;
    deps.answers
        .undo(user_id, target, &mbid)
        .await
        .map_err(|error| ServiceError::Internal(format!("feedback undo: {error}")))
}

/// Forget the user's history (see [`forget`]). An error means a step
/// failed and the steps before it are done; asking again finishes it.
pub async fn forget_history(deps: &DiscoverDeps, user_id: &str) -> Result<(), ServiceError> {
    let forgotten = forget::forget(deps, user_id)
        .await
        .map_err(|error| ServiceError::Internal(error.to_string()))?;
    tracing::info!(
        playback_events = forgotten.playback_events,
        imported_listens = forgotten.imported_listens,
        picks = forgotten.picks,
        activity = forgotten.activity,
        taste = forgotten.taste,
        pool = forgotten.pool,
        weekly_sets = forgotten.weekly_sets,
        pages = forgotten.pages,
        "a person's history forgotten"
    );
    Ok(())
}

/// Everything the user told Discover that is in effect, newest first.
pub async fn list_feedback(
    deps: &DiscoverDeps,
    user_id: &str,
) -> Result<DiscoverFeedbackResponse, ServiceError> {
    let items = deps
        .answers
        .list(user_id)
        .await
        .map_err(|error| ServiceError::Internal(format!("feedback read: {error}")))?;
    Ok(DiscoverFeedbackResponse { items })
}

/// The evidence behind one of the user's picks: as built for the newest of
/// their weekly sets that held it, else from their pool's stored rows. Not
/// found when neither holds it, or the pool's reason no longer holds.
pub async fn pick_evidence(
    deps: &DiscoverDeps,
    user_id: &str,
    release_group_mbid: &str,
) -> Result<PickEvidence, ServiceError> {
    let mbid = release_group_mbid.trim();
    if !droppedneedle_providers::musicbrainz::is_valid_mbid(mbid) {
        return Err(ServiceError::InvalidInput(
            "release_group_mbid must be a MusicBrainz id".to_owned(),
        ));
    }
    deps.evidence
        .of(user_id, &mbid.to_ascii_lowercase())
        .await
        .map_err(|error| ServiceError::Internal(format!("pick evidence: {error}")))?
        .ok_or(ServiceError::NotFound)
}

fn tuning_failure(error: TuningError) -> ServiceError {
    match error {
        TuningError::Invalid(reason) => ServiceError::InvalidInput(reason),
        TuningError::Store(error) => ServiceError::Internal(format!("tuning: {error}")),
    }
}

/// Keep one way the user tuned Discover, about one artist or a group;
/// what it changes is built again.
pub async fn save_tuning(
    deps: &DiscoverDeps,
    user_id: &str,
    body: &DiscoverTuningRequest,
) -> Result<DiscoverTuningsResponse, ServiceError> {
    let items = deps
        .tunings
        .save(user_id, body)
        .await
        .map_err(tuning_failure)?;
    Ok(DiscoverTuningsResponse { items })
}

/// Every way the user tuned Discover, newest first.
pub async fn list_tunings(
    deps: &DiscoverDeps,
    user_id: &str,
) -> Result<DiscoverTuningsResponse, ServiceError> {
    let items = deps
        .tunings
        .list(user_id)
        .await
        .map_err(|error| ServiceError::Internal(format!("tunings read: {error}")))?;
    Ok(DiscoverTuningsResponse { items })
}

/// Take back one way the user tuned Discover, or with `group` the whole
/// statement it was part of; what it changed is built again.
pub async fn undo_tuning(
    deps: &DiscoverDeps,
    user_id: &str,
    kind: &str,
    target: &str,
    group: bool,
) -> Result<(), ServiceError> {
    let (kind, target) = tunings::checked_path(kind, target).map_err(tuning_failure)?;
    deps.tunings
        .undo(user_id, kind, &target, group)
        .await
        .map_err(tuning_failure)
}

/// The user's taste, as their page shows it.
pub async fn taste(
    deps: &DiscoverDeps,
    user_id: &str,
) -> Result<DiscoverTasteResponse, ServiceError> {
    deps.taste_page
        .read(user_id)
        .await
        .map_err(|error| ServiceError::Internal(format!("taste read: {error}")))
}

fn later_failure(error: LaterError) -> ServiceError {
    match error {
        LaterError::Invalid(reason) => ServiceError::InvalidInput(reason),
        LaterError::Store(error) => ServiceError::Internal(format!("later pile: {error}")),
    }
}

/// Put one album on the user's Later pile. Nothing is requested.
pub async fn mark_later(
    deps: &DiscoverDeps,
    user_id: &str,
    release_group_mbid: &str,
    body: &LaterRequest,
) -> Result<(), ServiceError> {
    check_name(&body.title, "title")?;
    check_name(&body.artist_name, "artist_name")?;
    deps.later
        .mark(user_id, release_group_mbid, body)
        .await
        .map_err(later_failure)
}

/// Take one album off the user's Later pile.
pub async fn remove_later(
    deps: &DiscoverDeps,
    user_id: &str,
    release_group_mbid: &str,
) -> Result<(), ServiceError> {
    deps.later
        .remove(user_id, release_group_mbid)
        .await
        .map_err(later_failure)
}

/// One page of the user's Later pile, newest first; only one album when
/// `only` names it.
pub async fn later_page(
    deps: &DiscoverDeps,
    user_id: &str,
    paging: PageRequest<LaterKey>,
    only: Option<&str>,
) -> Result<Page<LaterAlbum>, ServiceError> {
    deps.later
        .page(user_id, paging, only)
        .await
        .map_err(later_failure)
}

fn stated_failure(error: StatedError) -> ServiceError {
    match error {
        StatedError::Invalid(reason) => ServiceError::InvalidInput(reason),
        StatedError::Full => {
            ServiceError::InvalidInput(format!("You can name at most {} artists", stated::LIMIT))
        }
        StatedError::Store(error) => ServiceError::Internal(format!("stated artists: {error}")),
    }
}

/// The artists the user named as ones they love, in the order they named
/// them.
pub async fn list_stated(
    deps: &DiscoverDeps,
    user_id: &str,
) -> Result<StatedArtistsResponse, ServiceError> {
    let items = deps
        .stated
        .list(user_id)
        .await
        .map_err(|error| ServiceError::Internal(format!("stated artists read: {error}")))?;
    Ok(StatedArtistsResponse {
        items,
        limit: stated::LIMIT,
    })
}

/// Name one artist the user loves; their taste is built again.
pub async fn name_stated(
    deps: &DiscoverDeps,
    user_id: &str,
    artist_mbid: &str,
    body: &StatedArtistRequest,
) -> Result<StatedArtist, ServiceError> {
    check_name(&body.artist_name, "artist_name")?;
    deps.stated
        .name(user_id, artist_mbid, &body.artist_name)
        .await
        .map_err(stated_failure)
}

/// Take one artist off the user's list; their taste is built again.
pub async fn remove_stated(
    deps: &DiscoverDeps,
    user_id: &str,
    artist_mbid: &str,
) -> Result<(), ServiceError> {
    deps.stated
        .remove(user_id, artist_mbid)
        .await
        .map_err(stated_failure)
}

/// One page of the user's new releases from their artists.
pub async fn new_releases_page(
    deps: &DiscoverDeps,
    user_id: &str,
    paging: PageRequest<ReleaseKey>,
) -> Result<Page<NewRelease>, ServiceError> {
    deps.new_releases
        .page(user_id, paging)
        .await
        .map_err(|error| ServiceError::Internal(format!("new releases: {error}")))
}

/// The user's week: where it stands and the set to show.
pub async fn week(deps: &DiscoverDeps, user_id: &str) -> Result<WeekResponse, ServiceError> {
    deps.weekly
        .week(user_id)
        .await
        .map_err(|error| ServiceError::Internal(format!("weekly set: {error}")))
}

/// One page of "dig further" past the user's set.
pub async fn week_more(
    deps: &DiscoverDeps,
    user_id: &str,
    paging: PageRequest<u64>,
) -> Result<Page<DigFurtherPick>, ServiceError> {
    deps.weekly
        .more(user_id, paging)
        .await
        .map_err(|error| ServiceError::Internal(format!("dig further: {error}")))
}

/// One page of the user's past weeks, the picks narrowed to `answer` and
/// to titles or artists holding `text`.
pub async fn past_weeks(
    deps: &DiscoverDeps,
    user_id: &str,
    paging: PageRequest<i64>,
    answer: Option<PickAnswer>,
    text: Option<&str>,
) -> Result<PastWeeksPage, ServiceError> {
    let text = text.map(str::trim).filter(|text| !text.is_empty());
    if let Some(text) = text {
        check_name(text, "search")?;
    }
    let filter = WeeksFilter {
        answer,
        text: text.map(str::to_lowercase),
    };
    deps.past_weeks
        .page(user_id, paging, filter)
        .await
        .map_err(|error| ServiceError::Internal(format!("past weeks: {error}")))
}

/// The albums the user kept from their sets.
pub async fn found_on_discover(
    deps: &DiscoverDeps,
    user_id: &str,
) -> Result<FoundOnDiscover, ServiceError> {
    deps.past_weeks
        .found(user_id)
        .await
        .map_err(|error| ServiceError::Internal(format!("found on discover: {error}")))
}

/// The user's set day and depth.
pub async fn discover_prefs(
    deps: &DiscoverDeps,
    user_id: &str,
) -> Result<DiscoverPrefs, ServiceError> {
    deps.weekly
        .prefs(user_id)
        .await
        .map_err(|error| ServiceError::Internal(format!("discover prefs: {error}")))
}

/// Keep the user's set day and depth; the week running keeps its set.
pub async fn save_discover_prefs(
    deps: &DiscoverDeps,
    user_id: &str,
    prefs: DiscoverPrefs,
) -> Result<DiscoverPrefs, ServiceError> {
    deps.weekly
        .save_prefs(user_id, prefs)
        .await
        .map_err(|error| ServiceError::Internal(format!("discover prefs: {error}")))
}

/// Track video lookup with the cached flag.
pub async fn youtube_track_search(
    deps: &DiscoverDeps,
    artist: &str,
    track: &str,
) -> Result<YouTubeSearchResponse, ServiceError> {
    check_name(artist, "artist")?;
    check_name(track, "track")?;
    let cached = deps.youtube.is_cached(artist, track, true).await;
    match deps.youtube.search_track(artist, track).await? {
        Some(video_id) => Ok(YouTubeSearchResponse {
            video_id: Some(video_id.clone()),
            embed_url: Some(format!("https://www.youtube.com/embed/{video_id}")),
            error: None,
            cached,
        }),
        None => Ok(YouTubeSearchResponse {
            video_id: None,
            embed_url: None,
            error: Some("not_found".to_owned()),
            cached,
        }),
    }
}

/// A 30-second track preview. Empty means no provider had one.
pub async fn track_preview(
    deps: &DiscoverDeps,
    artist: &str,
    track: &str,
) -> Result<TrackPreviewResponse, ServiceError> {
    check_name(artist, "artist")?;
    check_name(track, "track")?;
    let artist: String = artist.chars().take(NAME_MAX_LEN).collect();
    let track: String = track.chars().take(NAME_MAX_LEN).collect();
    Ok(deps.previews.track_preview(&artist, &track).await?)
}

/// 30-second album samples: the first tracks, or an audition's three clips.
pub async fn album_preview(
    deps: &DiscoverDeps,
    artist: &str,
    album: &str,
    count: Option<i64>,
    mode: AlbumPreviewMode,
) -> Result<AlbumPreviewResponse, ServiceError> {
    check_name(artist, "artist")?;
    check_name(album, "album")?;
    let artist: String = artist.chars().take(NAME_MAX_LEN).collect();
    let album: String = album.chars().take(NAME_MAX_LEN).collect();
    let count = clamp_limit(count, 4, ALBUM_PREVIEW_MAX);
    let (tracks, provider) = deps
        .previews
        .album_preview(&artist, &album, count, mode)
        .await?;
    Ok(AlbumPreviewResponse { tracks, provider })
}

/// Cached home shelves for the user. Same ownership rule as discover:
/// failed cache reads are internal faults.
pub async fn home(deps: &DiscoverDeps, user_id: &str) -> Result<HomeResponse, ServiceError> {
    deps.content
        .home(user_id)
        .await
        .map_err(owned_store_failure)
}

/// Integration availability, refined by a live local-files check that
/// never blanks the row on failure (the cached value wins on error).
pub async fn integration_status(deps: &DiscoverDeps) -> Result<IntegrationStatus, ServiceError> {
    let mut status = deps.content.integration_status().await?;
    if let Ok(has_local) = deps.content.has_local_files().await {
        status.localfiles = has_local;
    }
    Ok(status)
}

/// Genre detail with owned and popular rows.
pub async fn genre_detail(
    deps: &DiscoverDeps,
    genre: &str,
    limit: Option<i64>,
    artist_offset: Option<i64>,
    album_offset: Option<i64>,
) -> Result<GenreDetailResponse, ServiceError> {
    check_name(genre, "genre")?;
    let limit = clamp_limit(limit, 50, 200);
    let artist_offset = artist_offset.unwrap_or(0).max(0);
    let album_offset = album_offset.unwrap_or(0).max(0);
    Ok(deps
        .charts
        .genre_detail(genre, limit, artist_offset, album_offset)
        .await?)
}

/// One trending-artists page. One route with `?range=` serves every range.
/// No `range` means this week; the home root still carries teasers for the
/// overview use case.
pub async fn trending_artists(
    deps: &DiscoverDeps,
    user_id: &str,
    range: Option<StatsRange>,
    limit: Option<i64>,
    offset: Option<i64>,
    source: Option<ListenSource>,
) -> Result<ChartPage<ArtistRef>, ServiceError> {
    let range = range.unwrap_or_default();
    let source = source.unwrap_or_default();
    let limit = clamp_limit(limit, 25, CHART_RANGE_LIMIT_MAX);
    let offset = offset.unwrap_or(0).max(0);
    Ok(deps
        .charts
        .trending_artists(user_id, range, limit, offset, source)
        .await?)
}

/// One popular-albums page, with the range param.
pub async fn popular_albums(
    deps: &DiscoverDeps,
    user_id: &str,
    range: Option<StatsRange>,
    limit: Option<i64>,
    offset: Option<i64>,
    source: Option<ListenSource>,
) -> Result<ChartPage<AlbumRef>, ServiceError> {
    let range = range.unwrap_or_default();
    let source = source.unwrap_or_default();
    let limit = clamp_limit(limit, 25, CHART_RANGE_LIMIT_MAX);
    let offset = offset.unwrap_or(0).max(0);
    Ok(deps
        .charts
        .popular_albums(user_id, range, limit, offset, source)
        .await?)
}

/// The caller's own Last.fm lists, for their charts page.
pub async fn your_lastfm(
    deps: &DiscoverDeps,
    user_id: &str,
) -> Result<YourLastfmResponse, ServiceError> {
    Ok(deps.charts.your_lastfm(user_id).await?)
}

/// One your-top-albums page for the user.
pub async fn your_top_albums(
    deps: &DiscoverDeps,
    user_id: &str,
    range: Option<StatsRange>,
    limit: Option<i64>,
    offset: Option<i64>,
    source: Option<ListenSource>,
) -> Result<ChartPage<AlbumRef>, ServiceError> {
    let range = range.unwrap_or_default();
    let source = source.unwrap_or_default();
    let limit = clamp_limit(limit, 25, CHART_RANGE_LIMIT_MAX);
    let offset = offset.unwrap_or(0).max(0);
    Ok(deps
        .charts
        .your_top_albums(user_id, range, limit, offset, source)
        .await?)
}
