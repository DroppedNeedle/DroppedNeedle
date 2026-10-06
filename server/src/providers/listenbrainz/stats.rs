//! ListenBrainz statistics: sitewide and per-user top lists, genre
//! activity, and loved recordings.
//!
//! Ports the stats reads of v2's ListenBrainz repository: the home charts
//! (`/1/stats/sitewide/*`), the per-user "your top" lists and wrapped
//! (`/1/stats/user/{user}/*`), and the loved-recordings sample
//! (`/1/feedback/user/{user}/get-feedback`). Stats are public reads, so no
//! token is sent. ListenBrainz answers 204 while it is still computing a
//! user's stats; that reads as an empty list, like v2.
//!
//! Rows need their names: an artist row without `artist_name`, or a
//! release group without its name, is skipped rather than shown as
//! "Unknown" (v2 filled placeholders in).

use serde::Deserialize;

use super::{Body, ListenBrainzClient, ListenBrainzCredentials, Outcome, RequestFailure};
use crate::providers::{DegradationSink, Pacer};

/// Ranges the stats endpoints accept (v2 `ALLOWED_STATS_RANGE`, the
/// spellings the charts use).
pub const STATS_RANGES: [&str; 4] = ["this_week", "this_month", "this_year", "all_time"];
/// Largest page the stats endpoints serve (v2 clamps `count` to 100).
pub const MAX_STATS_COUNT: u32 = 100;

/// One ranked artist.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ArtistStat {
    /// Artist name: required.
    pub artist_name: String,
    /// Listens in the range.
    #[serde(default)]
    pub listen_count: i64,
    /// Artist MBIDs, when ListenBrainz mapped them.
    #[serde(default)]
    pub artist_mbids: Vec<String>,
}

/// One ranked release group.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseGroupStat {
    /// Release group title: required.
    pub release_group_name: String,
    /// Credited artist: required.
    pub artist_name: String,
    /// Listens in the range.
    #[serde(default)]
    pub listen_count: i64,
    /// Release group MBID, when mapped.
    #[serde(default)]
    pub release_group_mbid: Option<String>,
    /// Artist MBIDs, when mapped.
    #[serde(default)]
    pub artist_mbids: Vec<String>,
}

/// One ranked recording.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RecordingStat {
    /// Track title: required.
    pub track_name: String,
    /// Credited artist: required.
    pub artist_name: String,
    /// Listens in the range.
    #[serde(default)]
    pub listen_count: i64,
    /// Recording MBID, when mapped.
    #[serde(default)]
    pub recording_mbid: Option<String>,
}

/// Listens per genre, summed across the activity buckets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreActivity {
    /// Genre label.
    pub genre: String,
    /// Listens in that genre.
    pub listen_count: i64,
}

impl<P: Pacer, S: DegradationSink> ListenBrainzClient<P, S> {
    /// Sitewide top artists (`/1/stats/sitewide/artists`).
    pub async fn sitewide_top_artists(
        &self,
        range: &str,
        count: u32,
        offset: u32,
    ) -> Outcome<Vec<ArtistStat>> {
        self.stats_rows("/1/stats/sitewide/artists", range, count, offset, "artists")
            .await
    }

    /// Sitewide top release groups (`/1/stats/sitewide/release-groups`).
    pub async fn sitewide_top_release_groups(
        &self,
        range: &str,
        count: u32,
        offset: u32,
    ) -> Outcome<Vec<ReleaseGroupStat>> {
        self.stats_rows(
            "/1/stats/sitewide/release-groups",
            range,
            count,
            offset,
            "release_groups",
        )
        .await
    }

    /// A user's top artists (`/1/stats/user/{user}/artists`).
    pub async fn user_top_artists(
        &self,
        username: &str,
        range: &str,
        count: u32,
        offset: u32,
    ) -> Outcome<Vec<ArtistStat>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/stats/user/{username}/artists");
        self.stats_rows(&endpoint, range, count, offset, "artists")
            .await
    }

    /// A user's top release groups (`/1/stats/user/{user}/release-groups`).
    pub async fn user_top_release_groups(
        &self,
        username: &str,
        range: &str,
        count: u32,
        offset: u32,
    ) -> Outcome<Vec<ReleaseGroupStat>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/stats/user/{username}/release-groups");
        self.stats_rows(&endpoint, range, count, offset, "release_groups")
            .await
    }

    /// A user's top recordings (`/1/stats/user/{user}/recordings`).
    pub async fn user_top_recordings(
        &self,
        username: &str,
        range: &str,
        count: u32,
        offset: u32,
    ) -> Outcome<Vec<RecordingStat>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/stats/user/{username}/recordings");
        self.stats_rows(&endpoint, range, count, offset, "recordings")
            .await
    }

    /// A user's listens per genre, largest first
    /// (`/1/stats/user/{user}/genre-activity`, summed over its hourly
    /// buckets as v2 did).
    pub async fn user_genre_activity(&self, username: &str) -> Outcome<Vec<GenreActivity>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/stats/user/{username}/genre-activity");
        let payload = match self.public_get(&endpoint, &[]).await {
            Ok(Some(payload)) => payload,
            Ok(None) => return Outcome::Found(Vec::new()),
            Err(outcome) => return outcome,
        };
        let mut totals: Vec<GenreActivity> = Vec::new();
        let rows = payload
            .get("result")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for row in rows {
            let Some(genre) = row
                .get("genre")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|genre| !genre.is_empty())
            else {
                continue;
            };
            let count = row
                .get("listen_count")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            match totals.iter_mut().find(|entry| entry.genre == genre) {
                Some(entry) => entry.listen_count += count,
                None => totals.push(GenreActivity {
                    genre: genre.to_owned(),
                    listen_count: count,
                }),
            }
        }
        totals.sort_by(|a, b| b.listen_count.cmp(&a.listen_count));
        Outcome::Found(totals)
    }

    /// How many recordings the user loved, sampled from one page of at most
    /// `count` (ListenBrainz caps the page at 100, as v2 noted, so this is
    /// a sample size and not a total).
    pub async fn user_loved_sample(&self, username: &str, count: u32) -> Outcome<usize> {
        if username.is_empty() {
            return Outcome::Found(0);
        }
        let endpoint = format!("/1/feedback/user/{username}/get-feedback");
        let count_text = count.min(MAX_STATS_COUNT).to_string();
        let payload = match self
            .public_get(&endpoint, &[("score", "1"), ("count", &count_text)])
            .await
        {
            Ok(Some(payload)) => payload,
            Ok(None) => return Outcome::Found(0),
            Err(outcome) => return outcome,
        };
        let body = payload.get("payload").unwrap_or(&payload);
        let rows = body
            .get("feedback")
            .or_else(|| body.get("recordings"))
            .and_then(serde_json::Value::as_array)
            .or_else(|| body.as_array());
        Outcome::Found(rows.map_or(0, |rows| rows.iter().filter(|row| row.is_object()).count()))
    }

    /// One stats list: `payload.{key}` decoded row by row, skipping rows
    /// without their required names.
    async fn stats_rows<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        range: &str,
        count: u32,
        offset: u32,
        key: &str,
    ) -> Outcome<Vec<T>> {
        let range = if STATS_RANGES.contains(&range) {
            range
        } else {
            "this_month"
        };
        let count_text = count.clamp(1, MAX_STATS_COUNT).to_string();
        let offset_text = offset.to_string();
        let params = [
            ("count", count_text.as_str()),
            ("offset", offset_text.as_str()),
            ("range", range),
        ];
        let payload = match self.public_get(endpoint, &params).await {
            Ok(Some(payload)) => payload,
            Ok(None) => return Outcome::Found(Vec::new()),
            Err(outcome) => return outcome,
        };
        let rows = payload
            .get("payload")
            .and_then(|payload| payload.get(key))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Outcome::Found(
            rows.into_iter()
                .filter_map(|row| serde_json::from_value(row).ok())
                .collect(),
        )
    }

    /// One anonymous GET. `Ok(None)` is an empty answer (204, or a body
    /// that did not decode, which the classifier already recorded).
    async fn public_get<T>(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
    ) -> Result<Option<serde_json::Value>, Outcome<T>> {
        match self
            .get(
                endpoint,
                params,
                &ListenBrainzCredentials::default(),
                false,
                &[],
            )
            .await
        {
            Ok(Body::Json(payload)) => Ok(Some(payload)),
            Ok(Body::NoContent | Body::InvalidJson) => Ok(None),
            Err(RequestFailure::Outcome(outcome)) => Err(outcome),
            Err(RequestFailure::Accepted(_)) => {
                Err(self.recorded(None, "ListenBrainz gave an unexpected reply"))
            }
        }
    }
}
