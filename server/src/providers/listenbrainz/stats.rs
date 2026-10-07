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

use super::{
    Body, ListenBrainzClient, ListenBrainzCredentials, Outcome, RequestFailure, path_segment,
};
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

/// One fresh release from a user's fresh-releases feed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FreshRelease {
    /// Release group MBID: required identity.
    pub release_group_mbid: String,
    /// Release title: required.
    pub release_name: String,
    /// Credited artist, when sent.
    #[serde(default)]
    pub artist_credit_name: Option<String>,
    /// Artist MBIDs, when mapped.
    #[serde(default)]
    pub artist_mbids: Vec<String>,
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
        let endpoint = format!("/1/stats/user/{}/artists", path_segment(username));
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
        let endpoint = format!("/1/stats/user/{}/release-groups", path_segment(username));
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
        let endpoint = format!("/1/stats/user/{}/recordings", path_segment(username));
        self.stats_rows(&endpoint, range, count, offset, "recordings")
            .await
    }

    /// Users whose listening is most like `username`'s, most similar first
    /// (`/1/user/{user}/similar-users`). Only the user names are kept.
    pub async fn similar_users(&self, username: &str) -> Outcome<Vec<String>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/user/{}/similar-users", path_segment(username));
        let payload = match self.public_get(&endpoint, &[]).await {
            Ok(Some(payload)) => payload,
            Ok(None) => return Outcome::Found(Vec::new()),
            Err(outcome) => return outcome,
        };
        Outcome::Found(
            payload
                .get("payload")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("user_name")?.as_str())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }

    /// A user's listens per genre, largest first
    /// (`/1/stats/user/{user}/genre-activity`, summed over its hourly
    /// buckets as v2 did).
    pub async fn user_genre_activity(&self, username: &str) -> Outcome<Vec<GenreActivity>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/stats/user/{}/genre-activity", path_segment(username));
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
        let endpoint = format!("/1/feedback/user/{}/get-feedback", path_segment(username));
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

    /// Releases out recently by artists the user listens to
    /// (`/1/user/{user}/fresh_releases?past=true&future=false`, v2
    /// `get_user_fresh_releases`). Rows without a release group id or a
    /// title are skipped.
    pub async fn user_fresh_releases(&self, username: &str) -> Outcome<Vec<FreshRelease>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/user/{}/fresh_releases", path_segment(username));
        let payload = match self
            .public_get(&endpoint, &[("past", "true"), ("future", "false")])
            .await
        {
            Ok(Some(payload)) => payload,
            Ok(None) => return Outcome::Found(Vec::new()),
            Err(outcome) => return outcome,
        };
        let rows = payload
            .get("payload")
            .and_then(|payload| payload.get("releases"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Outcome::Found(
            rows.into_iter()
                .filter_map(|row| serde_json::from_value(row).ok())
                .collect(),
        )
    }

    /// The first artist MBID of each recording the user loved, in feedback
    /// order (`/1/feedback/user/{user}/get-feedback?score=1&metadata=true`,
    /// v2 `get_user_loved_recordings`). Recordings without a mapped artist
    /// are skipped; repeats stay so the caller decides how to dedupe.
    pub async fn user_loved_artist_mbids(
        &self,
        username: &str,
        count: u32,
    ) -> Outcome<Vec<String>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/feedback/user/{}/get-feedback", path_segment(username));
        let count_text = count.min(MAX_STATS_COUNT).to_string();
        let payload = match self
            .public_get(
                &endpoint,
                &[("score", "1"), ("count", &count_text), ("metadata", "true")],
            )
            .await
        {
            Ok(Some(payload)) => payload,
            Ok(None) => return Outcome::Found(Vec::new()),
            Err(outcome) => return outcome,
        };
        let body = payload.get("payload").unwrap_or(&payload);
        let rows = body
            .get("feedback")
            .or_else(|| body.get("recordings"))
            .and_then(serde_json::Value::as_array)
            .or_else(|| body.as_array())
            .cloned()
            .unwrap_or_default();
        Outcome::Found(rows.iter().filter_map(loved_artist_mbid).collect())
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
                Err(self.shape_error("ListenBrainz gave an unexpected reply"))
            }
        }
    }
}

/// The first artist MBID behind one feedback row: the MBID mapping first,
/// then the metadata's own list, then a lone `artist_mbid`.
fn loved_artist_mbid(row: &serde_json::Value) -> Option<String> {
    let metadata = ["recording_metadata", "track_metadata", "metadata"]
        .iter()
        .find_map(|key| row.get(*key).filter(|value| value.is_object()))?;
    let first = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_array)
            .and_then(|mbids| mbids.first())
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    first(
        metadata
            .get("mbid_mapping")
            .and_then(|mapping| mapping.get("artist_mbids")),
    )
    .or_else(|| first(metadata.get("artist_mbids")))
    .or_else(|| {
        metadata
            .get("artist_mbid")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
    .map(|mbid| mbid.trim().to_ascii_lowercase())
    .filter(|mbid| !mbid.is_empty())
}
