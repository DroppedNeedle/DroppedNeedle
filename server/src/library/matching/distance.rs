//! Weighted distance, the model beets and Lidarr share.
//!
//! A distance is a set of named penalties, each in `[0, 1]`, each with a
//! weight. Its normalized value is the weighted sum over the weighted
//! maximum of the penalties that applied, so missing data never counts
//! as distance. A key may hold several penalties (one per matched track
//! under `tracks`, one per missing track under `missing_tracks`), and
//! each counts with the key's weight.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::strings::string_dist;

/// Penalty names and weights, Lidarr's table (`Distance.cs`).
pub fn weight(key: &str) -> f64 {
    match key {
        "artist" | "album" | "track_title" => 3.0,
        "album_id" => 5.0,
        "recording_id" => 10.0,
        "tracks" | "track_artist" | "track_length" => 2.0,
        "media_count" | "year" | "track_index" => 1.0,
        "missing_tracks" => 0.6,
        "unmatched_tracks" => 0.9,
        _ => 0.5,
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Distance {
    penalties: BTreeMap<&'static str, Vec<f64>>,
}

/// One penalty's share of a normalized distance, for review cards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PenaltyShare {
    pub name: String,
    pub share: f64,
}

impl Distance {
    pub fn add(&mut self, key: &'static str, penalty: f64) {
        self.penalties
            .entry(key)
            .or_default()
            .push(penalty.clamp(0.0, 1.0));
    }

    pub fn add_bool(&mut self, key: &'static str, mismatch: bool) {
        self.add(key, if mismatch { 1.0 } else { 0.0 });
    }

    /// `value / max`, clamped; a zero `max` adds nothing.
    pub fn add_ratio(&mut self, key: &'static str, value: f64, max: f64) {
        if max > 0.0 {
            self.add(key, value / max);
        }
    }

    pub fn add_string(&mut self, key: &'static str, left: &str, right: &str) {
        self.add(key, string_dist(left, right));
    }

    fn sums(&self, skip: &[&str]) -> (f64, f64) {
        let mut raw = 0.0;
        let mut max = 0.0;
        for (key, values) in &self.penalties {
            if skip.contains(key) {
                continue;
            }
            let weight = weight(key);
            raw += weight * values.iter().sum::<f64>();
            max += weight * values.len() as f64;
        }
        (raw, max)
    }

    /// Weighted distance in `[0, 1]`; zero when nothing applied.
    pub fn normalized(&self) -> f64 {
        self.normalized_excluding(&[])
    }

    /// The same, ignoring some keys (Lidarr does this for files already
    /// in the library, where a partial album is normal).
    pub fn normalized_excluding(&self, skip: &[&str]) -> f64 {
        let (raw, max) = self.sums(skip);
        if max > 0.0 { raw / max } else { 0.0 }
    }

    /// Each non-zero penalty's share of the normalized distance, largest
    /// first.
    pub fn shares(&self) -> Vec<PenaltyShare> {
        let (_, max) = self.sums(&[]);
        if max <= 0.0 {
            return Vec::new();
        }
        let mut shares: Vec<PenaltyShare> = self
            .penalties
            .iter()
            .map(|(key, values)| PenaltyShare {
                name: (*key).to_owned(),
                share: weight(key) * values.iter().sum::<f64>() / max,
            })
            .filter(|share| share.share > 0.0)
            .collect();
        shares.sort_by(|a, b| b.share.total_cmp(&a.share));
        shares
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_over_what_applied() {
        let mut distance = Distance::default();
        assert_eq!(distance.normalized(), 0.0);
        distance.add_bool("album", true); // 3 of 3
        distance.add("tracks", 0.0); // 0 of 2
        distance.add("tracks", 0.5); // 1 of 2
        distance.add("missing_tracks", 1.0); // 0.6 of 0.6
        let all = (3.0 + 1.0 + 0.6) / (3.0 + 4.0 + 0.6);
        assert!((distance.normalized() - all).abs() < 1e-9);
        let partial = (3.0 + 1.0) / (3.0 + 4.0);
        assert!((distance.normalized_excluding(&["missing_tracks"]) - partial).abs() < 1e-9);
        assert_eq!(distance.shares()[0].name, "album");
    }
}
