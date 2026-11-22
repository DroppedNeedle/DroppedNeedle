//! Which MusicBrainz release group a Spotify album is.
//!
//! An album is matched only on proof, never on its name alone: a release
//! counts when it carries a recording with an ISRC Spotify gave for a
//! track of that album, under one of the album's title forms. The
//! releases come first from a recording search by those ISRCs, which
//! finds every edition whatever a title search would rank first, then,
//! when MusicBrainz knows none of them, from a release search by title
//! and the album's own artist, read with `inc=recordings+isrcs`. A
//! multi-track release (two or more tracks) MusicBrainz lists no ISRCs
//! for at all can't be proven that way, so it counts by its listing
//! instead: the album's title, an artist of it, Spotify's track count
//! and known album type, and every song of the album on the playlist on
//! it. One-track singles still require ISRC proof, a release listing any
//! ISRC must prove itself by one, and an ISRC proof wins over a listing.
//! Two proven groups (an album and a same-named single carrying the same
//! recording) are told apart by Spotify's album type, then its track
//! count. No proven release, or a tie left after that, leaves the album
//! unresolved, so an import never points a request at the wrong music.
//! Lookups run at background priority, paced by the executor, and each
//! album's answer is kept in the metadata cache.

use std::collections::{BTreeMap, BTreeSet};

use droppedneedle_kernel::db::fold_text;
use droppedneedle_kernel::runtime_config::{Live, sections::MusicBrainzSettings};
use droppedneedle_providers::cache::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
use droppedneedle_providers::musicbrainz::models::{
    MbRelease, ReleaseGroupRef, ReleaseSearchHit, Track,
};
use droppedneedle_providers::musicbrainz::{
    Criticality, MusicBrainzClient, MusicBrainzView, SOURCE, is_valid_mbid, normalize_mb_id,
    escape_lucene_phrase,
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
    /// The titles of its tracks on the playlist.
    pub songs: BTreeSet<String>,
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
        let can_list = !album.songs.is_empty() && album.total_tracks.is_some_and(|n| n >= 2);
        if album.title.trim().is_empty() || (album.isrcs.is_empty() && !can_list) {
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
        let songs: Vec<&str> = album.songs.iter().map(String::as_str).collect();
        // ASCII unit separator survives CacheKey::hashed's whitespace collapse,
        // keeping {"A B", "C"} distinct from {"A", "B C"}.
        let songs = songs.join("\x1f");
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
                &songs,
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

    /// A recording for a playlist row named by a title and an artist, or
    /// `None` when the names do not prove one. The search needs an exact
    /// title and an artist credit; music videos are skipped.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, so the caller can stop asking while the
    /// service is down.
    pub async fn resolve_track(
        &self,
        title: &str,
        artist: &str,
    ) -> Result<Option<String>, ProviderError> {
        let wanted = title_key(title);
        let who = title_key(artist);
        if wanted.is_empty() || who.is_empty() {
            return Ok(None);
        }
        let Some(settings) = self.settings.read() else {
            return Err(ProviderError::NotConfigured {
                provider: SOURCE,
                message: "the musicbrainz settings cannot be read".to_owned(),
            });
        };
        let view = self.musicbrainz.fixed(
            self.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        );
        let query = format!(
            "recording:\"{}\" AND artist:\"{}\"",
            escape_lucene_phrase(title),
            escape_lucene_phrase(artist)
        );
        let page = view
            .search_recordings_query(&query, SEARCH_LIMIT, Criticality::IdentityCritical)
            .await?;
        let mut recordings: BTreeSet<String> = BTreeSet::new();
        for hit in &page.items {
            if hit.video == Some(true) {
                continue;
            }
            let Some(name) = hit.title.as_deref() else {
                continue;
            };
            if title_key(name) != wanted {
                continue;
            }
            let credit = hit
                .artist_credit
                .iter()
                .map(|credit| credit.name.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !title_key(&credit).contains(&who) {
                continue;
            }
            let id = normalize_mb_id(&hit.id);
            if is_valid_mbid(&id) {
                recordings.insert(id);
            }
        }
        Ok((recordings.len() == 1).then(|| recordings.into_iter().next()).flatten())
    }

    /// A release group for an album named by a playlist row, or `None`
    /// when the names do not prove one. The search needs an exact title and
    /// an artist credit; it does not need an edition's track count, because
    /// a playlist row names songs rather than an edition.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, so the caller can stop asking while the
    /// service is down.
    pub async fn resolve_names(
        &self,
        album_title: &str,
        artist: &str,
    ) -> Result<Option<String>, ProviderError> {
        let wanted = title_key(album_title);
        let who = title_key(artist);
        if wanted.is_empty() || who.is_empty() {
            return Ok(None);
        }
        let Some(settings) = self.settings.read() else {
            return Err(ProviderError::NotConfigured {
                provider: SOURCE,
                message: "the musicbrainz settings cannot be read".to_owned(),
            });
        };
        let view = self.musicbrainz.fixed(
            self.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        );
        let page = view
            .search_releases(
                album_title,
                artist,
                SEARCH_LIMIT,
                Criticality::IdentityCritical,
            )
            .await?;
        let mut groups: BTreeSet<String> = BTreeSet::new();
        for hit in &page.items {
            let Some(group) = &hit.release_group else {
                continue;
            };
            let title = hit.title.as_deref().unwrap_or_default();
            if title_key(title) != wanted {
                continue;
            }
            let credit = hit
                .artist_credit
                .iter()
                .map(|credit| credit.name.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !title_key(&credit).contains(&who) {
                continue;
            }
            let id = normalize_mb_id(&group.id);
            if is_valid_mbid(&id) {
                groups.insert(id);
            }
        }
        Ok((groups.len() == 1)
            .then(|| groups.into_iter().next())
            .flatten())
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

    /// Whether the group is the known kind of release Spotify says the album
    /// is. Returns `None` when `kind` is absent or not one of Spotify's
    /// `album`, `single` or `compilation` types.
    fn fits_known(&self, kind: Option<&str>) -> Option<bool> {
        let has_secondary = |want: &str| {
            self.secondary_types
                .iter()
                .any(|secondary| secondary.eq_ignore_ascii_case(want))
        };
        let compilation = has_secondary("Compilation");
        let soundtrack = has_secondary("Soundtrack");
        match kind.map(str::to_ascii_lowercase).as_deref() {
            Some("compilation") => Some(compilation || soundtrack),
            Some("single") => Some(matches!(
                self.primary_type.as_deref(),
                Some("Single" | "EP")
            )),
            Some("album") => Some(self.primary_type.as_deref() == Some("Album") && !compilation),
            _ => None,
        }
    }

    /// Whether the group is the kind of release Spotify says the album is;
    /// any group fits an album Spotify gave no type.
    fn fits(&self, kind: Option<&str>) -> bool {
        self.fits_known(kind).unwrap_or(true)
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
/// form proves any by ISRC; if none does, the first form's listing
/// matches are returned.
async fn by_search(
    view: &MusicBrainzView,
    album: &SpotifyAlbum,
) -> Result<BTreeMap<String, Proven>, ProviderError> {
    let mut fallback: Option<BTreeMap<String, Proven>> = None;
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
        let mut by_listing: BTreeMap<String, Proven> = BTreeMap::new();
        for hit in &page.items {
            let listed = hit
                .release_group
                .as_ref()
                .map(|group| normalize_mb_id(&group.id));
            if listed.as_ref().is_some_and(|group| {
                proven.contains_key(group)
                    || (album.isrcs.is_empty() && by_listing.contains_key(group))
            }) {
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
            let tracks: Vec<&Track> = release
                .media
                .iter()
                .flat_map(|medium| &medium.tracks)
                .collect();
            let candidate = Proven::new(group);
            let found = if carries {
                &mut proven
            } else if lists_the_album(album, hit, &release, &tracks, &candidate) {
                &mut by_listing
            } else {
                continue;
            };
            found
                .entry(id)
                .or_insert(candidate)
                .track_counts
                .extend(u32::try_from(tracks.len()).ok());
        }
        if !proven.is_empty() {
            return Ok(proven);
        }
        if !by_listing.is_empty() {
            if album.isrcs.is_empty() {
                return Ok(by_listing);
            }
            if fallback.is_none() {
                fallback = Some(by_listing);
            }
        }
    }
    Ok(fallback.unwrap_or_default())
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

/// Whether a multi-track release MusicBrainz lists no ISRCs for is the
/// album by its listing: its title is one of the album's title forms, the
/// album's artist is among its credited artists, it has at least two
/// tracks and as many as Spotify counts, its group is the known kind
/// Spotify gives, and every song of the album on the playlist is on it.
fn lists_the_album(
    album: &SpotifyAlbum,
    hit: &ReleaseSearchHit,
    release: &MbRelease,
    tracks: &[&Track],
    group: &Proven,
) -> bool {
    let artist = title_key(&album.artist);
    let album_forms = non_empty_forms(&album.title);
    if artist.is_empty() || album_forms.is_empty() || album.songs.is_empty() {
        return false;
    }
    let on_release: BTreeSet<String> = tracks
        .iter()
        .filter_map(|track| track.display_title())
        .flat_map(non_empty_forms)
        .collect();
    let release_title = release
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .or(hit.title.as_deref());
    album
        .total_tracks
        .is_some_and(|n| n >= 2 && Some(n) == u32::try_from(tracks.len()).ok())
        && tracks.iter().all(|track| {
            track
                .recording
                .as_ref()
                .is_none_or(|recording| recording.isrcs.is_empty())
        })
        && release_title.is_some_and(|title| !non_empty_forms(title).is_disjoint(&album_forms))
        && hit.artist_credit.iter().any(|credit| {
            title_key(&credit.name) == artist || title_key(&credit.artist.name) == artist
        })
        && group.fits_known(album.kind.as_deref()) == Some(true)
        && album.songs.iter().all(|song| {
            let song_forms = non_empty_forms(song);
            !song_forms.is_empty() && !song_forms.is_disjoint(&on_release)
        })
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

    use droppedneedle_kernel::runtime_config::ConfigStore;
    use droppedneedle_kernel::runtime_config::crypto::Crypto;
    use droppedneedle_kernel::runtime_config::sections::MbSourceMode;
    use droppedneedle_kernel::test_support::ScratchDir;
    use droppedneedle_providers::test_support::ScriptedWire;

    use super::*;

    const MB_ROOT: &str = "http://127.0.0.1:9/musicbrainz/ws/2";
    const GROUP_A: &str = "5907a1b0-0000-4000-8000-000000000101";
    const GROUP_B: &str = "5907a1b0-0000-4000-8000-000000000102";
    const REL_1: &str = "5907a1b0-0000-4000-8000-000000000301";
    const REL_2: &str = "5907a1b0-0000-4000-8000-000000000302";
    const REL_3: &str = "5907a1b0-0000-4000-8000-000000000303";
    const JSON_HEADER: &[(&str, &str)] = &[("content-type", "application/json")];

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

    fn search_hit(rel_id: &str, group_id: &str, title: &str, artist: &str, kind: &str) -> String {
        format!(
            r#"{{"id":"{rel_id}","title":"{title}","artist-credit":[{{"name":"{artist}","artist":{{"id":"5907a1b0-0000-4000-8000-000000000401","name":"{artist}"}}}}],"release-group":{{"id":"{group_id}","title":"{title}","primary-type":"{kind}","secondary-types":[]}}}}"#
        )
    }

    fn release_doc(
        rel_id: &str,
        group_id: &str,
        title: &str,
        kind: &str,
        tracks: &[(&str, Option<&str>, &[&str])],
    ) -> String {
        let track_json: Vec<String> = tracks
            .iter()
            .enumerate()
            .map(|(index, (rec_title, track_title, isrcs))| {
                let isrcs_json: Vec<String> = isrcs.iter().map(|i| format!(r#""{i}""#)).collect();
                let title_field = match track_title {
                    Some(t) => format!(r#""title":"{t}","#),
                    None => String::new(),
                };
                format!(
                    r#"{{"id":"5907a1b0-0000-4000-8000-00000000050{index}",{title_field}"recording":{{"id":"5907a1b0-0000-4000-8000-00000000060{index}","title":"{rec_title}","isrcs":[{}]}}}}"#,
                    isrcs_json.join(",")
                )
            })
            .collect();
        format!(
            r#"{{"id":"{rel_id}","title":"{title}","release-group":{{"id":"{group_id}","title":"{title}","primary-type":"{kind}","secondary-types":[]}},"media":[{{"position":1,"tracks":[{}]}}]}}"#,
            track_json.join(",")
        )
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
            JSON_HEADER,
            format!(
                r#"{{"recordings":[{{"id":"5907a1b0-0000-4000-8000-000000000201","title":"Song","releases":[
                    {{"id":"{REL_1}","title":"÷","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"÷","primary-type":"Album","secondary-types":[]}}}},
                    {{"id":"{REL_2}","title":"","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"","primary-type":"Album","secondary-types":[]}}}}
                ]}}]}}"#
            ),
        );
        let view = scripted_view(wire);

        let mut plus = SpotifyAlbum {
            title: "+".to_owned(),
            artist: "Ed Sheeran".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(12),
            songs: BTreeSet::new(),
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
            JSON_HEADER,
            format!(
                r#"{{"recordings":[{{"id":"5907a1b0-0000-4000-8000-000000000201","title":"Signal","releases":[
                    {{"id":"{REL_1}","title":"Night Signals (Original Soundtrack)","track-count":12,"release-group":{{"id":"{GROUP_A}","title":"Night Signals","primary-type":"Album","secondary-types":["Soundtrack"]}}}}
                ]}}]}}"#
            ),
        );
        let view = scripted_view(wire.clone());

        let mut album = SpotifyAlbum {
            title: "Night Signals".to_owned(),
            artist: "Various Artists".to_owned(),
            kind: Some("compilation".to_owned()),
            total_tracks: Some(12),
            songs: BTreeSet::new(),
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

    #[tokio::test]
    async fn listing_rejects_one_track_single_without_isrcs() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Solo Drop", "Aurora Current", "Single")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Solo Drop",
                "Single",
                &[("Solo Drop", Some("Solo Drop"), &[])],
            ),
        );
        let view = scripted_view(wire);

        let album = SpotifyAlbum {
            title: "Solo Drop".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("single".to_owned()),
            total_tracks: Some(1),
            songs: BTreeSet::from(["Solo Drop".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            prove(&view, &album).await.expect("mb answers"),
            None,
            "1-track single with zero ISRCs must stay unresolved"
        );
    }

    #[tokio::test]
    async fn listing_requires_known_album_kind() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Quiet Harbor", "Aurora Current", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Quiet Harbor",
                "Album",
                &[
                    ("Low Tide", Some("Low Tide"), &[]),
                    ("Undertow", Some("Undertow"), &[]),
                ],
            ),
        );
        let view = scripted_view(wire);

        for kind in [None, Some("unknown".to_owned())] {
            let album = SpotifyAlbum {
                title: "Quiet Harbor".to_owned(),
                artist: "Aurora Current".to_owned(),
                kind: kind.clone(),
                total_tracks: Some(2),
                songs: BTreeSet::from(["Low Tide".to_owned()]),
                isrcs: BTreeSet::new(),
            };
            assert_eq!(
                prove(&view, &album).await.expect("mb answers"),
                None,
                "kind {kind:?} must not match by listing"
            );
        }
    }

    #[tokio::test]
    async fn listing_rejects_blank_and_symbol_mismatches() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{},{}]}}"#,
                search_hit(REL_1, GROUP_A, "Quiet Harbor", "", "Album"),
                search_hit(REL_2, GROUP_B, "÷", "÷", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Quiet Harbor",
                "Album",
                &[("", Some(""), &[]), ("Track Two", Some("Track Two"), &[])],
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_2}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_2,
                GROUP_B,
                "÷",
                "Album",
                &[("÷", Some("÷"), &[]), ("Two", Some("Two"), &[])],
            ),
        );
        let view = scripted_view(wire);

        let blank_artist = SpotifyAlbum {
            title: "Quiet Harbor".to_owned(),
            artist: "   ".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["Track Two".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            prove(&view, &blank_artist).await.expect("mb answers"),
            None,
            "blank artist must not match empty MB artist credit"
        );

        let blank_song = SpotifyAlbum {
            title: "Quiet Harbor".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["   ".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        let wire_song = Arc::new(ScriptedWire::new());
        wire_song.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Quiet Harbor", "Aurora Current", "Album")
            ),
        );
        wire_song.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Quiet Harbor",
                "Album",
                &[("", Some(""), &[]), ("Track Two", Some("Track Two"), &[])],
            ),
        );
        assert_eq!(
            prove(&scripted_view(wire_song), &blank_song)
                .await
                .expect("mb answers"),
            None,
            "blank song must not match empty MB track title"
        );

        let symbol_mismatch = SpotifyAlbum {
            title: "+".to_owned(),
            artist: "+".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["+".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            prove(&view, &symbol_mismatch).await.expect("mb answers"),
            None,
            "punctuation-only + must not match ÷ in listing"
        );
    }

    #[tokio::test]
    async fn by_search_does_not_let_listing_suppress_isrc_proof() {
        // Part A: Hit 1 of Group A matches by listing, Hit 2 of Group A carries
        // the ISRC, Hit 3 of Group B (a Single) also carries the ISRC.
        // Group A must not be skipped on Hit 2.
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{},{},{}]}}"#,
                search_hit(REL_1, GROUP_A, "Glass Bloom", "Aurora Current", "Album"),
                search_hit(REL_2, GROUP_A, "Glass Bloom", "Aurora Current", "Album"),
                search_hit(REL_3, GROUP_B, "Glass Bloom", "Aurora Current", "Single")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Glass Bloom",
                "Album",
                &[
                    ("Track 1", Some("Track 1"), &[]),
                    ("Track 2", Some("Track 2"), &[]),
                ],
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_2}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_2,
                GROUP_A,
                "Glass Bloom",
                "Album",
                &[
                    ("Track 1", Some("Track 1"), &["QZAAA2600099"]),
                    ("Track 2", Some("Track 2"), &[]),
                ],
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_3}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_3,
                GROUP_B,
                "Glass Bloom",
                "Single",
                &[("Track 1", Some("Track 1"), &["QZAAA2600099"])],
            ),
        );
        let view = scripted_view(wire);

        let mut album = SpotifyAlbum {
            title: "Glass Bloom".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["Track 1".to_owned(), "Track 2".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        album.add_isrc("QZAAA2600099");

        let proven = by_search(&view, &album).await.expect("mb answers");
        assert!(
            proven.contains_key(GROUP_A),
            "Group A must not be suppressed by an earlier listing match"
        );
        assert_eq!(pick(&album, proven).as_deref(), Some(GROUP_A));

        // Part B: Form 1 ("Glass Bloom (Deluxe)") matches Group B by listing,
        // while Form 2 ("Glass Bloom") proves Group A by ISRC.
        let wire_forms = Arc::new(ScriptedWire::new());
        wire_forms.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(
                    REL_1,
                    GROUP_B,
                    "Glass Bloom (Deluxe)",
                    "Aurora Current",
                    "Album"
                )
            ),
        );
        wire_forms.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_2, GROUP_A, "Glass Bloom", "Aurora Current", "Album")
            ),
        );
        wire_forms.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_B,
                "Glass Bloom (Deluxe)",
                "Album",
                &[
                    ("Track 1", Some("Track 1"), &[]),
                    ("Track 2", Some("Track 2"), &[]),
                ],
            ),
        );
        wire_forms.answer(
            &format!("{MB_ROOT}/release/{REL_2}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_2,
                GROUP_A,
                "Glass Bloom",
                "Album",
                &[
                    ("Track 1", Some("Track 1"), &["QZAAA2600099"]),
                    ("Track 2", Some("Track 2"), &[]),
                ],
            ),
        );

        let mut deluxe = album.clone();
        deluxe.title = "Glass Bloom (Deluxe)".to_owned();
        let proven_forms = by_search(&scripted_view(wire_forms), &deluxe)
            .await
            .expect("mb answers");
        assert_eq!(
            pick(&deluxe, proven_forms).as_deref(),
            Some(GROUP_A),
            "ISRC proof on a later title form must win over a listing match on the first"
        );
    }

    #[tokio::test]
    async fn listing_uses_canonical_release_and_track_display_titles() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Search Alias", "Aurora Current", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Canonical Album",
                "Album",
                &[("First Song", None, &[]), ("Second Song", None, &[])],
            ),
        );
        let view = scripted_view(wire);

        let album = SpotifyAlbum {
            title: "Canonical Album".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["First Song".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            prove(&view, &album).await.expect("mb answers").as_deref(),
            Some(GROUP_A),
            "listing should use release.title and track.display_title()"
        );
    }

    #[tokio::test]
    async fn cache_key_distinguishes_song_boundaries() {
        let dir = ScratchDir::new("spotify-albums-cache").expect("scratch dir");
        let crypto = Crypto::from_key_bytes(&[7; 32]).expect("crypto");
        let store = ConfigStore::scratch(dir.path(), crypto).expect("config store");
        store
            .save(MusicBrainzSettings {
                source_mode: MbSourceMode::Official,
                selected_source_mode: MbSourceMode::Official,
                ..MusicBrainzSettings::default()
            })
            .await
            .expect("mb settings save");

        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{MB_ROOT}/release"),
            200,
            JSON_HEADER,
            format!(
                r#"{{"releases":[{}]}}"#,
                search_hit(REL_1, GROUP_A, "Boundary Album", "Aurora Current", "Album")
            ),
        );
        wire.answer(
            &format!("{MB_ROOT}/release/{REL_1}"),
            200,
            JSON_HEADER,
            release_doc(
                REL_1,
                GROUP_A,
                "Boundary Album",
                "Album",
                &[("A B", Some("A B"), &[]), ("C", Some("C"), &[])],
            ),
        );
        let providers = Providers::unpaced(wire);
        let resolver = AlbumResolver::new(&providers, store.live());

        let first = SpotifyAlbum {
            title: "Boundary Album".to_owned(),
            artist: "Aurora Current".to_owned(),
            kind: Some("album".to_owned()),
            total_tracks: Some(2),
            songs: BTreeSet::from(["A B".to_owned(), "C".to_owned()]),
            isrcs: BTreeSet::new(),
        };
        assert_eq!(
            resolver
                .resolve(&first)
                .await
                .expect("mb answers")
                .as_deref(),
            Some(GROUP_A)
        );

        let second = SpotifyAlbum {
            songs: BTreeSet::from(["A".to_owned(), "B C".to_owned()]),
            ..first
        };
        assert_eq!(
            resolver.resolve(&second).await.expect("mb answers"),
            None,
            "songs {{\"A B\", \"C\"}} and {{\"A\", \"B C\"}} must not share a cache key"
        );
    }
}
