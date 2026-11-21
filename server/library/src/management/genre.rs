//! Genre projection for the managed genre field, with the
//! `GenreProjectionService` and `GenreNormalizer`.
//!
//! Candidates come from the profile's sources in order: MusicBrainz
//! (the release's, its release group's and its recordings' genres, as
//! the release document carries them), ListenBrainz and Last.fm (fetched
//! per album before planning, see `enrich`), and the file's own genres.
//! Each candidate is folded (NFC, trimmed, whitespace collapsed, case
//! folded), aliased, filtered by the allow and deny lists, given its
//! canonical display spelling, and deduplicated. A source that could not
//! be asked is *deferred*: when that leaves only MusicBrainz's answer (or
//! nothing) and the file has genres, the file's genres are kept rather
//! than replaced by a partial answer. A source the admin switched off, or
//! one with no usable key, is not a source at all: it is never asked and
//! defers nothing.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use unicode_normalization::UnicodeNormalization as _;

use crate::matching::Release;
use droppedneedle_kernel::runtime_config::sections::{
    GenreManagementSettings, GenreMode, GenreSource,
};

/// The MusicBrainz genre vocabulary: canonical spellings and
/// aliases (the parent map rides along unused: genres are written
/// without their ancestors).
const VOCABULARY: &str = include_str!("assets/genres.json");

/// A genre a source suggests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub count: Option<i64>,
    /// ListenBrainz marks curated genres; everything else is a free tag.
    pub curated: bool,
}

/// What one remote genre source answered for an album.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SourceGenres {
    /// It could not be asked now (an outage, a rate limit): deferred.
    #[default]
    Unanswered,
    /// The admin switched it off, or it has no usable key: not a source.
    Off,
    Answered(Vec<Candidate>),
}

/// What the remote sources answered for an album.
#[derive(Debug, Clone, Default)]
pub struct RemoteGenres {
    pub listenbrainz: SourceGenres,
    pub lastfm: SourceGenres,
}

/// The genres to write and how they were reached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Projection {
    pub genres: Vec<String>,
    /// Sources that could not be asked.
    pub deferred: Vec<&'static str>,
    /// The file's own genres were kept.
    pub preserved_existing: bool,
}

struct Vocabulary {
    display: HashMap<String, String>,
    aliases: HashMap<String, String>,
}

fn vocabulary() -> &'static Vocabulary {
    static VOCAB: OnceLock<Vocabulary> = OnceLock::new();
    VOCAB.get_or_init(|| {
        let parsed: serde_json::Value = serde_json::from_str(VOCABULARY).unwrap_or_default();
        let display = parsed["genres"]
            .as_array()
            .map(|genres| {
                genres
                    .iter()
                    .filter_map(|value| value.as_str())
                    .filter(|value| !fold(value).is_empty())
                    .map(|value| (fold(value), value.nfc().collect::<String>()))
                    .collect()
            })
            .unwrap_or_default();
        let pairs = |key: &str| -> HashMap<String, String> {
            parsed[key]
                .as_object()
                .map(|map| {
                    map.iter()
                        .filter_map(|(from, to)| Some((fold(from), fold(to.as_str()?))))
                        .collect()
                })
                .unwrap_or_default()
        };
        Vocabulary {
            display,
            aliases: pairs("aliases"),
        }
    })
}

/// Fold a genre for comparison: NFC, trimmed, whitespace collapsed,
/// case folded.
pub fn fold(value: &str) -> String {
    let normalized: String = value.nfc().collect();
    let collapsed = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    caseless::default_case_fold_str(&collapsed)
}

/// Fold a genre and resolve profile and vocabulary aliases when
/// canonicalization is on.
fn canonical_key(name: &str, settings: &GenreManagementSettings) -> Option<String> {
    let mut folded = fold(name);
    if folded.is_empty() {
        return None;
    }
    if settings.canonicalize {
        let vocab = vocabulary();
        let profile_alias = settings
            .aliases
            .iter()
            .find(|alias| fold(&alias.source) == folded)
            .map(|alias| fold(&alias.target));
        folded = profile_alias
            .or_else(|| vocab.aliases.get(&folded).cloned())
            .unwrap_or(folded);
    }
    Some(folded)
}

/// One candidate through the profile's rules: `(folded, display)`, or
/// `None` when the rules drop it.
fn normalize(
    name: &str,
    settings: &GenreManagementSettings,
    require_canonical: bool,
) -> Option<(String, String)> {
    let vocab = vocabulary();
    let folded = canonical_key(name, settings)?;
    let allowed: HashSet<String> = settings.allowlist.iter().map(|value| fold(value)).collect();
    let denied = settings.denylist.iter().any(|value| fold(value) == folded);
    if denied || (!allowed.is_empty() && !allowed.contains(&folded)) {
        return None;
    }
    if require_canonical && !vocab.display.contains_key(&folded) && !allowed.contains(&folded) {
        return None;
    }
    let display = settings
        .preferred_casing
        .iter()
        .find(|value| fold(value) == folded)
        .map(|value| value.nfc().collect::<String>().trim().to_owned())
        .or_else(|| vocab.display.get(&folded).cloned())
        .unwrap_or_else(|| {
            name.nfc()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
    Some((folded, display))
}

/// The file's genres as normalized candidates, deduplicated, used when
/// `Replace` includes `ExistingLocal` in its source order.
fn existing_genres(values: &[String], settings: &GenreManagementSettings) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    values
        .iter()
        .filter_map(|value| normalize(value, settings, false))
        .filter(|(folded, _)| seen.insert(folded.clone()))
        .collect()
}

/// Every MusicBrainz genre of the release that clears the vote floor:
/// the release's and release group's, then each recording's.
fn musicbrainz_genres(release: &Release, settings: &GenreManagementSettings) -> Vec<Candidate> {
    let floor = settings.musicbrainz_minimum_count;
    release
        .genres
        .iter()
        .chain(release.tracks.iter().flat_map(|track| track.genres.iter()))
        .filter(|genre| genre.count.is_none_or(|count| i64::from(count) >= floor))
        .map(|genre| Candidate {
            name: genre.name.clone(),
            count: genre.count.map(i64::from),
            curated: true,
        })
        .collect()
}

enum SourceOutcome {
    Candidates(Vec<Candidate>),
    Off,
    Deferred(&'static str),
}

fn source_outcome(
    source: GenreSource,
    settings: &GenreManagementSettings,
    release: &Release,
    remote: &RemoteGenres,
) -> SourceOutcome {
    match source {
        GenreSource::Musicbrainz => {
            SourceOutcome::Candidates(musicbrainz_genres(release, settings))
        }
        GenreSource::Listenbrainz => match &remote.listenbrainz {
            SourceGenres::Answered(values) => SourceOutcome::Candidates(
                values
                    .iter()
                    .filter(|value| {
                        value
                            .count
                            .is_none_or(|count| count >= settings.listenbrainz_minimum_count)
                    })
                    .filter(|value| !settings.listenbrainz_curated_only || value.curated)
                    .cloned()
                    .collect(),
            ),
            SourceGenres::Off => SourceOutcome::Off,
            SourceGenres::Unanswered => SourceOutcome::Deferred("listenbrainz"),
        },
        GenreSource::Lastfm => match &remote.lastfm {
            SourceGenres::Answered(values) => SourceOutcome::Candidates(
                values
                    .iter()
                    .filter(|value| {
                        value
                            .count
                            .is_none_or(|weight| weight >= settings.lastfm_minimum_weight)
                    })
                    .cloned()
                    .collect(),
            ),
            SourceGenres::Off => SourceOutcome::Off,
            SourceGenres::Unanswered => SourceOutcome::Deferred("lastfm"),
        },
        GenreSource::ExistingLocal => SourceOutcome::Off,
    }
}

/// Project the genres one file of `release` should carry.
pub fn project(
    settings: &GenreManagementSettings,
    release: &Release,
    remote: &RemoteGenres,
    existing: &[String],
) -> Projection {
    let has_existing = existing.iter().any(|value| !value.trim().is_empty());
    if settings.mode == GenreMode::FillMissing && has_existing {
        return Projection {
            genres: existing.to_vec(),
            deferred: Vec::new(),
            preserved_existing: true,
        };
    }

    let cap = usize::try_from(settings.maximum_count.max(1)).unwrap_or(1);
    let keep_current = |deferred: Vec<&'static str>| Projection {
        genres: existing.to_vec(),
        deferred,
        preserved_existing: true,
    };

    if settings.mode == GenreMode::Merge {
        let mut seen = HashSet::new();
        let mut genres: Vec<String> = Vec::new();
        for value in existing {
            if let Some(folded) = canonical_key(value, settings) {
                seen.insert(folded);
                genres.push(value.clone());
            }
        }
        let existing_count = genres.len();
        let mut deferred = Vec::new();
        'sources: for &source in &settings.sources {
            if source == GenreSource::ExistingLocal {
                continue;
            }
            if genres.len() >= cap {
                break;
            }
            let candidates = match source_outcome(source, settings, release, remote) {
                SourceOutcome::Candidates(candidates) => candidates,
                SourceOutcome::Off => continue,
                SourceOutcome::Deferred(name) => {
                    if !deferred.contains(&name) {
                        deferred.push(name);
                    }
                    continue;
                }
            };
            for candidate in candidates {
                let require_canonical = match source {
                    GenreSource::Listenbrainz => !candidate.curated,
                    GenreSource::Lastfm => settings.lastfm_whitelist_only,
                    _ => false,
                };
                let Some((folded, display)) =
                    normalize(&candidate.name, settings, require_canonical)
                else {
                    continue;
                };
                if seen.insert(folded) {
                    genres.push(display);
                    if genres.len() >= cap {
                        break 'sources;
                    }
                }
            }
        }
        if has_existing && genres.len() == existing_count {
            return keep_current(deferred);
        }
        return Projection {
            genres,
            deferred,
            preserved_existing: false,
        };
    }

    let mut deferred = Vec::new();
    let mut by_source: Vec<(GenreSource, Vec<Candidate>)> = Vec::new();
    for &source in &settings.sources {
        match source_outcome(source, settings, release, remote) {
            SourceOutcome::Candidates(candidates) => by_source.push((source, candidates)),
            SourceOutcome::Off => {}
            SourceOutcome::Deferred(name) => deferred.push(name),
        }
    }

    let current = existing_genres(existing, settings);
    let mut seen = HashSet::new();
    let mut selected: Vec<(String, String, GenreSource)> = Vec::new();
    'sources: for source in &settings.sources {
        if *source == GenreSource::ExistingLocal {
            for (folded, display) in &current {
                if seen.insert(folded.clone()) {
                    selected.push((folded.clone(), display.clone(), *source));
                    if selected.len() >= cap {
                        break 'sources;
                    }
                }
            }
            continue;
        }
        let Some((_, candidates)) = by_source.iter().find(|(known, _)| known == source) else {
            continue;
        };
        for candidate in candidates {
            let require_canonical = match source {
                GenreSource::Listenbrainz => !candidate.curated,
                GenreSource::Lastfm => settings.lastfm_whitelist_only,
                _ => false,
            };
            let Some((folded, display)) = normalize(&candidate.name, settings, require_canonical)
            else {
                continue;
            };
            if seen.insert(folded.clone()) {
                selected.push((folded, display, *source));
                if selected.len() >= cap {
                    break 'sources;
                }
            }
        }
    }
    let only_local_answer = selected
        .iter()
        .all(|(_, _, source)| *source == GenreSource::Musicbrainz);
    if has_existing
        && (selected.is_empty()
            || (settings.mode == GenreMode::Replace && !deferred.is_empty() && only_local_answer))
    {
        return keep_current(deferred);
    }
    Projection {
        genres: selected
            .into_iter()
            .map(|(_, display, _)| display)
            .collect(),
        deferred,
        preserved_existing: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::Genre;

    fn release(genres: &[(&str, u32)]) -> Release {
        Release {
            genres: genres
                .iter()
                .map(|(name, count)| Genre {
                    name: (*name).to_owned(),
                    count: Some(*count),
                })
                .collect(),
            ..Release::default()
        }
    }

    fn settings(mode: GenreMode, sources: Vec<GenreSource>) -> GenreManagementSettings {
        GenreManagementSettings {
            mode,
            sources,
            ..GenreManagementSettings::default()
        }
    }

    /// `project` per mode, source and list: MusicBrainz genres are
    /// canonicalized and capped, a source that did not answer keeps the
    /// file's genres verbatim, Fill missing leaves existing genres alone,
    /// Merge keeps existing genres as written and appends new provider
    /// genres, and the deny list filters provider candidates.
    #[test]
    fn genres_follow_mode_sources_and_lists() {
        let replace = || settings(GenreMode::Replace, vec![GenreSource::Musicbrainz]);
        let capped = GenreManagementSettings {
            maximum_count: 2,
            ..replace()
        };
        let deferring = settings(
            GenreMode::Replace,
            vec![GenreSource::Musicbrainz, GenreSource::Listenbrainz],
        );
        let fill = settings(GenreMode::FillMissing, vec![GenreSource::Musicbrainz]);
        let merge = settings(GenreMode::Merge, vec![GenreSource::Musicbrainz]);
        let denying = GenreManagementSettings {
            denylist: vec!["Rock".to_owned()],
            ..replace()
        };
        // (label, settings, release genres, the file's genres, genres,
        // deferred sources, the file's genres kept); `None` is not checked.
        #[allow(clippy::type_complexity)]
        let cases: [(
            &str,
            &GenreManagementSettings,
            &[(&str, u32)],
            &[&str],
            &[&str],
            Option<&[&str]>,
            Option<bool>,
        ); 6] = [
            (
                "canonicalized and capped",
                &capped,
                &[("alt rock", 3), ("Shoegaze", 2), ("jazz", 1)],
                &["Pop"],
                &["alternative rock", "shoegaze"],
                None,
                Some(false),
            ),
            (
                "a deferred source",
                &deferring,
                &[("rock", 3)],
                &["Pop"],
                &["Pop"],
                Some(&["listenbrainz"]),
                Some(true),
            ),
            (
                "fill missing, file has genres",
                &fill,
                &[("rock", 3)],
                &["Jazz"],
                &["Jazz"],
                None,
                Some(true),
            ),
            (
                "fill missing, file has none",
                &fill,
                &[("rock", 3)],
                &[],
                &["rock"],
                None,
                Some(false),
            ),
            (
                "merge",
                &merge,
                &[("rock", 3)],
                &["Jazz"],
                &["Jazz", "rock"],
                None,
                Some(false),
            ),
            (
                "deny list",
                &denying,
                &[("rock", 3), ("jazz", 2)],
                &[],
                &["jazz"],
                None,
                None,
            ),
        ];
        for (label, settings, genres, existing, expected, deferred, preserved) in cases {
            let existing: Vec<String> = existing.iter().map(|name| (*name).to_owned()).collect();
            let projection = project(
                settings,
                &release(genres),
                &RemoteGenres::default(),
                &existing,
            );
            assert_eq!(projection.genres, expected, "{label}");
            if let Some(deferred) = deferred {
                assert_eq!(projection.deferred, deferred, "{label}");
            }
            if let Some(preserved) = preserved {
                assert_eq!(projection.preserved_existing, preserved, "{label}");
            }
        }
    }

    /// A source the admin switched off is not a source: it neither defers
    /// nor keeps the file's genres, and the others answer. One that could
    /// not be reached still defers, and the file keeps its genres verbatim.
    #[test]
    fn a_switched_off_source_is_left_out_but_an_unreachable_one_defers() {
        let sources = settings(
            GenreMode::Replace,
            vec![GenreSource::Musicbrainz, GenreSource::Listenbrainz],
        );
        let existing = vec!["Pop".to_owned()];
        let answer = |listenbrainz| {
            project(
                &sources,
                &release(&[("rock", 3)]),
                &RemoteGenres {
                    listenbrainz,
                    ..RemoteGenres::default()
                },
                &existing,
            )
        };
        let off = answer(SourceGenres::Off);
        assert_eq!(off.genres, ["rock"]);
        assert!(off.deferred.is_empty(), "switched off defers nothing");
        assert!(!off.preserved_existing);
        let unreachable = answer(SourceGenres::Unanswered);
        assert_eq!(unreachable.genres, ["Pop"]);
        assert_eq!(unreachable.deferred, ["listenbrainz"]);
        assert!(unreachable.preserved_existing);
    }

    /// Fill missing leaves any file with non-blank genres untouched:
    /// spelling, count, denylist/allowlist, and unreachable remote sources
    /// never rewrite, truncate, or defer it. Blank-only values count as
    /// missing and are filled from providers.
    #[test]
    fn fill_missing_keeps_existing_genres_untouched() {
        let base = GenreManagementSettings {
            mode: GenreMode::FillMissing,
            sources: vec![GenreSource::Musicbrainz, GenreSource::Listenbrainz],
            canonicalize: false,
            maximum_count: 1,
            denylist: vec!["Progressive Rock".to_owned()],
            allowlist: vec!["jazz".to_owned()],
            ..GenreManagementSettings::default()
        };
        let existing = vec!["Progressive Rock".to_owned(), "Art Rock".to_owned()];
        let kept = project(
            &base,
            &release(&[("jazz", 3)]),
            &RemoteGenres::default(),
            &existing,
        );
        assert_eq!(kept.genres, existing);
        assert!(kept.deferred.is_empty());
        assert!(kept.preserved_existing);

        let blank_only = vec!["   ".to_owned()];
        let filled = project(
            &base,
            &release(&[("jazz", 3)]),
            &RemoteGenres {
                listenbrainz: SourceGenres::Off,
                ..RemoteGenres::default()
            },
            &blank_only,
        );
        assert_eq!(filled.genres, ["jazz"]);
        assert!(!filled.preserved_existing);
    }

    /// Replace keeps the file's spelling verbatim when no provider returns
    /// any matching candidate.
    #[test]
    fn replace_empty_provider_preserves_existing_verbatim() {
        let replace = settings(GenreMode::Replace, vec![GenreSource::Musicbrainz]);
        let existing = vec!["Pop".to_owned()];
        let projection = project(&replace, &release(&[]), &RemoteGenres::default(), &existing);
        assert_eq!(projection.genres, ["Pop"]);
        assert!(projection.preserved_existing);
    }

    /// Merge keeps the file's existing genres in their written spelling and
    /// order (never truncating past `maximum_count`), deduplicates provider
    /// tags against them through aliases and folding, and appends normalized
    /// new provider genres up to `maximum_count`.
    #[test]
    fn merge_keeps_existing_verbatim_and_appends_normalized_provider_genres() {
        use droppedneedle_kernel::runtime_config::sections::GenreAlias;

        let merge = settings(GenreMode::Merge, vec![GenreSource::Musicbrainz]);
        let overlap = project(
            &merge,
            &release(&[("progressive rock", 3), ("art rock", 2)]),
            &RemoteGenres::default(),
            &["Progressive Rock".to_owned()],
        );
        assert_eq!(overlap.genres, ["Progressive Rock", "art rock"]);
        assert!(!overlap.preserved_existing);

        let only_overlap = project(
            &merge,
            &release(&[("progressive rock", 3)]),
            &RemoteGenres::default(),
            &["Progressive Rock".to_owned()],
        );
        assert_eq!(only_overlap.genres, ["Progressive Rock"]);
        assert!(only_overlap.preserved_existing);

        let aliased = GenreManagementSettings {
            mode: GenreMode::Merge,
            sources: vec![GenreSource::Musicbrainz],
            canonicalize: true,
            aliases: vec![GenreAlias {
                source: "prog".to_owned(),
                target: "progressive rock".to_owned(),
            }],
            preferred_casing: vec!["PROG ROCK".to_owned(), "Alternative Rock".to_owned()],
            ..GenreManagementSettings::default()
        };
        let merged_alias = project(
            &aliased,
            &release(&[("prog", 4), ("alt rock", 3)]),
            &RemoteGenres::default(),
            &["Progressive Rock".to_owned()],
        );
        assert_eq!(
            merged_alias.genres,
            ["Progressive Rock", "Alternative Rock"]
        );
        assert!(!merged_alias.preserved_existing);

        let capped = GenreManagementSettings {
            mode: GenreMode::Merge,
            sources: vec![GenreSource::Musicbrainz],
            maximum_count: 1,
            ..GenreManagementSettings::default()
        };
        let kept_over_cap = project(
            &capped,
            &release(&[("jazz", 3)]),
            &RemoteGenres::default(),
            &["Rock".to_owned(), "Pop".to_owned()],
        );
        assert_eq!(kept_over_cap.genres, ["Rock", "Pop"]);
        assert!(kept_over_cap.preserved_existing);
    }

    /// Merge does not defer remote sources it never needs to consult because
    /// the file already met or reached `maximum_count`.
    #[test]
    fn merge_skips_deferring_unvisited_remote_sources_once_cap_is_reached() {
        let at_cap = GenreManagementSettings {
            mode: GenreMode::Merge,
            sources: vec![GenreSource::Musicbrainz, GenreSource::Listenbrainz],
            maximum_count: 1,
            ..GenreManagementSettings::default()
        };
        let over_cap = project(
            &at_cap,
            &release(&[("jazz", 3)]),
            &RemoteGenres::default(),
            &["Rock".to_owned(), "Pop".to_owned()],
        );
        assert_eq!(over_cap.genres, ["Rock", "Pop"]);
        assert!(over_cap.deferred.is_empty());
        assert!(over_cap.preserved_existing);

        let filled_before_remote = GenreManagementSettings {
            mode: GenreMode::Merge,
            sources: vec![GenreSource::Musicbrainz, GenreSource::Listenbrainz],
            maximum_count: 2,
            ..GenreManagementSettings::default()
        };
        let filled = project(
            &filled_before_remote,
            &release(&[("jazz", 3)]),
            &RemoteGenres::default(),
            &["Rock".to_owned()],
        );
        assert_eq!(filled.genres, ["Rock", "jazz"]);
        assert!(filled.deferred.is_empty());
        assert!(!filled.preserved_existing);
    }
}
