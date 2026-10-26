//! Which release a single track is fetched as part of.
//!
//! A recording usually sits on many releases: the album in several
//! editions, singles, compilations, live sets. The order tried is: an
//! edition the library has chosen for one of those albums (so a track
//! request fills the album the user already holds, in the edition they
//! picked), then the release the user requested it from, then the best
//! official release (Official, not live or remix, no secondary types,
//! Album before EP before Single, earliest). The secondary-type penalty
//! ranks ahead of the primary type, so a compilation or soundtrack album
//! falls behind the artist's own release: a download is named after that
//! release, not after a compilation that lists the track.

use std::collections::{HashMap, HashSet};

use super::lookup::ReleaseCandidate;
use crate::edition::{ChosenEdition, EditionBasis};
use droppedneedle_providers::musicbrainz::recording_release_group_rank;

/// Release groups whose library edition is read, at most. Each costs one
/// local query.
pub const MAX_GROUPS_CHECKED: usize = 12;

/// Releases tried in order, with why each is a candidate.
pub fn release_order(
    candidates: &[ReleaseCandidate],
    library: &HashMap<String, ChosenEdition>,
    requested: Option<&str>,
) -> Vec<(String, &'static str)> {
    let on_recording: HashSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let push = |id: &str, basis: &'static str, out: &mut Vec<(String, &'static str)>| {
        let id = id.trim().to_ascii_lowercase();
        if !id.is_empty() && !out.iter().any(|(seen, _)| *seen == id) {
            out.push((id, basis));
        }
    };
    // Library editions that carry this recording: a person's choice first.
    let mut chosen: Vec<&ChosenEdition> = library
        .values()
        .filter(|edition| on_recording.contains(edition.release_mbid.as_str()))
        .collect();
    chosen.sort_by_key(|edition| {
        (
            match edition.basis {
                EditionBasis::Manual => 0,
                EditionBasis::BestFit => 1,
            },
            edition.release_mbid.clone(),
        )
    });
    for edition in chosen {
        push(&edition.release_mbid, edition.basis.as_str(), &mut out);
    }
    // The release the user asked from, even when the recording lookup
    // left it out (MusicBrainz caps linked releases); the tracklist check
    // proves it carries the recording.
    if let Some(requested) = requested {
        push(requested, "requested_release", &mut out);
    }
    let mut ranked: Vec<&ReleaseCandidate> = candidates.iter().collect();
    ranked.sort_by_key(|candidate| {
        let (official, version_changed, primary, secondary_penalty, date, group) =
            recording_release_group_rank(
                candidate.status.as_deref(),
                &candidate.secondary_types,
                candidate.primary_type.as_deref(),
                candidate.date.as_deref(),
                &candidate.release_group_mbid,
            );
        // Official and version-keeping decide first. A secondary type then
        // falls behind the artist's own release, whatever its primary type.
        (
            official,
            version_changed,
            secondary_penalty,
            primary,
            date,
            group,
            candidate.id.clone(),
        )
    });
    for candidate in ranked {
        push(&candidate.id, "best_official", &mut out);
    }
    out
}

/// Distinct release groups worth checking for a library edition, best
/// ranked first.
pub fn groups_to_check(candidates: &[ReleaseCandidate]) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();
    for candidate in candidates {
        if !groups.contains(&candidate.release_group_mbid) {
            groups.push(candidate.release_group_mbid.clone());
        }
    }
    groups.truncate(MAX_GROUPS_CHECKED);
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, group: &str, primary: &str, secondary: &[&str]) -> ReleaseCandidate {
        ReleaseCandidate {
            id: id.to_owned(),
            status: Some("Official".to_owned()),
            date: Some("2000-01-01".to_owned()),
            release_group_mbid: group.to_owned(),
            primary_type: Some(primary.to_owned()),
            secondary_types: secondary.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn library_edition_then_request_then_album_before_compilation() {
        let candidates = vec![
            candidate("comp", "g-comp", "Album", &["Compilation"]),
            candidate("single", "g-single", "Single", &[]),
            candidate("album", "g-album", "Album", &[]),
            candidate("deluxe", "g-album", "Album", &[]),
        ];
        let none = HashMap::new();
        let order: Vec<_> = release_order(&candidates, &none, None)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            order[0], "album",
            "a plain album outranks single and compilation"
        );
        // The artist's own single comes before a compilation album.
        assert_eq!(order, ["album", "deluxe", "single", "comp"]);

        let mut library = HashMap::new();
        library.insert(
            "g-album".to_owned(),
            ChosenEdition {
                release_mbid: "deluxe".to_owned(),
                basis: EditionBasis::Manual,
            },
        );
        let order = release_order(&candidates, &library, Some("single"));
        assert_eq!(order[0], ("deluxe".to_owned(), "manual_identity"));
        assert_eq!(order[1], ("single".to_owned(), "requested_release"));

        // Official still decides first: an official compilation beats a
        // bootleg single.
        let mut bootleg = candidate("bootleg", "g-boot", "Single", &[]);
        bootleg.status = Some("Bootleg".to_owned());
        let candidates = vec![
            bootleg,
            candidate("wire-08", "g-wire", "Album", &["Compilation"]),
        ];
        let order: Vec<_> = release_order(&candidates, &none, None)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(order, ["wire-08", "bootleg"]);
    }
}
