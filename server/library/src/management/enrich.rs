//! What Library Management asks the outside world for while it plans an
//! album: ListenBrainz and Last.fm genres, LRCLIB lyrics, and the front
//! cover from the artwork cache. Planning runs on a blocking worker
//! thread, so each call is driven to completion there with a time limit;
//! an answer that does not come in time counts as *deferred*, never as
//! "nothing there", so a partial answer never replaces what a file holds.

use std::time::Duration;

use futures_util::future::BoxFuture;

use super::genre::Candidate;
use crate::identify::sqlite::SqliteIdentifyStore;
use crate::identify::stores::RELEASE_FRESH_SECS;
use crate::matching::Release;
use crate::matching::model::{RELATIONS_VERSION, ReleaseExtras};
use droppedneedle_kernel::runtime_config::sections::RelationshipType;

/// Longest one outside call may take while planning.
const CALL_LIMIT: Duration = Duration::from_secs(20);

/// What LRCLIB is asked for one track.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricsQuery {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_seconds: f64,
}

/// Lyrics LRCLIB matched exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LyricsFound {
    pub plain: Option<String>,
    /// LRC text, one timed line per line.
    pub synced: Option<String>,
}

/// A front cover image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// The outside sources planning reads. `Err` means the source could not
/// answer now; `Ok(None)` (or an empty list) that it has nothing.
pub trait EnrichmentPort: Send + Sync {
    /// `background` asks at background priority (automatic runs).
    fn listenbrainz_genres<'a>(
        &'a self,
        release_group_mbid: &'a str,
        background: bool,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>>;
    /// `background` asks at background priority (automatic runs).
    fn lastfm_genres<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
        background: bool,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>>;
    /// `background` asks at background priority (automatic runs).
    fn lyrics<'a>(
        &'a self,
        query: &'a LyricsQuery,
        background: bool,
    ) -> BoxFuture<'a, Result<Option<LyricsFound>, String>>;
    /// `background` asks at background priority (automatic runs).
    fn front_cover<'a>(
        &'a self,
        release_mbid: &'a str,
        release_group_mbid: &'a str,
        background: bool,
    ) -> BoxFuture<'a, Result<Option<Cover>, String>>;
    /// The parts of a release `wants` names, in one MusicBrainz lookup:
    /// its recording and work credits, its credited artists' own names
    /// and aliases. `Err` when MusicBrainz could not answer or no longer
    /// has the release: nothing was fetched, which is never the same as
    /// "there are none". `background` asks at background priority
    /// (automatic runs).
    fn release_extras<'a>(
        &'a self,
        release_mbid: &'a str,
        wants: ExtrasWanted,
        background: bool,
    ) -> BoxFuture<'a, Result<ReleaseExtras, String>>;
    /// Look up to [`MAX_BATCH`](droppedneedle_providers::acoustid::MAX_BATCH)
    /// AcoustID prints in one request. An empty list asks only whether a
    /// key is set: `NoKey`, or an empty answer.
    fn acoustid_lookup<'a>(&'a self, prints: &'a [AcoustIdPrint]) -> BoxFuture<'a, AcoustIdAnswer>;
}

/// An AcoustID print of a file's audio, with its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcoustIdPrint {
    pub print: String,
    pub duration_seconds: u32,
}

/// What AcoustID said about a list of prints.
#[derive(Debug, Clone, PartialEq)]
pub enum AcoustIdAnswer {
    /// No key in Settings, or AcoustID refused the one there.
    NoKey,
    /// AcoustID could not be reached, or its answer was unreadable.
    Unavailable,
    /// The results for each print, in the order asked.
    Answered(Vec<Vec<droppedneedle_providers::acoustid::BatchResult>>),
}

/// Which parts of a release one lookup asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtrasWanted {
    pub relations: bool,
    pub names: bool,
}

/// Whether a plan has a part of the release it asks for (the credits, the
/// artist names).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Credits {
    /// The profile does not need the part, the edition is a custom one,
    /// or no outside sources are attached (a test bundle).
    #[default]
    NotAsked,
    /// The stored document carries it, fetched within the week.
    Ready,
    /// A refetch failed, so the older stored part is used.
    Stale,
    /// It could not be fetched now.
    Unavailable,
}

/// The release parts a plan asks for, and whether each is there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReleaseParts {
    pub credits: Credits,
    pub names: Credits,
}

impl ReleaseParts {
    /// A part the plan needs could not be fetched now.
    pub fn unavailable(&self) -> bool {
        self.credits == Credits::Unavailable || self.names == Credits::Unavailable
    }
}

/// The profile fields filled from MusicBrainz credits, with the
/// Relationships type each needs (`None`: work fields, which ignore those
/// settings).
pub const CREDIT_FIELDS: [(&str, Option<RelationshipType>); 14] = [
    ("composer", Some(RelationshipType::Composer)),
    ("lyricist", Some(RelationshipType::Lyricist)),
    ("conductor", Some(RelationshipType::Conductor)),
    ("performer", Some(RelationshipType::Performer)),
    ("arranger", Some(RelationshipType::Arranger)),
    ("remixer", Some(RelationshipType::Remixer)),
    ("producer", Some(RelationshipType::Producer)),
    ("engineer", Some(RelationshipType::Other)),
    ("mixer", Some(RelationshipType::Other)),
    ("dj_mixer", Some(RelationshipType::Other)),
    ("writer", Some(RelationshipType::Other)),
    ("director", Some(RelationshipType::Other)),
    ("work", None),
    ("musicbrainz_work_id", None),
];

/// The first [`ReleaseRelations`](crate::matching::model::ReleaseRelations)
/// version that carries the "Other roles" credits. Older stored credits
/// never do, so with them those fields keep the file's values.
pub const OTHER_ROLES_RELATIONS_VERSION: u32 = 2;

/// Whether a profile field is one of the "Other roles" credit fields.
pub fn other_role_field(field: &str) -> bool {
    CREDIT_FIELDS
        .iter()
        .any(|(name, kind)| *name == field && *kind == Some(RelationshipType::Other))
}

/// Whether the profile writes "Other roles": the Relationships switch is
/// on and Other is ticked.
pub fn other_roles(
    profile: &droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
) -> bool {
    let relationships = &profile.metadata.relationships;
    relationships.enabled && relationships.types.contains(&RelationshipType::Other)
}

/// Whether the profile asks for a credit field: `None` when the name is
/// not one, `Some(false)` when its Relationships type or switch is off.
pub fn credit_asked(
    profile: &droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
    field: &str,
) -> Option<bool> {
    let (_, kind) = CREDIT_FIELDS.iter().find(|(name, _)| *name == field)?;
    let relationships = &profile.metadata.relationships;
    Some(kind.is_none_or(|kind| relationships.enabled && relationships.types.contains(&kind)))
}

/// Whether planning with this profile needs the credits: a credit field
/// it asks for is managed and not preserved.
pub fn credits_wanted(
    profile: &droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
) -> bool {
    use droppedneedle_kernel::runtime_config::sections::FieldMode;
    let metadata = &profile.metadata;
    metadata.enabled
        && metadata.fields.iter().any(|field| {
            !matches!(field.mode, FieldMode::Disabled | FieldMode::Preserve)
                && credit_asked(profile, &field.field) == Some(true)
                && !metadata.preserve_fields.contains(&field.field)
        })
}

/// The tag fields whose values are artist names from the release's and
/// the track's credits.
pub const ARTIST_FIELDS: [&str; 6] = [
    "artist",
    "artists",
    "artist_sort",
    "album_artist",
    "album_artists",
    "album_artist_sort",
];

/// Whether planning with this profile needs the credited artists' own
/// MusicBrainz names: it writes MusicBrainz or translated names into an
/// artist field it manages and does not preserve.
pub fn names_wanted(
    profile: &droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
) -> bool {
    use droppedneedle_kernel::runtime_config::sections::{ArtistStandardization, FieldMode};
    let metadata = &profile.metadata;
    let credits = &metadata.artist_credits;
    metadata.enabled
        && (credits.standardization == ArtistStandardization::Canonical || credits.translate_names)
        && metadata.fields.iter().any(|field| {
            !matches!(field.mode, FieldMode::Disabled | FieldMode::Preserve)
                && ARTIST_FIELDS.contains(&field.field.as_str())
                && !metadata.preserve_fields.contains(&field.field)
        })
}

/// Make sure the stored document for `release` carries the parts the
/// profile needs (credits, artist names), fresh: when one is missing,
/// older than [`RELEASE_FRESH_SECS`] or parsed under an older version,
/// every wanted part is fetched in one lookup, grafted onto the stored
/// document and put on `release`. Never inside a transaction. A stored
/// part is kept and used when its refetch fails.
pub fn ensure_release_extras(
    port: Option<&dyn EnrichmentPort>,
    store: &SqliteIdentifyStore,
    profile: &droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
    release: &mut Release,
    background: bool,
) -> ReleaseParts {
    let Some(port) = port else {
        return ReleaseParts::default();
    };
    if super::custom::is_custom(release) {
        return ReleaseParts::default();
    }
    let wants = ExtrasWanted {
        relations: credits_wanted(profile),
        names: names_wanted(profile),
    };
    let now_ms = store.clock().now_ms();
    let fresh_ms = (RELEASE_FRESH_SECS * 1000) as i64;
    let relations_fresh = release
        .recording_relations
        .as_ref()
        .is_some_and(|relations| {
            now_ms - relations.fetched_at_ms < fresh_ms && relations.version >= RELATIONS_VERSION
        });
    let names_fresh = release
        .artist_names
        .as_ref()
        .is_some_and(|names| now_ms - names.fetched_at_ms < fresh_ms);
    let state = |wanted: bool, ready: bool| match (wanted, ready) {
        (false, _) => Credits::NotAsked,
        (true, true) => Credits::Ready,
        (true, false) => Credits::Unavailable,
    };
    if (!wants.relations || relations_fresh) && (!wants.names || names_fresh) {
        return ReleaseParts {
            credits: state(wants.relations, true),
            names: state(wants.names, true),
        };
    }
    let fetched = wait(port.release_extras(&release.id, wants, background));
    let mut fetched = fetched.unwrap_or_default();
    if !wants.relations {
        fetched.relations = None;
    }
    if !wants.names {
        fetched.names = None;
    }
    if let Some(relations) = fetched.relations.as_mut() {
        relations.fetched_at_ms = now_ms;
    }
    if let Some(names) = fetched.names.as_mut() {
        names.fetched_at_ms = now_ms;
    }
    let got_relations = fetched.relations.is_some();
    let got_names = fetched.names.is_some();
    if got_relations || got_names {
        // A stored document that cannot be updated still gets the fetched
        // parts for this run; the next run fetches them again.
        match store.graft_extras(&release.id, &fetched) {
            Ok(Some(grafted)) => *release = grafted,
            Ok(None) => release.graft_extras(&fetched),
            Err(error) => {
                tracing::warn!(%error, release = release.id, "fetched parts not kept on file");
                release.graft_extras(&fetched);
            }
        }
    }
    let outcome = |wanted: bool, got: bool, fresh: bool, stored: bool, part: &str| match (
        wanted,
        got || fresh,
        stored,
    ) {
        (false, _, _) => Credits::NotAsked,
        (true, true, _) => Credits::Ready,
        (true, false, true) => {
            tracing::info!(
                release = release.id,
                part,
                "could not be refreshed; the stored one is used"
            );
            Credits::Stale
        }
        (true, false, false) => Credits::Unavailable,
    };
    ReleaseParts {
        credits: outcome(
            wants.relations,
            got_relations,
            relations_fresh,
            release.recording_relations.is_some(),
            "credits",
        ),
        names: outcome(
            wants.names,
            got_names,
            names_fresh,
            release.artist_names.is_some(),
            "artist names",
        ),
    }
}

/// Drive one call to completion from the blocking planning thread. `None`
/// when there is no runtime, the call fails, or it runs out of time.
pub fn wait<T>(call: BoxFuture<'_, Result<T, String>>) -> Option<T> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    // Planning runs on a blocking thread, outside any async context.
    let outcome =
        match droppedneedle_kernel::db::block_on(&handle, "management_enrichment", async {
            tokio::time::timeout(CALL_LIMIT, call).await
        }) {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::error!(%error, "library management enrichment not asked");
                return None;
            }
        };
    match outcome {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::info!(%error, "library management enrichment deferred");
            None
        }
        Err(_) => {
            tracing::info!("library management enrichment timed out");
            None
        }
    }
}

/// What planning gathered for one album before its files are planned.
#[derive(Debug, Clone, Default)]
pub struct AlbumExtras {
    pub genres: super::genre::RemoteGenres,
    /// Per library track.
    pub files: std::collections::HashMap<String, FileExtras>,
    /// Why ReplayGain was not measured for the album, when it was wanted.
    pub replaygain_note: Option<String>,
    /// The album's front cover, when the profile writes art and one was
    /// found.
    pub cover: Option<Cover>,
    /// The cover could not be fetched now.
    pub cover_deferred: bool,
    /// The library track whose plan carries the album's cover file.
    pub cover_owner: Option<String>,
    /// Whether the recording and work credits are on the release.
    pub credits: Credits,
    /// Whether the credited artists' own names are on the release.
    pub names: Credits,
    /// An automatic run: a file whose credits are missing is blocked
    /// rather than planned with a warning.
    pub automatic: bool,
    /// Why AcoustID values were left out, by the library track whose
    /// plan says so (one of each per album).
    pub acoustid_notes: Vec<(String, String)>,
}

/// What planning gathered for one file.
#[derive(Debug, Clone, Default)]
pub struct FileExtras {
    pub replaygain: Option<ReplayGainValues>,
    /// The AcoustID print of the file's audio, when the profile writes it.
    pub acoustid_print: Option<String>,
    /// The AcoustID id of the file's audio, when the profile writes it and
    /// AcoustID links it to the file's recording.
    pub acoustid_id: Option<String>,
}

/// Measured ReplayGain tag values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayGainValues {
    pub track_gain: String,
    pub track_peak: String,
    /// Present when the album was measured as a whole.
    pub album_gain: Option<String>,
    pub album_peak: Option<String>,
}

/// Ask the outside sources the profile needs for one album. Credits and
/// artist names the profile writes are fetched onto `release` (and its stored document)
/// first; `automatic` runs ask at background priority.
pub fn gather(
    port: Option<&dyn EnrichmentPort>,
    store: &SqliteIdentifyStore,
    profile: &droppedneedle_kernel::runtime_config::sections::LibraryManagementProfile,
    release: &mut Release,
    automatic: bool,
) -> AlbumExtras {
    use droppedneedle_kernel::runtime_config::sections::GenreSource;
    let parts = ensure_release_extras(port, store, profile, release, automatic);
    let mut extras = AlbumExtras {
        credits: parts.credits,
        names: parts.names,
        automatic,
        ..AlbumExtras::default()
    };
    let release = &*release;
    let Some(port) = port else {
        return extras;
    };
    let artwork = &profile.artwork;
    let archive = artwork.providers.iter().any(|provider| {
        matches!(
            provider,
            droppedneedle_kernel::runtime_config::sections::ArtworkProvider::CoverArtArchiveRelease
                | droppedneedle_kernel::runtime_config::sections::ArtworkProvider::CoverArtArchiveReleaseGroup
        )
    });
    if (artwork.embedded_enabled || artwork.external_enabled) && archive {
        let release_id = if super::custom::is_custom(release) {
            ""
        } else {
            release.id.as_str()
        };
        match wait(port.front_cover(release_id, &release.release_group_id, automatic)) {
            Some(cover) => extras.cover = cover,
            None => extras.cover_deferred = true,
        }
    }
    if profile.genres.enabled {
        if profile.genres.sources.contains(&GenreSource::Listenbrainz) {
            extras.genres.listenbrainz =
                wait(port.listenbrainz_genres(&release.release_group_id, automatic));
        }
        if profile.genres.sources.contains(&GenreSource::Lastfm) {
            let artist = release.artist_text();
            extras.genres.lastfm = wait(port.lastfm_genres(&artist, &release.title, automatic));
        }
    }
    extras
}

/// What LRCLIB said about one file's lyrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LyricsAnswer {
    /// The profile does not manage lyrics.
    Off,
    /// LRCLIB could not be asked now.
    Deferred,
    /// No exact match.
    Missing,
    Found(LyricsFound),
}

/// Ask LRCLIB for one track's lyrics by its exact signature (v2's
/// accuracy gate: title, artist, album and a length within two seconds),
/// at background priority for an automatic run.
pub fn lyrics(
    port: Option<&dyn EnrichmentPort>,
    settings: &droppedneedle_kernel::runtime_config::sections::LyricsManagementSettings,
    release: &crate::matching::Release,
    track: &crate::matching::ReleaseTrack,
    automatic: bool,
) -> LyricsAnswer {
    if !settings.enabled || (!settings.write_plain && !settings.write_synced) {
        return LyricsAnswer::Off;
    }
    let Some(port) = port else {
        return LyricsAnswer::Deferred;
    };
    let artist = if track.artists.is_empty() {
        release.artist_text()
    } else {
        crate::matching::model::credit_text(&track.artists)
    };
    let Some(length) = track.length_ms else {
        return LyricsAnswer::Missing;
    };
    let query = LyricsQuery {
        title: track.title.clone(),
        artist,
        album: release.title.clone(),
        duration_seconds: length as f64 / 1000.0,
    };
    match wait(port.lyrics(&query, automatic)) {
        Some(Some(found)) => LyricsAnswer::Found(found),
        Some(None) => LyricsAnswer::Missing,
        None => LyricsAnswer::Deferred,
    }
}

/// A cover ready to write: bytes, MIME type and size in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCover {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

/// Fit a cover inside `max` pixels (0 keeps its size) and encode it as
/// `format` asks (`Original` keeps its encoding when no resize is
/// needed). `None` for bytes that are not a JPEG, PNG or WebP image, or
/// over 40 megapixels (a decompression-bomb guard).
pub fn render_cover(
    bytes: &[u8],
    max: i64,
    format: droppedneedle_kernel::runtime_config::sections::ArtworkOutputFormat,
) -> Option<RenderedCover> {
    use droppedneedle_kernel::runtime_config::sections::ArtworkOutputFormat;
    use image::{ImageFormat, ImageReader, imageops::FilterType};
    use std::io::Cursor;

    let source = image::guess_format(bytes).ok()?;
    if !matches!(
        source,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
    ) {
        return None;
    }
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), source)
        .into_dimensions()
        .ok()?;
    if u64::from(width) * u64::from(height) > 40_000_000 {
        return None;
    }
    let target = match format {
        ArtworkOutputFormat::Original => source,
        ArtworkOutputFormat::Jpeg => ImageFormat::Jpeg,
        ArtworkOutputFormat::Png => ImageFormat::Png,
        ArtworkOutputFormat::Webp => ImageFormat::WebP,
    };
    let limit = u32::try_from(max).unwrap_or(0);
    let shrink = limit > 0 && width.max(height) > limit;
    let mime = |format: ImageFormat| match format {
        ImageFormat::Png => "image/png",
        ImageFormat::WebP => "image/webp",
        _ => "image/jpeg",
    };
    if !shrink && target == source {
        return Some(RenderedCover {
            bytes: bytes.to_vec(),
            mime: mime(source).to_owned(),
            width,
            height,
        });
    }
    let mut picture = ImageReader::with_format(Cursor::new(bytes), source)
        .decode()
        .ok()?;
    if shrink {
        picture = picture.resize(limit, limit, FilterType::Lanczos3);
    }
    let mut out = Vec::new();
    match target {
        ImageFormat::Jpeg => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
            picture.to_rgb8().write_with_encoder(encoder).ok()?;
        }
        other => picture
            .to_rgba8()
            .write_to(&mut Cursor::new(&mut out), other)
            .ok()?,
    }
    Some(RenderedCover {
        width: picture.width(),
        height: picture.height(),
        bytes: out,
        mime: mime(target).to_owned(),
    })
}

/// Normalize LRC text: Unix line ends, no blank edges, one final newline.
pub fn normalize_lrc(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let body = lines.join("\n");
    format!("{}\n", body.trim())
}

/// Scripted answers for the test suite.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct ScriptedEnrichment {
    pub listenbrainz: Option<Vec<Candidate>>,
    pub lyrics: Option<LyricsFound>,
    pub cover: Option<Cover>,
    /// The credits MusicBrainz answers with; `None` while it is down.
    pub relations: std::sync::Mutex<Option<crate::matching::model::ReleaseRelations>>,
    /// The artist names MusicBrainz answers with; `None` while it is down.
    pub names: std::sync::Mutex<Option<crate::matching::model::ArtistNames>>,
    /// Release lookups asked for: the parts each wanted, and whether it
    /// was background.
    pub lookups: std::sync::Mutex<Vec<(ExtrasWanted, bool)>>,
    /// What AcoustID answers for every print; `None` while no key is set.
    pub acoustid: std::sync::Mutex<Option<ScriptedAcoustId>>,
    /// The prints AcoustID was asked about, one list per request.
    pub acoustid_calls: std::sync::Mutex<Vec<Vec<AcoustIdPrint>>>,
}

/// A scripted AcoustID.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub enum ScriptedAcoustId {
    /// These results for every print.
    Answers(Vec<droppedneedle_providers::acoustid::BatchResult>),
    Unavailable,
}

#[cfg(any(test, feature = "test-support"))]
impl EnrichmentPort for ScriptedEnrichment {
    fn listenbrainz_genres<'a>(
        &'a self,
        _release_group_mbid: &'a str,
        _background: bool,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        let answer = self
            .listenbrainz
            .clone()
            .ok_or_else(|| "not scripted".to_owned());
        Box::pin(async move { answer })
    }

    fn lastfm_genres<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _background: bool,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        Box::pin(async { Err("not scripted".to_owned()) })
    }

    fn lyrics<'a>(
        &'a self,
        _query: &'a LyricsQuery,
        _background: bool,
    ) -> BoxFuture<'a, Result<Option<LyricsFound>, String>> {
        let answer = Ok(self.lyrics.clone());
        Box::pin(async move { answer })
    }

    fn front_cover<'a>(
        &'a self,
        _release_mbid: &'a str,
        _release_group_mbid: &'a str,
        _background: bool,
    ) -> BoxFuture<'a, Result<Option<Cover>, String>> {
        let answer = Ok(self.cover.clone());
        Box::pin(async move { answer })
    }

    fn release_extras<'a>(
        &'a self,
        _release_mbid: &'a str,
        wants: ExtrasWanted,
        background: bool,
    ) -> BoxFuture<'a, Result<ReleaseExtras, String>> {
        if let Ok(mut lookups) = self.lookups.lock() {
            lookups.push((wants, background));
        }
        let relations = self.relations.lock().ok().and_then(|found| found.clone());
        let names = self.names.lock().ok().and_then(|found| found.clone());
        // One lookup: it answers every part asked for, or none.
        let answer = if (wants.relations && relations.is_none()) || (wants.names && names.is_none())
        {
            Err("MusicBrainz is down".to_owned())
        } else {
            Ok(ReleaseExtras {
                relations: relations.filter(|_| wants.relations),
                names: names.filter(|_| wants.names),
            })
        };
        Box::pin(async move { answer })
    }

    fn acoustid_lookup<'a>(&'a self, prints: &'a [AcoustIdPrint]) -> BoxFuture<'a, AcoustIdAnswer> {
        let script = self.acoustid.lock().ok().and_then(|script| script.clone());
        if !prints.is_empty()
            && let Ok(mut calls) = self.acoustid_calls.lock()
        {
            calls.push(prints.to_vec());
        }
        let answer = match script {
            None => AcoustIdAnswer::NoKey,
            Some(ScriptedAcoustId::Unavailable) if !prints.is_empty() => {
                AcoustIdAnswer::Unavailable
            }
            Some(ScriptedAcoustId::Unavailable) => AcoustIdAnswer::Answered(Vec::new()),
            Some(ScriptedAcoustId::Answers(results)) => {
                AcoustIdAnswer::Answered(vec![results; prints.len()])
            }
        };
        Box::pin(async move { answer })
    }
}
