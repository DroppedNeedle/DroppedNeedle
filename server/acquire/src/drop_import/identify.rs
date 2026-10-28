//! Which album is this folder? The scanner's tiers, on the landing's
//! matching engine.
//!
//! 1. Consistent MusicBrainz tags name the album: the files are scored
//!    against that album's editions only. Tags that name an album the
//!    files do not match go to review; they never fall through to a
//!    guess.
//! 2. Otherwise the album and artist (tags, else the folder name) are
//!    searched and every edition found is scored.
//! 3. When that finds nothing close and an AcoustID key is set, the audio
//!    is fingerprinted and the releases it is heard on are scored.
//!
//! A match must be close (library distance at most
//! [`ACCEPT_ALBUM`], album title and artist agreeing). Once the album is
//! known, the edition the library already keeps for it wins: files that
//! are not close to that edition go to review instead of importing as
//! another edition.

use std::collections::HashMap;

use super::extract::LOOSE_UNIT_NAME;
use crate::landing::matching::{
    FoundRelease, MatchState, MatchSummary, choose, find, local_album, merged_recordings,
};
use crate::landing::ports::LandingLibrary;
use crate::landing::probe::Landing;
use crate::landing::specs::Target;
use crate::reason::Reason;
use droppedneedle_library::matching::Release;
use droppedneedle_library::matching::decide::ACCEPT_ALBUM;
use droppedneedle_providers::RequestPriority;

/// Releases fetched from fingerprint or group lookups.
const FETCH: usize = 3;

/// A person matching a review item by hand waits on the answer; working
/// out a dropped folder by itself runs in the background.
const PERSON: RequestPriority = RequestPriority::UserInitiated;

/// What identification decided.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The album, its chosen edition, and how the files pair with it.
    Matched(Box<MatchSummary>),
    /// A person has to decide.
    Review {
        reason: Reason,
        /// Specifics for the sentence (the album the tags name, ...).
        detail: Option<String>,
    },
    /// MusicBrainz could not answer; worth another try later.
    Unavailable(String),
}

fn review(reason: Reason) -> Outcome {
    Outcome::Review {
        reason,
        detail: None,
    }
}

/// Close enough to import without asking (Lidarr's `CloseAlbumMatch`).
fn close(found: &FoundRelease) -> bool {
    !found.scored.pairs.is_empty()
        && found.scored.names_agree
        && found.scored.conflicts.is_empty()
        && found.scored.library_distance() <= ACCEPT_ALBUM
}

/// The one release group every file's tags name, when they all agree.
fn tagged_group(landing: &Landing) -> Option<String> {
    let mut groups = landing.audio.iter().map(|file| {
        file.tag
            .musicbrainz_release_group_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_ascii_lowercase)
    });
    let first = groups.next()??;
    groups
        .all(|group| group.as_deref() == Some(first.as_str()))
        .then_some(first)
}

/// Album and artist to search with: the tags, else `Artist - Album` from
/// the folder name, else the folder name as the album.
fn names(landing: &Landing, folder: &str) -> (String, String) {
    let common = |values: Vec<&str>| {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for value in values.into_iter().filter(|value| !value.is_empty()) {
            *counts.entry(value).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
            .map(|(value, _)| value.to_owned())
    };
    let album = common(landing.audio.iter().map(|f| f.tag.album.trim()).collect());
    let artist = common(
        landing
            .audio
            .iter()
            .map(|file| {
                file.tag
                    .album_artist
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(file.tag.artist.trim())
            })
            .collect(),
    );
    let from_folder = (folder != LOOSE_UNIT_NAME).then(|| match folder.split_once(" - ") {
        Some((artist, album)) => (album.trim().to_owned(), artist.trim().to_owned()),
        None => (folder.trim().to_owned(), String::new()),
    });
    let (folder_album, folder_artist) = from_folder.unwrap_or_default();
    (
        album.unwrap_or(folder_album),
        artist.unwrap_or(folder_artist),
    )
}

/// Identify one folder's files.
pub async fn identify(library: &dyn LandingLibrary, landing: &Landing, folder: &str) -> Outcome {
    let (album_title, artist_name) = names(landing, folder);
    let tagged = tagged_group(landing);
    let target = Target {
        artist_name,
        album_title,
        release_group_mbid: tagged.clone().unwrap_or_default(),
        ..Target::default()
    };
    let summary = find(library, &target, landing).await;
    if let MatchState::Unavailable(detail) = &summary.state {
        return Outcome::Unavailable(detail.clone());
    }
    if summary.best().is_some_and(close) {
        return chosen_edition(library, landing, summary, &target).await;
    }
    if tagged.is_some() {
        return Outcome::Review {
            reason: Reason::TagsDisagree,
            detail: summary
                .best()
                .map(|found| format!("The tags name {}.", found.release.title)),
        };
    }
    match by_fingerprint(library, landing, &target).await {
        Ok(Some(summary)) => chosen_edition(library, landing, summary, &target).await,
        Ok(None) => review(Reason::NoAlbumMatch),
        Err(detail) => Outcome::Unavailable(detail),
    }
}

/// Files a person assigned to one release group: the library's edition of
/// it when it keeps one, else the group's closest edition, out of the
/// release the files' tags name (when it is of the group) and the
/// editions closest by track count. No closeness gate: the person's choice
/// of album is the authority.
pub async fn assigned(
    library: &dyn LandingLibrary,
    landing: &Landing,
    release_group_mbid: &str,
) -> Outcome {
    let group = release_group_mbid.trim().to_ascii_lowercase();
    let mut target = Target {
        release_group_mbid: group.clone(),
        release_mbid: library.chosen_edition(&group).await,
        ..Target::default()
    };
    let local = local_album(&landing.audio, &target);
    let mut releases: Vec<Release> = Vec::new();
    if let Some(pin) = target.release_mbid.clone() {
        match library.release(&pin, PERSON).await {
            Ok(Some(release)) => releases.push(release),
            Ok(None) => target.release_mbid = None,
            Err(error) => return Outcome::Unavailable(error.0),
        }
    }
    if releases.is_empty()
        && let Some(tagged) = local.tagged_release()
    {
        match library.release(&tagged, PERSON).await {
            Ok(Some(release)) if release.release_group_id.eq_ignore_ascii_case(&group) => {
                releases.push(release);
            }
            Ok(_) => {}
            Err(error) => return Outcome::Unavailable(error.0),
        }
    }
    if target.release_mbid.is_none() {
        let mut hits = match library.group_releases(&group, PERSON).await {
            Ok(hits) => hits,
            Err(error) => return Outcome::Unavailable(error.0),
        };
        let files = landing.audio.len();
        hits.sort_by_key(|hit| {
            hit.track_count
                .map_or(usize::MAX, |count| (count as usize).abs_diff(files))
        });
        hits.retain(|hit| !releases.iter().any(|known| known.answers_to(&hit.id)));
        for hit in hits.iter().take(FETCH) {
            match library.release(&hit.id, PERSON).await {
                Ok(Some(release)) if release.release_group_id.eq_ignore_ascii_case(&group) => {
                    releases.push(release);
                }
                Ok(_) => {}
                Err(error) => return Outcome::Unavailable(error.0),
            }
        }
    }
    let aliases = match merged_recordings(library, &local, &releases, PERSON).await {
        Ok(aliases) => aliases,
        Err(detail) => return Outcome::Unavailable(detail),
    };
    let mut summary = choose(&local, releases, &aliases, &target);
    if summary.best().is_none() {
        return review(Reason::AlbumNotFound);
    }
    summary.owned = library.owned(&group, None).await;
    Outcome::Matched(Box::new(summary))
}

/// The third tier: releases the audio is heard on, voted by file.
async fn by_fingerprint(
    library: &dyn LandingLibrary,
    landing: &Landing,
    target: &Target,
) -> Result<Option<MatchSummary>, String> {
    let files: Vec<(String, std::path::PathBuf)> = landing
        .audio
        .iter()
        .enumerate()
        .map(|(index, file)| (index.to_string(), file.path.clone()))
        .collect();
    let heard = library.fingerprint_releases(files).await;
    if heard.is_empty() {
        return Ok(None);
    }
    let mut votes: HashMap<String, usize> = HashMap::new();
    for releases in heard.values() {
        for release in releases {
            *votes.entry(release.clone()).or_default() += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = votes.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut releases = Vec::new();
    for (id, _) in ranked.into_iter().take(FETCH) {
        if let Some(release) = library
            .release(&id, RequestPriority::BackgroundSync)
            .await
            .map_err(|error| error.0)?
        {
            releases.push(release);
        }
    }
    let local = local_album(&landing.audio, target);
    let aliases =
        merged_recordings(library, &local, &releases, RequestPriority::BackgroundSync).await?;
    let summary = choose(&local, releases, &aliases, target);
    Ok(summary.best().is_some_and(close).then_some(summary))
}

/// Honour the edition the library keeps for the matched album, and note
/// what the library already holds of it.
async fn chosen_edition(
    library: &dyn LandingLibrary,
    landing: &Landing,
    mut summary: MatchSummary,
    target: &Target,
) -> Outcome {
    let Some(found) = summary.best() else {
        return review(Reason::NoAlbumMatch);
    };
    let group = found.release.release_group_id.to_ascii_lowercase();
    if let Some(pin) = library.chosen_edition(&group).await
        && !found.release.answers_to(&pin)
    {
        let pinned = Target {
            release_group_mbid: group.clone(),
            release_mbid: Some(pin),
            ..target.clone()
        };
        let again = find(library, &pinned, landing).await;
        if let MatchState::Unavailable(detail) = &again.state {
            return Outcome::Unavailable(detail.clone());
        }
        if !again.best().is_some_and(close) {
            return Outcome::Review {
                reason: Reason::NotChosenEdition,
                detail: Some(format!(
                    "They look like {} by {}.",
                    found.release.title,
                    found.release.artist_text()
                )),
            };
        }
        summary = again;
    }
    summary.owned = library.owned(&group, None).await;
    Outcome::Matched(Box::new(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_split_artist_and_album() {
        let landing = Landing::default();
        assert_eq!(
            names(&landing, "Boards of Canada - Geogaddi"),
            ("Geogaddi".to_owned(), "Boards of Canada".to_owned())
        );
        assert_eq!(
            names(&landing, "Geogaddi"),
            ("Geogaddi".to_owned(), String::new())
        );
        assert_eq!(
            names(&landing, LOOSE_UNIT_NAME),
            (String::new(), String::new())
        );
    }
}
