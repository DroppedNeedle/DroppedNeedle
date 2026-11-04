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
//! Rows need their names and counts: an answer without one of those fields
//! fails decoding, and a row where one is null or blank is skipped rather
//! than shown as "Unknown" (v2 filled placeholders in). The wire shapes are
//! in [`models`](super::models).

use serde::de::DeserializeOwned;

use super::models::{
    ArtistStatRow, ArtistStatsAnswer, FeedbackAnswer, FeedbackMetadata, FeedbackRow,
    FreshReleaseRow, FreshReleasesAnswer, GenreActivityAnswer, RecordingStatRow,
    RecordingStatsAnswer, ReleaseGroupStatRow, ReleaseGroupStatsAnswer, SimilarUsersAnswer,
};
use super::{ListenBrainzClient, ListenBrainzCredentials, non_blank};
use crate::{ProviderError, RequestPriority};

/// Ranges the stats endpoints accept (the
/// spellings the charts use).
pub const STATS_RANGES: [&str; 4] = ["this_week", "this_month", "this_year", "all_time"];
/// Largest page the stats endpoints serve (v2 clamps `count` to 100).
pub const MAX_STATS_COUNT: u32 = 100;

/// One ranked artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistStat {
    /// Artist name.
    pub artist_name: String,
    /// Listens in the range.
    pub listen_count: i64,
    /// Artist MBIDs, when ListenBrainz mapped them.
    pub artist_mbids: Vec<String>,
}

/// One ranked release group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseGroupStat {
    /// Release group title.
    pub release_group_name: String,
    /// Credited artist.
    pub artist_name: String,
    /// Listens in the range.
    pub listen_count: i64,
    /// Release group MBID, when mapped.
    pub release_group_mbid: Option<String>,
    /// Artist MBIDs, when mapped.
    pub artist_mbids: Vec<String>,
}

/// One ranked recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingStat {
    /// Track title.
    pub track_name: String,
    /// Credited artist.
    pub artist_name: String,
    /// Listens in the range.
    pub listen_count: i64,
    /// Recording MBID, when mapped.
    pub recording_mbid: Option<String>,
}

/// One fresh release from a user's fresh-releases feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshRelease {
    /// Release group MBID.
    pub release_group_mbid: String,
    /// Release title.
    pub release_name: String,
    /// Credited artist, when sent.
    pub artist_credit_name: Option<String>,
    /// Artist MBIDs, when mapped.
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
        Ok(self
            .stats_answer::<ArtistStatsAnswer>(
                &["1", "stats", "sitewide", "artists"],
                range,
                count,
                offset,
                priority,
            )
            .await?
            .map_or_else(Vec::new, artist_stats))
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
        Ok(self
            .stats_answer::<ReleaseGroupStatsAnswer>(
                &["1", "stats", "sitewide", "release-groups"],
                range,
                count,
                offset,
                priority,
            )
            .await?
            .map_or_else(Vec::new, release_group_stats))
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
        Ok(self
            .stats_answer::<ArtistStatsAnswer>(
                &["1", "stats", "user", username, "artists"],
                range,
                count,
                offset,
                priority,
            )
            .await?
            .map_or_else(Vec::new, artist_stats))
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
        Ok(self
            .stats_answer::<ReleaseGroupStatsAnswer>(
                &["1", "stats", "user", username, "release-groups"],
                range,
                count,
                offset,
                priority,
            )
            .await?
            .map_or_else(Vec::new, release_group_stats))
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
        Ok(self
            .stats_answer::<RecordingStatsAnswer>(
                &["1", "stats", "user", username, "recordings"],
                range,
                count,
                offset,
                priority,
            )
            .await?
            .map_or_else(Vec::new, recording_stats))
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
        let Some(answer) = self
            .public_read::<SimilarUsersAnswer>(
                &["1", "user", username, "similar-users"],
                &[],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(answer
            .payload
            .into_iter()
            .filter_map(|row| row.user_name)
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
        let Some(answer) = self
            .public_read::<GenreActivityAnswer>(
                &["1", "stats", "user", username, "genre-activity"],
                &[],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let mut totals: Vec<GenreActivity> = Vec::new();
        for row in answer.result {
            let Some(genre) = row.genre else {
                continue;
            };
            let count = row.listen_count.unwrap_or(0);
            match totals.iter_mut().find(|entry| entry.genre == genre) {
                Some(entry) => entry.listen_count += count,
                None => totals.push(GenreActivity {
                    genre,
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
        let answer = self
            .public_read::<FeedbackAnswer>(
                &["1", "feedback", "user", username, "get-feedback"],
                &[("score", "1"), ("count", &count_text)],
                priority,
            )
            .await?;
        Ok(answer.map_or(0, |answer| answer.rows().len()))
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
        let Some(answer) = self
            .public_read::<FreshReleasesAnswer>(
                &["1", "user", username, "fresh_releases"],
                &[("past", "true"), ("future", "false")],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(answer
            .payload
            .releases
            .into_iter()
            .filter_map(fresh_release)
            .collect())
    }

    /// The user's loved recordings with their names, newest first (the home
    /// "Your Favorites" shelf). Rows without a track title or an artist name
    /// are skipped.
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
        Ok(rows.into_iter().filter_map(loved_recording).collect())
    }

    /// One page of the user's loved-recording feedback rows, with metadata.
    async fn loved_feedback(
        &self,
        username: &str,
        count: u32,
        priority: RequestPriority,
    ) -> Result<Vec<FeedbackRow>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        let count_text = count.min(MAX_STATS_COUNT).to_string();
        let answer = self
            .public_read::<FeedbackAnswer>(
                &["1", "feedback", "user", username, "get-feedback"],
                &[("score", "1"), ("count", &count_text), ("metadata", "true")],
                priority,
            )
            .await?;
        Ok(answer.map_or_else(Vec::new, FeedbackAnswer::rows))
    }

    /// One stats list. `None` is a 204: not computed yet.
    async fn stats_answer<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        range: &str,
        count: u32,
        offset: u32,
        priority: RequestPriority,
    ) -> Result<Option<T>, ProviderError> {
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
        self.public_read(segments, &params, priority).await
    }

    /// One anonymous read. `None` is a 204.
    async fn public_read<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<T>, ProviderError> {
        let request = self.get(segments, params, &ListenBrainzCredentials::default())?;
        self.read(request, priority).await
    }
}

/// The artists of one stats answer.
fn artist_stats(answer: ArtistStatsAnswer) -> Vec<ArtistStat> {
    let rows = answer.payload.artists.into_iter();
    rows.filter_map(artist_stat).collect()
}

/// The release groups of one stats answer.
fn release_group_stats(answer: ReleaseGroupStatsAnswer) -> Vec<ReleaseGroupStat> {
    let rows = answer.payload.release_groups.into_iter();
    rows.filter_map(release_group_stat).collect()
}

/// The recordings of one stats answer.
fn recording_stats(answer: RecordingStatsAnswer) -> Vec<RecordingStat> {
    let rows = answer.payload.recordings.into_iter();
    rows.filter_map(recording_stat).collect()
}

/// One ranked artist. The sitewide list names one MBID; the list form is read
/// when it has any.
fn artist_stat(row: ArtistStatRow) -> Option<ArtistStat> {
    Some(ArtistStat {
        artist_name: row.artist_name?,
        listen_count: row.listen_count?,
        artist_mbids: match row.artist_mbids {
            Some(mbids) if !mbids.is_empty() => mbids,
            _ => row.artist_mbid.into_iter().collect(),
        },
    })
}

/// One ranked release group.
fn release_group_stat(row: ReleaseGroupStatRow) -> Option<ReleaseGroupStat> {
    Some(ReleaseGroupStat {
        release_group_name: row.release_group_name?,
        artist_name: row.artist_name?,
        listen_count: row.listen_count?,
        release_group_mbid: row.release_group_mbid,
        artist_mbids: row.artist_mbids.unwrap_or_default(),
    })
}

/// One ranked recording.
fn recording_stat(row: RecordingStatRow) -> Option<RecordingStat> {
    Some(RecordingStat {
        track_name: row.track_name?,
        artist_name: row.artist_name?,
        listen_count: row.listen_count?,
        recording_mbid: row.recording_mbid,
    })
}

/// One fresh release.
fn fresh_release(row: FreshReleaseRow) -> Option<FreshRelease> {
    Some(FreshRelease {
        release_group_mbid: row.release_group_mbid?,
        release_name: row.release_name?,
        artist_credit_name: row.artist_credit_name,
        artist_mbids: row.artist_mbids.unwrap_or_default(),
    })
}

/// The metadata object of one feedback row, under any of its spellings.
fn feedback_metadata(row: &FeedbackRow) -> Option<&FeedbackMetadata> {
    row.recording_metadata
        .as_ref()
        .or(row.track_metadata.as_ref())
        .or(row.metadata.as_ref())
}

/// One loved recording with its names. The row's own fields stand in for
/// missing metadata, like v2; a row with no track title or no artist name
/// is skipped.
fn loved_recording(row: FeedbackRow) -> Option<LovedRecording> {
    let artist_mbid = loved_artist_mbid(&row);
    let metadata = feedback_metadata(&row).cloned().unwrap_or_default();
    let mapping = metadata.mbid_mapping.unwrap_or_default();
    Some(LovedRecording {
        track_name: metadata
            .track_name
            .or(metadata.recording_name)
            .or(row.track_name)?,
        artist_name: metadata
            .artist_name
            .or(metadata.artist)
            .or(row.artist_name)?,
        release_name: metadata
            .release_name
            .or(metadata.album_name)
            .or(row.release_name),
        recording_mbid: row
            .recording_mbid
            .or_else(|| non_blank(mapping.recording_mbid))
            .or(metadata.recording_mbid),
        release_mbid: non_blank(mapping.release_mbid)
            .or(row.release_mbid)
            .or(metadata.release_mbid),
        artist_mbid,
    })
}

/// The first artist MBID behind one feedback row: the MBID mapping first,
/// then the metadata's own list, then a lone `artist_mbid`.
fn loved_artist_mbid(row: &FeedbackRow) -> Option<String> {
    let metadata = feedback_metadata(row)?;
    let first = |mbids: Option<&Vec<String>>| mbids.and_then(|mbids| mbids.first()).cloned();
    first(
        metadata
            .mbid_mapping
            .as_ref()
            .and_then(|mapping| mapping.artist_mbids.as_ref()),
    )
    .or_else(|| first(metadata.artist_mbids.as_ref()))
    .or_else(|| metadata.artist_mbid.clone())
    .map(|mbid| mbid.trim().to_ascii_lowercase())
    .filter(|mbid| !mbid.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loved row with no artist name is skipped, not shown as "Unknown".
    #[test]
    fn a_loved_row_without_an_artist_is_skipped() {
        let row = |json: &str| loved_recording(serde_json::from_str(json).expect("row decodes"));
        assert!(row(r#"{"track_metadata": {"track_name": "Opener"}}"#).is_none());
        let named = row(r#"{"track_metadata": {"track_name": "Opener", "artist_name": "Aurora"}}"#)
            .expect("a named row stays");
        assert_eq!(named.artist_name, "Aurora");
    }
}
