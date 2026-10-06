//! Which chart rows the library already holds.
//!
//! Charts and the queue mark rows as owned (v2 `_mark_*_ownership`): an
//! album when some library album is identified as its release group, an
//! artist when some library artist carries its MBID. The local id comes
//! back too, so the frontend can link straight to the library page.

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::reads::discover::ports::ProviderFailure;

/// Most ids one lookup binds; chart pages are at most 100 rows.
const MAX_IDS: usize = 200;

/// Lowercased release-group MBID to the local album id identified as it.
pub async fn owned_albums(
    pool: &SqlitePool,
    mbids: &[&str],
) -> Result<HashMap<String, String>, ProviderFailure> {
    lookup(
        pool,
        mbids,
        "SELECT ae.release_group_mbid, MIN(ae.local_album_id) \
         FROM local_album_external_identities ae \
         JOIN local_albums a ON a.id = ae.local_album_id AND a.retired_into_album_id IS NULL \
         WHERE ae.release_group_mbid IN ({ids}) GROUP BY ae.release_group_mbid",
    )
    .await
}

/// Lowercased artist MBID to the local artist id carrying it.
pub async fn owned_artists(
    pool: &SqlitePool,
    mbids: &[&str],
) -> Result<HashMap<String, String>, ProviderFailure> {
    lookup(
        pool,
        mbids,
        "SELECT lower(ai.provider_artist_id), MIN(ai.local_artist_id) \
         FROM local_artist_external_identities ai \
         JOIN local_artists r ON r.id = ai.local_artist_id AND r.retired_into_artist_id IS NULL \
         WHERE lower(ai.provider_artist_id) IN ({ids}) GROUP BY lower(ai.provider_artist_id)",
    )
    .await
}

async fn lookup(
    pool: &SqlitePool,
    mbids: &[&str],
    template: &str,
) -> Result<HashMap<String, String>, ProviderFailure> {
    let mut wanted: Vec<String> = mbids
        .iter()
        .map(|mbid| mbid.trim().to_ascii_lowercase())
        .filter(|mbid| !mbid.is_empty())
        .collect();
    wanted.sort();
    wanted.dedup();
    wanted.truncate(MAX_IDS);
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = template.replace("{ids}", &vec!["?"; wanted.len()].join(","));
    let mut query = sqlx::query_as::<_, (String, String)>(&sql);
    for mbid in &wanted {
        query = query.bind(mbid);
    }
    let rows = query
        .fetch_all(pool)
        .await
        .map_err(|error| ProviderFailure::failed(format!("library ownership: {error}")))?;
    Ok(rows
        .into_iter()
        .map(|(mbid, local_id)| (mbid.to_ascii_lowercase(), local_id))
        .collect())
}
