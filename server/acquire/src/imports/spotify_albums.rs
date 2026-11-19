//! Which MusicBrainz release group a Spotify album is.
//!
//! An album is matched only on proof, never on its name alone: a release
//! counts when it carries a recording with an ISRC Spotify gave for a
//! track of that album, under one of the album's title forms. The
//! releases come first from a recording search by those ISRCs, which
//! finds every edition whatever a title search would rank first, then,
//! when MusicBrainz knows none of them, from a release search by title
//! and the album's own artist, read with `inc=recordings+isrcs`. Two
//! proven groups (an album and a same-named single carrying the same
//! recording) are told apart by Spotify's album type, then its track
//! count. No ISRC, no proven release, or a tie left after that leave the
//! album unresolved, so an import never points a request at the wrong
//! music. Lookups run at background priority, paced by the executor, and
//! each album's answer is kept in the metadata cache.

use std::collections::{BTreeMap, BTreeSet};

use droppedneedle_kernel::db::fold_text;
use droppedneedle_kernel::runtime_config::{Live, sections::MusicBrainzSettings};
use droppedneedle_providers::cache::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
use droppedneedle_providers::musicbrainz::models::ReleaseGroupRef;
use droppedneedle_providers::musicbrainz::{
    Criticality, MusicBrainzClient, MusicBrainzView, SOURCE, is_valid_mbid, normalize_mb_id,
};
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};

/// Releases read from one search. Editions of a proven group cost no
/// lookup, so this bounds the lookups to the distinct candidates.
const SEARCH_LIMIT: u32 = 5;

/// Recordings read from one ISRC search, where every hit carries its
/// releases inline with no extra lookups.
const RECORDING_SEARCH_LIMIT: u32 = 25;

/// An album's ISRCs asked by recording search, in order, until one
/// proves a group: one is enough when MusicBrainz knows the album.
const ISRC_SEARCHES: usize = 2;

/// The includes a candidate is proven with.
const PROOF_INCLUDES: [&str; 3] = ["isrcs", "recordings", "release-groups"];

/// One Spotify album as the import knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyAlbum {
    /// Album title.
    pub title: String,
    /// The album's first artist, else the first credited artist of a
    /// track on it.
    pub artist: String,
    /// Spotify's album type: `album`, `single` or `compilation`.
    pub kind: Option<String>,
    /// Tracks on the album, as Spotify counts them.
    pub total_tracks: Option<u32>,
    /// The ISRCs of its tracks on the playlist, uppercased and well formed.
    pub isrcs: BTreeSet<String>,
}

impl SpotifyAlbum {
    /// The album with `isrc` added when it is a well-formed ISRC.
    pub fn add_isrc(&mut self, isrc: &str) {
        let isrc = isrc.trim().to_ascii_uppercase();
        if is_isrc(&isrc) {
            self.isrcs.insert(isrc);
        }
    }
}

/// Finds the release group of Spotify albums through MusicBrainz.
#[derive(Clone)]
pub struct AlbumResolver {
    musicbrainz: MusicBrainzClient,
    settings: Live<MusicBrainzSettings>,
    cache: ProviderCacheStore,
}

impl AlbumResolver {
    /// The resolver over the shared providers, reading the MusicBrainz
    /// source from `settings` at each album.
    pub fn new(providers: &Providers, settings: Live<MusicBrainzSettings>) -> Self {
        Self {
            musicbrainz: providers.musicbrainz.clone(),
            settings,
            cache: providers.cache.clone(),
        }
    }

    /// The album's release group id, `None` when nothing proves one.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, so the caller can stop asking while the
    /// service is down. Nothing is cached for it.
    pub async fn resolve(&self, album: &SpotifyAlbum) -> Result<Option<String>, ProviderError> {
        if album.isrcs.is_empty() || album.title.trim().is_empty() {
            return Ok(None);
        }
        let Some(settings) = self.settings.read() else {
            return Err(ProviderError::NotConfigured {
                provider: SOURCE,
                message: "the musicbrainz settings cannot be read".to_owned(),
            });
        };
        let namespace = format!("{}:g{}", settings.source_id, settings.generation);
        let view = self.musicbrainz.fixed(
            self.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        );
        let isrcs: Vec<&str> = album.isrcs.iter().map(String::as_str).collect();
        let joined = isrcs.join(",");
        let tracks = album
            .total_tracks
            .map(|n| n.to_string())
            .unwrap_or_default();
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "spotify_album",
            &[
                &namespace,
                &album.title,
                &album.artist,
                album.kind.as_deref().unwrap_or_default(),
                &tracks,
                &joined,
            ],
        );
        let album = album.clone();
        self.cache
            .get_or_fetch(
                &key,
                Policy::RELEASE_TO_GROUP,
                RequestPriority::BackgroundSync,
                move |_| async move {
                    Ok(match prove(&view, &album).await? {
                        Some(group) => Fetched::Found(group),
                        None => Fetched::Missing,
                    })
                },
            )
            .await
    }
}

/// A release group a release proved, with what a tie is broken by.
struct Proven {
    primary_type: Option<String>,
    secondary_types: Vec<String>,
    /// The track counts of its proving releases.
    track_counts: BTreeSet<u32>,
}

impl Proven {
    fn new(group: &ReleaseGroupRef) -> Self {
        Self {
            primary_type: group.primary_type.clone(),
            secondary_types: group.secondary_types.clone(),
            track_counts: BTreeSet::new(),
        }
    }

    /// Whether the group is the kind of release Spotify says the album is;
    /// any group fits an album Spotify gave no type.
    fn fits(&self, kind: Option<&str>) -> bool {
        let has_secondary = |want: &str| {
            self.secondary_types
                .iter()
                .any(|secondary| secondary.eq_ignore_ascii_case(want))
        };
        let compilation = has_secondary("Compilation");
        let soundtrack = has_secondary("Soundtrack");
        match kind.map(str::to_ascii_lowercase).as_deref() {
            Some("compilation") => compilation || soundtrack,
            Some("single") => matches!(self.primary_type.as_deref(), Some("Single" | "EP")),
            Some("album") => self.primary_type.as_deref() == Some("Album") && !compilation,
            _ => true,
        }
    }
}

/// The proven group, by the album's ISRCs first and its title and
/// artist second.
async fn prove(
    view: &MusicBrainzView,
    album: &SpotifyAlbum,
) -> Result<Option<String>, ProviderError> {
    let mut proven = by_isrc(view, album).await?;
    if proven.is_empty() {
        proven = by_search(view, album).await?;
    }
    Ok(pick(album, proven))
}

/// The groups of the releases a recording search by the album's ISRCs
/// finds under one of its title forms.
async fn by_isrc(
    view: &MusicBrainzView,
    album: &SpotifyAlbum,
) -> Result<BTreeMap<String, Proven>, ProviderError> {
    let forms = non_empty_forms(&album.title);
    if forms.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut proven: BTreeMap<String, Proven> = BTreeMap::new();
    for isrc in album
        .isrcs
        .iter()
        .filter(|isrc| is_isrc(isrc))
        .take(ISRC_SEARCHES)
    {
        let page = view
            .search_recordings_query(
                &format!("isrc:{isrc}"),
                RECORDING_SEARCH_LIMIT,
                Criticality::IdentityCritical,
            )
            .await?;
        for release in page.items.iter().flat_map(|hit| &hit.releases) {
            let (Some(title), Some(group)) = (&release.title, &release.release_group) else {
                continue;
            };
            let id = normalize_mb_id(&group.id);
            if is_valid_mbid(&id) && !non_empty_forms(title).is_disjoint(&forms) {
                proven
                    .entry(id)
                    .or_insert_with(|| Proven::new(group))
                    .track_counts
                    .extend(release.track_count);
            }
        }
        if !proven.is_empty() {
            break;
        }
    }
    Ok(proven)
}

/// The groups proven by a release search for each title form, until one
/// form proves any.
async fn by_search(
    view: &MusicBrainzView,
    album: &SpotifyAlbum,
) -> Result<BTreeMap<String, Proven>, ProviderError> {
    for title in title_forms(&album.title) {
        let page = view
            .search_releases(
                &title,
                &album.artist,
                SEARCH_LIMIT,
                Criticality::IdentityCritical,
            )
            .await?;
        let mut proven: BTreeMap<String, Proven> = BTreeMap::new();
        for hit in page.items {
            let listed = hit
                .release_group
                .as_ref()
                .map(|group| normalize_mb_id(&group.id));
            if listed
                .as_ref()
                .is_some_and(|group| proven.contains_key(group))
            {
                continue;
            }
            let Some(release) = view
                .lookup_release(&hit.id, &PROOF_INCLUDES, Criticality::IdentityCritical)
                .await?
            else {
                continue;
            };
            let release = release.entity;
            let Some(group) = release.release_group.as_ref() else {
                continue;
            };
            let id = normalize_mb_id(&group.id);
            if !is_valid_mbid(&id) {
                continue;
            }
            let carries = release
                .media
                .iter()
                .flat_map(|medium| &medium.tracks)
                .filter_map(|track| track.recording.as_ref())
                .flat_map(|recording| &recording.isrcs)
                .any(|isrc| album.isrcs.contains(&isrc.trim().to_ascii_uppercase()));
            if carries {
                let tracks = release
                    .media
                    .iter()
                    .map(|medium| medium.tracks.len())
                    .sum::<usize>();
                proven
                    .entry(id)
                    .or_insert_with(|| Proven::new(group))
                    .track_counts
                    .extend(u32::try_from(tracks).ok());
            }
        }
        if !proven.is_empty() {
            return Ok(proven);
        }
    }
    Ok(BTreeMap::new())
}

/// The one proven group, after a tie is narrowed by the album's type and
/// then its track count; a rule that would rule out every group is skipped.
fn pick(album: &SpotifyAlbum, proven: BTreeMap<String, Proven>) -> Option<String> {
    let mut left: Vec<(&String, &Proven)> = proven.iter().collect();
    let rules: [&dyn Fn(&Proven) -> bool; 2] =
        [&|group| group.fits(album.kind.as_deref()), &|group| {
            album
                .total_tracks
                .is_none_or(|tracks| group.track_counts.contains(&tracks))
        }];
    for rule in rules {
        if left.len() <= 1 {
            break;
        }
        let kept: Vec<_> = left
            .iter()
            .copied()
            .filter(|(_, group)| rule(group))
            .collect();
        if !kept.is_empty() {
            left = kept;
        }
    }
    match left.as_slice() {
        [(group, _)] => Some((*group).clone()),
        [] => None,
        _ => {
            tracing::debug!(
                groups = left.len(),
                "spotify album matches more than one release group; left unresolved"
            );
            None
        }
    }
}

/// A title as the proof compares it: folded, with punctuation as spaces,
/// so "Awesome Mix, Vol. 1" reads as "Awesome Mix Vol. 1". Titles made only
/// of symbols (`+`, `÷`, `( )`) keep their folded symbols instead of
/// collapsing to the empty string.
fn title_key(title: &str) -> String {
    let folded = fold_text(title);
    let words = folded
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if words.is_empty() {
        folded.trim().to_owned()
    } else {
        words
    }
}

/// Non-empty title keys for `title` and its stripped edition form, so an
/// empty or whitespace-only string never matches anything.
fn non_empty_forms(title: &str) -> BTreeSet<String> {
    title_forms(title)
        .iter()
        .map(|form| title_key(form))
        .filter(|key| !key.is_empty())
        .collect()
}

/// The title as Spotify gives it, then without a trailing edition note
/// (`Abbey Road (Remastered)`, `Glass Tides - Single`), which MusicBrainz
/// keeps out of the title.
fn title_forms(title: &str) -> Vec<String> {
    let title = title.trim();
    let mut forms = vec![title.to_owned()];
    let base = if let Some(stripped) = title
        .strip_suffix(')')
        .and_then(|rest| rest.rfind('(').map(|at| &rest[..at]))
    {
        stripped
    } else if let Some(stripped) = title
        .strip_suffix(']')
        .and_then(|rest| rest.rfind('[').map(|at| &rest[..at]))
    {
        stripped
    } else if let Some(at) = title.rfind(" - ") {
        &title[..at]
    } else {
        title
    };
    let base = base.trim();
    if !base.is_empty() && base != title {
        forms.push(base.to_owned());
    }
    forms
}

/// `CC XXX YY NNNNN`: two letters, three letters or digits, then seven
/// digits.
fn is_isrc(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 12
        && bytes[..2].iter().all(u8::is_ascii_uppercase)
        && bytes[2..5]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && bytes[5..].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use droppedneedle_kernel::runtime_config::sections::MbSourceMode;
    use droppedneedle_providers::test_support::ScriptedWire;

    use super::*;

    const MB_ROOT: &str = "http://127.0.0.1:9/musicbrainz/ws/2";
    const GROUP_A: &str = "5907a1b0-0000-4000-8000-000000000101";

    fn scripted_view(wire: Arc<ScriptedWire>) -> MusicBrainzView {
        let providers = Providers::unpaced(wire);
        let settings = MusicBrainzSettings {
            source_mode: MbSourceMode::Official,
            selected_source_mode: MbSourceMode::Official,
            ..MusicBrainzSettings::default()
        };
        providers.musicbrainz.fixed(
            providers.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        )
    }

    fn group_ref(primary_type: Option<&str>, secondary_types: &[&str]) -> ReleaseGroupRef {
        ReleaseGroupRef {
            id: GROUP_A.to_owned(),
            title: Some("Sample".to_owned()),
            first_release_date: None,
            primary_type: primary_type.map(str::to_owned),
            primary_type_id: None,
            secondary_types: secondary_types.iter().map(|s| (*s).to_owned()).collect(),
            artist_credit: Vec::new(),
            disambiguation: None,
            genres: Vec::new(),
        }
    }

    #[tokio::test]
    async fn title_key_keeps_symbols_distinct_and_blanks_never_match() {
        assert_eq!(title_key("+"), "+");
        assert_eq!(title_key("÷"), "÷");
        assert_ne!(title_key("+"), title_key("÷"));
        assert_eq!(title_key("( )"), "( )");
        assert_eq!(title_key("   "), "");

        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/recording"),
            200,
            &[("content-type", "application/json")],
            format!(
                r#"{{"recordings":[{{"id":"5907a1b0-0000-4000-8000-000000000201","title":"Song","releases":[
                    {{"id":"5907a1b0-0000-4000-8000-000000000301","title":"÷","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"÷","primary-type":"Album","secondary-types":[]}}}},
                    {{"id":"5907a1b0-0000-4000-8000-000000000302","title":"","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"","primary-type":"Album","secondary-types":[]}}}}
                ]}}]}}"#
            ),
        );
        let view = scripted_view(wire);

        let mut plus = SpotifyAlbum {
            title: "+".to_owned(),
            artist: "Ed Sheeran".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(12),
            isrcs: BTreeSet::new(),
        };
        plus.add_isrc("QZAAA2600001");
        assert!(
            by_isrc(&view, &plus).await.expect("mb answers").is_empty(),
            "+ must not match ÷"
        );

        let mut blank = plus.clone();
        blank.title = "   ".to_owned();
        assert!(
            by_isrc(&view, &blank).await.expect("mb answers").is_empty(),
            "blank title must never match"
        );
    }

    #[tokio::test]
    async fn by_isrc_strips_musicbrainz_edition_suffix_and_validates_isrcs() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/recording"),
            200,
            &[("content-type", "application/json")],
            format!(
                r#"{{"recordings":[{{"id":"5907a1b0-0000-4000-8000-000000000201","title":"Signal","releases":[
                    {{"id":"5907a1b0-0000-4000-8000-000000000301","title":"Night Signals (Original Soundtrack)","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"Night Signals","primary-type":"Album","secondary-types":["Soundtrack"]}}}}
                ]}}]}}"#
            ),
        );
        let view = scripted_view(wire.clone());

        let mut album = SpotifyAlbum {
            title: "Night Signals".to_owned(),
            artist: "Various Artists".to_owned(),
            kind: Some("compilation".to_owned()),
            total_tracks: Some(12),
            isrcs: BTreeSet::new(),
        };
        // Invalid value inserted directly into the pub field must be skipped.
        album.isrcs.insert("BAD OR artist:x".to_owned());
        album.add_isrc("QZAAA2600001");

        let proven = by_isrc(&view, &album).await.expect("mb answers");
        assert!(
            proven.contains_key(GROUP_A),
            "MB release with trailing edition note should match on ISRC lookup"
        );

        let sent = wire.sent();
        assert_eq!(sent.len(), 1, "invalid ISRC skipped, valid ISRC hits once");
        assert_eq!(sent[0].query("query").as_deref(), Some("isrc:QZAAA2600001"));
        assert_eq!(sent[0].query("limit").as_deref(), Some("25"));
    }

    #[test]
    fn proven_fits_handles_case_and_soundtracks() {
        let soundtrack = Proven::new(&group_ref(Some("Album"), &["Soundtrack"]));
        assert!(soundtrack.fits(Some("compilation")));
        assert!(soundtrack.fits(Some("COMPILATION")));
        assert!(soundtrack.fits(Some("album")));
        assert!(soundtrack.fits(Some("ALBUM")));
        assert!(!soundtrack.fits(Some("single")));
        assert!(!soundtrack.fits(Some("SINGLE")));

        let compilation = Proven::new(&group_ref(Some("Album"), &["Compilation"]));
        assert!(compilation.fits(Some("Compilation")));
        assert!(!compilation.fits(Some("album")));
        assert!(!compilation.fits(Some("ALBUM")));

        let single = Proven::new(&group_ref(Some("Single"), &[]));
        assert!(single.fits(Some("SINGLE")));
        assert!(!single.fits(Some("ALBUM")));
    }
}
