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

use super::{ListenBrainzClient, ListenBrainzCredentials};
use crate::{ProviderError, RequestPriority};

/// Ranges the stats endpoints accept (the
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

/// One loved recording with the names to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LovedRecording {
    /// Track title.
    pub track_name: String,
    /// Credited artist name.
    pub artist_name: String,
    /// Release title, when sent.
    pub release_name: Option<String>,
    /// Recording MBID, when sent.
    pub recording_mbid: Option<String>,
    /// Release MBID, when mapped.
    pub release_mbid: Option<String>,
    /// First credited artist MBID, when mapped.
    pub artist_mbid: Option<String>,
}

/// Listens per genre, summed across the activity buckets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreActivity {
    /// Genre label.
    pub genre: String,
    /// Listens in that genre.
    pub listen_count: i64,
}

impl ListenBrainzClient {
    /// Sitewide top artists (`/1/stats/sitewide/artists`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn sitewide_top_artists(
        &self,
        range: &str,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Result<Vec<ArtistStat>, ProviderError> {
        self.stats_rows(
            &["1", "stats", "sitewide", "artists"],
            range,
            count,
            offset,
            "artists",
            priority,
        )
        .await
    }

    /// Sitewide top release groups (`/1/stats/sitewide/release-groups`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn sitewide_top_release_groups(
        &self,
        range: &str,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Result<Vec<ReleaseGroupStat>, ProviderError> {
        self.stats_rows(
            &["1", "stats", "sitewide", "release-groups"],
            range,
            count,
            offset,
            "release_groups",
            priority,
        )
        .await
    }

    /// A user's top artists (`/1/stats/user/{user}/artists`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_top_artists(
        &self,
        username: &str,
        range: &str,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Result<Vec<ArtistStat>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        self.stats_rows(
            &["1", "stats", "user", username, "artists"],
            range,
            count,
            offset,
            "artists",
            priority,
        )
        .await
    }

    /// A user's top release groups (`/1/stats/user/{user}/release-groups`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_top_release_groups(
        &self,
        username: &str,
        range: &str,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Result<Vec<ReleaseGroupStat>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        self.stats_rows(
            &["1", "stats", "user", username, "release-groups"],
            range,
            count,
            offset,
            "release_groups",
            priority,
        )
        .await
    }

    /// A user's top recordings (`/1/stats/user/{user}/recordings`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_top_recordings(
        &self,
        username: &str,
        range: &str,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Result<Vec<RecordingStat>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        self.stats_rows(
            &["1", "stats", "user", username, "recordings"],
            range,
            count,
            offset,
            "recordings",
            priority,
        )
        .await
    }

    /// Users whose listening is most like `username`'s, most similar first
    /// (`/1/user/{user}/similar-users`). Only the user names are kept.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn similar_users(
        &self,
        username: &str,
        priority: RequestPriority,
    ) -> Result<Vec<String>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        let Some(payload) = self
            .public_read(&["1", "user", username, "similar-users"], &[], priority)
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(payload
            .get("payload")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("user_name")?.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// A user's listens per genre, largest first
    /// (`/1/stats/user/{user}/genre-activity`, summed over its hourly
    /// buckets as v2 did).
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_genre_activity(
        &self,
        username: &str,
        priority: RequestPriority,
    ) -> Result<Vec<GenreActivity>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        let Some(payload) = self
            .public_read(
                &["1", "stats", "user", username, "genre-activity"],
                &[],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
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
        Ok(totals)
    }

    /// How many recordings the user loved, sampled from one page of at most
    /// `count` (ListenBrainz caps the page at 100, as v2 noted, so this is
    /// a sample size and not a total).
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_loved_sample(
        &self,
        username: &str,
        count: u32,
        priority: RequestPriority,
    ) -> Result<usize, ProviderError> {
        if username.is_empty() {
            return Ok(0);
        }
        let count_text = count.min(MAX_STATS_COUNT).to_string();
        let Some(payload) = self
            .public_read(
                &["1", "feedback", "user", username, "get-feedback"],
                &[("score", "1"), ("count", &count_text)],
                priority,
            )
            .await?
        else {
            return Ok(0);
        };
        let body = payload.get("payload").unwrap_or(&payload);
        let rows = body
            .get("feedback")
            .or_else(|| body.get("recordings"))
            .and_then(serde_json::Value::as_array)
            .or_else(|| body.as_array());
        Ok(rows.map_or(0, |rows| rows.iter().filter(|row| row.is_object()).count()))
    }

    /// Releases out recently by artists the user listens to
    /// (`/1/user/{user}/fresh_releases?past=true&future=false`, v2
    /// `get_user_fresh_releases`). Rows without a release group id or a
    /// title are skipped.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_fresh_releases(
        &self,
        username: &str,
        priority: RequestPriority,
    ) -> Result<Vec<FreshRelease>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        let Some(payload) = self
            .public_read(
                &["1", "user", username, "fresh_releases"],
                &[("past", "true"), ("future", "false")],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let rows = payload
            .get("payload")
            .and_then(|payload| payload.get("releases"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(rows
            .into_iter()
            .filter_map(|row| serde_json::from_value(row).ok())
            .collect())
    }

    /// The first artist MBID of each recording the user loved, in feedback
    /// order (`/1/feedback/user/{user}/get-feedback?score=1&metadata=true`).
    /// Recordings without a mapped artist
    /// are skipped; repeats stay so the caller decides how to dedupe.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_loved_artist_mbids(
        &self,
        username: &str,
        count: u32,
        priority: RequestPriority,
    ) -> Result<Vec<String>, ProviderError> {
        let rows = self.loved_feedback(username, count, priority).await?;
        Ok(rows.iter().filter_map(loved_artist_mbid).collect())
    }

    /// The user's loved recordings with their names, newest first (the home
    /// "Your Favorites" shelf). Rows without a track title are skipped.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_loved_recordings(
        &self,
        username: &str,
        count: u32,
        priority: RequestPriority,
    ) -> Result<Vec<LovedRecording>, ProviderError> {
        let rows = self.loved_feedback(username, count, priority).await?;
        Ok(rows.iter().filter_map(loved_recording).collect())
    }

    /// One page of the user's loved-recording feedback rows, with metadata.
    async fn loved_feedback(
        &self,
        username: &str,
        count: u32,
        priority: RequestPriority,
    ) -> Result<Vec<serde_json::Value>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        let count_text = count.min(MAX_STATS_COUNT).to_string();
        let Some(payload) = self
            .public_read(
                &["1", "feedback", "user", username, "get-feedback"],
                &[("score", "1"), ("count", &count_text), ("metadata", "true")],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let body = payload.get("payload").unwrap_or(&payload);
        Ok(body
            .get("feedback")
            .or_else(|| body.get("recordings"))
            .and_then(serde_json::Value::as_array)
            .or_else(|| body.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// One stats list: `payload.{key}` decoded row by row, skipping rows
    /// without their required names.
    async fn stats_rows<T: serde::de::DeserializeOwned>(
        &self,
        segments: &[&str],
        range: &str,
        count: u32,
        offset: u32,
        key: &str,
        priority: RequestPriority,
    ) -> Result<Vec<T>, ProviderError> {
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
        let Some(payload) = self.public_read(segments, &params, priority).await? else {
            return Ok(Vec::new());
        };
        let rows = payload
            .get("payload")
            .and_then(|payload| payload.get(key))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(rows
            .into_iter()
            .filter_map(|row| serde_json::from_value(row).ok())
            .collect())
    }

    /// One anonymous read. `None` is an empty answer.
    async fn public_read(
        &self,
        segments: &[&str],
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<serde_json::Value>, ProviderError> {
        let request = self.get(segments, params, &ListenBrainzCredentials::default())?;
        self.read(request, priority).await
    }
}

/// The metadata object of one feedback row, under any of its spellings.
fn feedback_metadata(row: &serde_json::Value) -> Option<&serde_json::Value> {
    ["recording_metadata", "track_metadata", "metadata"]
        .iter()
        .find_map(|key| row.get(*key).filter(|value| value.is_object()))
}

/// One loved recording with its names. The row's own fields stand in for
/// missing metadata, like v2.
fn loved_recording(row: &serde_json::Value) -> Option<LovedRecording> {
    let empty = serde_json::Value::Null;
    let metadata = feedback_metadata(row).unwrap_or(&empty);
    let mapping = metadata.get("mbid_mapping").unwrap_or(&empty);
    let text = |sources: &[(&serde_json::Value, &str)]| {
        sources.iter().find_map(|(value, key)| {
            value
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        })
    };
    let track_name = text(&[
        (metadata, "track_name"),
        (metadata, "recording_name"),
        (row, "track_name"),
    ])?;
    Some(LovedRecording {
        track_name,
        artist_name: text(&[
            (metadata, "artist_name"),
            (metadata, "artist"),
            (row, "artist_name"),
        ])
        .unwrap_or_else(|| "Unknown".to_owned()),
        release_name: text(&[
            (metadata, "release_name"),
            (metadata, "album_name"),
            (row, "release_name"),
        ]),
        recording_mbid: text(&[
            (row, "recording_mbid"),
            (mapping, "recording_mbid"),
            (metadata, "recording_mbid"),
        ]),
        release_mbid: text(&[
            (mapping, "release_mbid"),
            (row, "release_mbid"),
            (metadata, "release_mbid"),
        ]),
        artist_mbid: loved_artist_mbid(row),
    })
}

/// The first artist MBID behind one feedback row: the MBID mapping first,
/// then the metadata's own list, then a lone `artist_mbid`.
fn loved_artist_mbid(row: &serde_json::Value) -> Option<String> {
    let metadata = feedback_metadata(row)?;
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
