//! The order a station by sound plays in: a walk that keeps each track
//! near the one before it as well as near the seed that found it.
//!
//! Candidates are sorted by their distance to their seed and cut into
//! buckets of [`BUCKET`]. In each bucket the next pick is the candidate
//! with the lowest `0.7 x distance to the previous pick + 0.3 x distance to
//! its seed`, one track per artist per bucket, never three in a row by one
//! artist and never more than [`MOST_IN_STRETCH`] by one artist in
//! [`STRETCH`] tracks. A candidate its bucket could not take for its
//! artist waits for the next one; a bucket that can take none is passed.

use std::collections::{HashMap, HashSet};

/// Candidates walked together.
pub const BUCKET: usize = 50;
/// How much the previous pick counts against the seed.
const TO_PREVIOUS: f32 = 0.7;
const TO_SEED: f32 = 0.3;
/// Most tracks in a row by one artist.
const MOST_IN_A_ROW: usize = 2;
/// Most tracks by one artist in [`STRETCH`] tracks.
pub const MOST_IN_STRETCH: usize = 4;
/// The stretch [`MOST_IN_STRETCH`] counts over.
pub const STRETCH: usize = 30;

/// One track a walk may take.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// The library track.
    pub track_id: String,
    /// Its artist, as the per-artist rules count it.
    pub artist: String,
    /// Its distance to the seed that found it, with any push down added.
    pub to_seed: f32,
}

/// Up to `count` of `candidates` in walking order, starting from `start`
/// (the last track played, or the seed) and after `recent` (the artists of
/// the tracks played last, oldest first). `distance` answers how far apart
/// two tracks sound; a pair it cannot answer counts by the seed alone.
pub fn walk(
    mut candidates: Vec<Step>,
    start: Option<&str>,
    recent: &[String],
    count: usize,
    distance: impl Fn(&str, &str) -> Option<f32>,
) -> Vec<Step> {
    candidates.sort_by(|a, b| {
        a.to_seed
            .total_cmp(&b.to_seed)
            .then_with(|| a.track_id.cmp(&b.track_id))
    });
    let mut history: Vec<String> = recent[recent.len().saturating_sub(STRETCH)..].to_vec();
    let mut previous: Option<String> = start.map(str::to_owned);
    let mut picked: Vec<Step> = Vec::new();
    while picked.len() < count && !candidates.is_empty() {
        let bucket = candidates.len().min(BUCKET);
        let mut taken: HashSet<usize> = HashSet::new();
        let mut artists: HashSet<String> = HashSet::new();
        while picked.len() < count {
            let mut best: Option<(f32, usize)> = None;
            for (at, step) in candidates[..bucket].iter().enumerate() {
                if taken.contains(&at)
                    || artists.contains(&step.artist)
                    || !allowed(&history, &step.artist)
                {
                    continue;
                }
                let to_previous = previous
                    .as_deref()
                    .and_then(|previous| distance(previous, &step.track_id));
                let score = match to_previous {
                    Some(to_previous) => TO_PREVIOUS * to_previous + TO_SEED * step.to_seed,
                    None => step.to_seed,
                };
                if best.is_none_or(|(lowest, _)| score < lowest) {
                    best = Some((score, at));
                }
            }
            let Some((_, at)) = best else {
                break;
            };
            let step = candidates[at].clone();
            taken.insert(at);
            artists.insert(step.artist.clone());
            history.push(step.artist.clone());
            previous = Some(step.track_id.clone());
            picked.push(step);
        }
        if taken.is_empty() {
            // Nothing in this bucket may follow now: every artist in it is
            // at its limit. The tracks further out still may.
            candidates.drain(..bucket);
            continue;
        }
        let mut at = 0;
        candidates.retain(|_| {
            let keep = !taken.contains(&at);
            at += 1;
            keep
        });
    }
    picked
}

/// Whether one more track by `artist` keeps the rules: never three in a
/// row and at most [`MOST_IN_STRETCH`] in the last [`STRETCH`].
pub fn allowed(history: &[String], artist: &str) -> bool {
    let in_a_row = history.len() >= MOST_IN_A_ROW
        && history[history.len() - MOST_IN_A_ROW..]
            .iter()
            .all(|recent| recent == artist);
    let stretch = &history[history.len().saturating_sub(STRETCH - 1)..];
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for recent in stretch {
        *counts.entry(recent.as_str()).or_default() += 1;
    }
    !in_a_row && counts.get(artist).copied().unwrap_or(0) < MOST_IN_STRETCH
}
