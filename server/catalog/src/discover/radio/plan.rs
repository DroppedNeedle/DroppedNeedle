//! The pure parts of a radio plan: pool
//! cleanup and the interleave that turns a library pool and a provider
//! pool into one finite, varied station.

use std::collections::HashSet;

use super::walk::{STRETCH, allowed};
use crate::discover::models::{RadioPlanTrack, StationTrack};

/// Seed artists one station spreads over.
pub const MAX_SEED_ARTISTS: usize = 8;
/// Provider tracks read per seed artist.
pub const TRACKS_PER_ARTIST: usize = 5;
/// Owned tracks read for one station.
pub const LIBRARY_POOL: i64 = 120;

/// The `artist|title` key two tracks share when they are the same song.
pub fn song_key(artist: &str, title: &str) -> String {
    format!("{}|{}", artist.to_lowercase(), title.to_lowercase())
}

/// The keys that leave a station's tracks out: each one's recording and
/// its artist and title, so another copy of one is left out too.
pub fn station_keys(tracks: &[StationTrack]) -> HashSet<String> {
    tracks
        .iter()
        .flat_map(|track| {
            let recording = track
                .recording_mbid
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_lowercase);
            recording
                .into_iter()
                .chain([song_key(&track.artist_name, &track.track_name)])
        })
        .collect()
}

/// The artist the per-artist rules count a station's track under, as
/// [`artist_key`] counts a plan track.
pub fn station_artist_key(track: &StationTrack) -> String {
    if track.artist_mbid.is_empty() {
        track.artist_name.to_lowercase()
    } else {
        track.artist_mbid.to_lowercase()
    }
}

/// The artist the per-artist rules count a plan track under.
pub fn artist_key(track: &RadioPlanTrack) -> String {
    if track.artist_mbid.is_empty() {
        track.artist_name.to_lowercase()
    } else {
        track.artist_mbid.to_lowercase()
    }
}

/// Drop the station's tracks (`exclude`, from [`station_keys`]), excluded
/// library tracks and repeated songs, keeping the first copy.
pub fn clean_pool(
    tracks: Vec<RadioPlanTrack>,
    exclude: &HashSet<String>,
    excluded_tracks: &HashSet<String>,
) -> Vec<RadioPlanTrack> {
    let mut seen = HashSet::new();
    tracks
        .into_iter()
        .filter(|track| {
            let recording = track.recording_mbid.as_deref().unwrap_or("").to_lowercase();
            let key = song_key(&track.artist_name, &track.track_name);
            if (!recording.is_empty() && exclude.contains(&recording)) || exclude.contains(&key) {
                return false;
            }
            if track
                .local_file_id
                .as_ref()
                .is_some_and(|id| excluded_tracks.contains(id))
            {
                return false;
            }
            seen.insert(song_key(&track.artist_name, &track.track_name))
        })
        .collect()
}

/// Interleave the pools (library only, or library and provider about half
/// and half), after `recent` (the artists of the tracks the station played
/// last, oldest first), with at most four tracks per artist in thirty and
/// never three in a row by one artist.
pub fn mix(
    library: Vec<RadioPlanTrack>,
    external: Vec<RadioPlanTrack>,
    count: usize,
    hybrid: bool,
    recent: &[String],
) -> Vec<RadioPlanTrack> {
    let pools: Vec<Vec<RadioPlanTrack>> = if hybrid {
        vec![library, external]
    } else {
        vec![library]
    }
    .into_iter()
    .filter(|pool| !pool.is_empty())
    .collect();
    if pools.is_empty() {
        return Vec::new();
    }
    let total: usize = pools.iter().map(Vec::len).sum();
    let mut cursors = vec![0usize; pools.len()];
    let mut selected: Vec<RadioPlanTrack> = Vec::new();
    let mut seen = HashSet::new();
    // The station's last tracks and this chunk's picks, which every
    // stretch of thirty is counted over as it slides.
    let mut history: Vec<String> = recent[recent.len().saturating_sub(STRETCH)..].to_vec();
    let mut stall = 0usize;
    let mut turn = 0usize;
    while selected.len() < count && stall < total + pools.len() {
        let which = turn % pools.len();
        turn += 1;
        let Some(track) = pools[which].get(cursors[which]) else {
            stall += 1;
            continue;
        };
        cursors[which] += 1;
        let key = song_key(&track.artist_name, &track.track_name);
        let artist = artist_key(track);
        if seen.contains(&key) || !allowed(&history, &artist) {
            stall += 1;
            continue;
        }
        seen.insert(key);
        history.push(artist);
        selected.push(track.clone());
        stall = 0;
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(artist: &str, title: &str, owned: bool) -> RadioPlanTrack {
        RadioPlanTrack {
            track_name: title.to_owned(),
            artist_name: artist.to_owned(),
            artist_mbid: String::new(),
            recording_mbid: None,
            album_mbid: None,
            album_name: None,
            in_library: owned,
            local_file_id: None,
            file_format: None,
            duration_s: None,
            reason: None,
        }
    }

    /// No stretch of thirty tracks across two chunks holds more than four
    /// by one artist: the counts slide over the chunk before.
    #[test]
    fn four_in_thirty_holds_across_chunks() {
        let other = |name: String| track(&name, "song", true);
        let heavy = |name: String| track("Heavy", &name, true);
        // The first chunk ends on the heavy artist; the second's pool has
        // it every other track.
        let first = mix(
            (0..26)
                .map(|at| other(format!("a{at}")))
                .chain((0..10).map(|at| heavy(format!("a{at}"))))
                .collect(),
            Vec::new(),
            30,
            false,
            &[],
        );
        let recent: Vec<String> = first.iter().map(artist_key).collect();
        let second = mix(
            (0..10)
                .flat_map(|at| [heavy(format!("b{at}")), other(format!("b{at}"))])
                .chain((10..40).map(|at| other(format!("b{at}"))))
                .collect(),
            Vec::new(),
            30,
            false,
            &recent,
        );
        let played: Vec<String> = first.iter().chain(&second).map(artist_key).collect();
        for stretch in played.windows(STRETCH) {
            let heavy = stretch.iter().filter(|artist| *artist == "heavy").count();
            assert!(heavy <= 4, "{heavy} by one artist in {stretch:?}");
        }
        assert_eq!(second.len(), 30);
    }

    #[test]
    fn library_mode_ignores_the_provider_pool() {
        let plan = mix(
            vec![track("A", "one", true)],
            vec![track("B", "two", false)],
            5,
            false,
            &[],
        );
        assert_eq!(plan.len(), 1);
        assert!(plan[0].in_library);
    }
}
