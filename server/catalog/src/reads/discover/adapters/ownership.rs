//! Which chart rows the library already holds.
//!
//! Charts and the queue mark rows as owned (v2 `_mark_*_ownership`): an
//! album when an owned library album is identified as its release group,
//! an artist when an owned library artist carries its MBID. The local id
//! comes back too, so the frontend can link straight to the library page.

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::reads::discover::ports::ProviderFailure;
use crate::reads::library::definitions::{musicbrainz, owned_album, owned_artist};

/// Most ids one lookup binds; chart pages are at most 100 rows.
const MAX_IDS: usize = 200;

/// Lowercased release-group MBID to the oldest owned album identified as
/// it.
pub async fn owned_albums(
    pool: &SqlitePool,
    mbids: &[&str],
) -> Result<HashMap<String, String>, ProviderFailure> {
    lookup(
        pool,
        mbids,
        concat!(
            "SELECT ae.release_group_mbid, (SELECT o.local_album_id \
             FROM local_album_external_identities o JOIN local_albums a ON a.id = o.local_album_id \
             WHERE o.release_group_mbid = ae.release_group_mbid AND ",
            musicbrainz!("o"),
            " AND ",
            owned_album!("a"),
            " ORDER BY a.created_at, a.id LIMIT 1) AS local_id \
             FROM local_album_external_identities ae \
             WHERE ae.release_group_mbid IN ({ids}) AND ",
            musicbrainz!("ae"),
            " GROUP BY ae.release_group_mbid HAVING local_id IS NOT NULL"
        ),
    )
    .await
}

/// Lowercased artist MBID to the owned local artist carrying it.
pub async fn owned_artists(
    pool: &SqlitePool,
    mbids: &[&str],
) -> Result<HashMap<String, String>, ProviderFailure> {
    lookup(
        pool,
        mbids,
        concat!(
            "SELECT lower(ai.provider_artist_id), MIN(ai.local_artist_id) \
             FROM local_artist_external_identities ai \
             JOIN local_artists r ON r.id = ai.local_artist_id \
             WHERE lower(ai.provider_artist_id) IN ({ids}) AND ",
            musicbrainz!("ai"),
            " AND ",
            owned_artist!("r"),
            " GROUP BY lower(ai.provider_artist_id)"
        ),
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
