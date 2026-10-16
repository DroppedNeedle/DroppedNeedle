//! The pure parts of a radio plan (v2 `RadioPlanService`): pool
//! cleanup and the interleave that turns a library pool and a provider
//! pool into one finite, varied station.

use std::collections::{HashMap, HashSet};

use crate::reads::discover::models::RadioPlanTrack;

/// Seed artists one station spreads over.
pub const MAX_SEED_ARTISTS: usize = 8;
/// Provider tracks read per seed artist.
pub const TRACKS_PER_ARTIST: usize = 5;
/// Most tracks one artist gets in a plan.
const MAX_PER_ARTIST: usize = 4;
/// Most tracks in a row by the same artist.
const MAX_CONSECUTIVE_SAME_ARTIST: usize = 2;
/// Owned tracks read for one station.
pub const LIBRARY_POOL: i64 = 120;

/// The `artist|title` key two tracks share when they are the same song.
pub fn song_key(artist: &str, title: &str) -> String {
    format!("{}|{}", artist.to_lowercase(), title.to_lowercase())
}

fn artist_key(track: &RadioPlanTrack) -> String {
    if track.artist_mbid.is_empty() {
        track.artist_name.to_lowercase()
    } else {
        track.artist_mbid.to_lowercase()
    }
}

/// Drop excluded recordings and repeated songs, keeping the first copy.
pub fn clean_pool(tracks: Vec<RadioPlanTrack>, exclude: &HashSet<String>) -> Vec<RadioPlanTrack> {
    let mut seen = HashSet::new();
    tracks
        .into_iter()
        .filter(|track| {
            let recording = track.recording_mbid.as_deref().unwrap_or("").to_lowercase();
            if !recording.is_empty() && exclude.contains(&recording) {
                return false;
            }
            seen.insert(song_key(&track.artist_name, &track.track_name))
        })
        .collect()
}

/// Interleave the pools (library only, or library and provider about half
/// and half), with at most four tracks per artist and never three in a
/// row by one artist (v2 `_mix`).
pub fn mix(
    library: Vec<RadioPlanTrack>,
    external: Vec<RadioPlanTrack>,
    count: usize,
    hybrid: bool,
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
    let mut per_artist: HashMap<String, usize> = HashMap::new();
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
        let streak = selected.len() >= MAX_CONSECUTIVE_SAME_ARTIST
            && selected[selected.len() - MAX_CONSECUTIVE_SAME_ARTIST..]
                .iter()
                .all(|recent| artist_key(recent) == artist);
        if seen.contains(&key)
            || per_artist.get(&artist).copied().unwrap_or(0) >= MAX_PER_ARTIST
            || streak
        {
            stall += 1;
            continue;
        }
        seen.insert(key);
        *per_artist.entry(artist).or_insert(0) += 1;
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
        }
    }

    #[test]
    fn mix_alternates_pools_and_caps_artists() {
        let library: Vec<_> = (0..6)
            .map(|n| track("Portishead", &format!("own {n}"), true))
            .collect();
        let external = vec![
            track("Massive Attack", "Teardrop", false),
            track("Tricky", "Hell Is Round the Corner", false),
        ];
        let plan = mix(library, external, 10, true);
        // Four Portishead tracks at most, starting from the library pool.
        assert_eq!(
            plan.iter()
                .filter(|track| track.artist_name == "Portishead")
                .count(),
            4
        );
        assert!(plan[0].in_library);
        assert_eq!(plan[1].artist_name, "Massive Attack");
        // Never three in a row by one artist.
        for window in plan.windows(3) {
            assert!(
                !(window[0].artist_name == window[1].artist_name
                    && window[1].artist_name == window[2].artist_name)
            );
        }
    }

    #[test]
    fn library_mode_ignores_the_provider_pool() {
        let plan = mix(
            vec![track("A", "one", true)],
            vec![track("B", "two", false)],
            5,
            false,
        );
        assert_eq!(plan.len(), 1);
        assert!(plan[0].in_library);
    }
}
