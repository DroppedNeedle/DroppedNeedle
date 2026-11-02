//! Top Picks scoring: "we think you'd like X, 78% match".
//!
//! Every signal is between 0 and 1:
//! - similarity of the album's artist to the user's seed artists
//!   (ListenBrainz scores are divided by the batch maximum, Last.fm's
//!   match is used as is; trending candidates score 0);
//! - genre overlap between the artist's genres and the user's top genres
//!   (`shared / 3`, capped at 1);
//! - popularity, `min(1, log10(listens + 1) / 6)`.
//!
//! The score is `0.5 sim + w genre + 0.15 pop` plus a small jitter that is
//! fixed for a user, an album and a day, so a refresh on the same day
//! keeps the same order while the list varies across days. `w` is half of
//! the genre-affinity setting (0.35 at the default 0.7). The match shown
//! is `40 + 58 score`, so nothing ever claims 100%.

use std::collections::{HashMap, HashSet};

/// One album that could become a pick.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Release-group MBID (lowercase).
    pub release_group_mbid: String,
    /// Album title.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Artist MBID (lowercase), empty when unknown.
    pub artist_mbid: String,
    /// Similarity to the seeds, 0 to 1.
    pub sim: f64,
    /// Listens behind the album (0 when unknown).
    pub listen_count: i64,
    /// The seed artist this candidate is similar to.
    pub seed_artist: Option<String>,
    /// True when it came from the worldwide chart, not similarity.
    pub from_trending: bool,
}

/// A scored candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// The candidate.
    pub candidate: Candidate,
    /// Score, 0 to 1.
    pub score: f64,
    /// Match shown to the user, 40 to 98.
    pub match_pct: i64,
    /// Up to two reasons in plain words.
    pub reasons: Vec<String>,
}

/// Per-user, per-album, per-day jitter between 0 and 0.05 (v2's md5 recipe).
fn daily_jitter(user_id: &str, mbid: &str, date_iso: &str) -> f64 {
    let digest = format!("{:x}", md5::compute(format!("{user_id}:{mbid}:{date_iso}")));
    let head = u64::from_str_radix(&digest[..6], 16).unwrap_or(0);
    (head % 1000) as f64 / 20_000.0
}

/// Score the candidates and pick up to `count`: personalised candidates
/// ahead of trending ones (each tier by score), no album twice and at
/// most two albums per artist.
pub fn score(
    candidates: Vec<Candidate>,
    user_id: &str,
    date_iso: &str,
    user_genres: &HashSet<String>,
    genres_by_artist: &HashMap<String, Vec<String>>,
    genre_weight: f64,
    count: usize,
) -> Vec<Pick> {
    let mut scored: Vec<Pick> = candidates
        .into_iter()
        .map(|candidate| {
            let artist_genres: HashSet<String> = genres_by_artist
                .get(&candidate.artist_mbid.to_lowercase())
                .into_iter()
                .flatten()
                .map(|genre| genre.to_lowercase())
                .collect();
            let mut shared: Vec<&String> = artist_genres.intersection(user_genres).collect();
            shared.sort();
            let overlap = (shared.len() as f64 / 3.0).min(1.0);
            let pop = ((candidate.listen_count.max(0) as f64 + 1.0).log10() / 6.0).min(1.0);
            let raw = 0.5 * candidate.sim + genre_weight * overlap + 0.15 * pop;
            let score =
                (raw + daily_jitter(user_id, &candidate.release_group_mbid, date_iso)).min(1.0);
            let mut reasons = Vec::new();
            if let Some(seed) = &candidate.seed_artist
                && candidate.sim > 0.0
            {
                reasons.push(format!("Because you listen to {seed}"));
            }
            if overlap >= 0.33
                && let Some(top) = shared.first()
            {
                reasons.push(format!("You love {top}"));
            }
            if candidate.from_trending && reasons.is_empty() {
                reasons.push("Trending worldwide".to_owned());
            }
            reasons.truncate(2);
            Pick {
                match_pct: (40.0 + 58.0 * score).round() as i64,
                score,
                reasons,
                candidate,
            }
        })
        .collect();
    scored.sort_by(|left, right| {
        left.candidate
            .from_trending
            .cmp(&right.candidate.from_trending)
            .then(right.score.total_cmp(&left.score))
    });
    let mut picked = Vec::new();
    let mut per_artist: HashMap<String, usize> = HashMap::new();
    let mut seen = HashSet::new();
    for pick in scored {
        let artist = if pick.candidate.artist_mbid.is_empty() {
            pick.candidate.artist_name.to_lowercase()
        } else {
            pick.candidate.artist_mbid.to_lowercase()
        };
        let taken = per_artist.get(&artist).copied().unwrap_or(0);
        if taken >= 2 || !seen.insert(pick.candidate.release_group_mbid.to_lowercase()) {
            continue;
        }
        per_artist.insert(artist, taken + 1);
        picked.push(pick);
        if picked.len() >= count {
            break;
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(mbid: &str, artist: &str, sim: f64, trending: bool) -> Candidate {
        Candidate {
            release_group_mbid: mbid.to_owned(),
            album_name: format!("Album {mbid}"),
            artist_name: artist.to_owned(),
            artist_mbid: artist.to_owned(),
            sim,
            listen_count: 1000,
            seed_artist: (!trending).then(|| "Seed".to_owned()),
            from_trending: trending,
        }
    }

    #[test]
    fn personalised_lead_two_per_artist_and_match_stays_below_100() {
        let picks = score(
            vec![
                candidate("t1", "z", 0.0, true),
                candidate("a1", "x", 1.0, false),
                candidate("a2", "x", 0.9, false),
                candidate("a3", "x", 0.8, false),
                candidate("b1", "y", 0.2, false),
            ],
            "user",
            "2026-10-07",
            &HashSet::new(),
            &HashMap::new(),
            0.35,
            10,
        );
        let ids: Vec<&str> = picks
            .iter()
            .map(|pick| pick.candidate.release_group_mbid.as_str())
            .collect();
        assert_eq!(ids, ["a1", "a2", "b1", "t1"]);
        assert!(picks.iter().all(|pick| (40..=98).contains(&pick.match_pct)));
        assert_eq!(picks[3].reasons, ["Trending worldwide"]);
        assert_eq!(picks[0].reasons, ["Because you listen to Seed"]);
    }
}
