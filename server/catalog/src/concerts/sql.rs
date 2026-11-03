//! The concerts feed's statements.

use droppedneedle_kernel::db::time::Millis;

use super::matching::{Confidence, TmBasis};
use super::models::{ConcertStatus, EventSource};

droppedneedle_kernel::sql! {
    /// Every distinct followed artist, keyed by lowercase MBID.
    pub(super) FOLLOWED_ARTISTS = "SELECT artist_mbid_lower, MIN(artist_mbid) AS artist_mbid, \
             MIN(artist_name) AS artist_name FROM acquire_followed_artists \
             GROUP BY artist_mbid_lower ORDER BY artist_mbid_lower";
    pub(super) row FollowedArtist for [FOLLOWED_ARTISTS] {
        artist_mbid_lower: String,
        artist_mbid: String,
        artist_name: String,
    }
    /// Library artists with a MusicBrainz identity on a live album with indexed tracks.
    pub(super) LIBRARY_ARTISTS = "SELECT lower(e.provider_artist_id) AS mbid_lower, \
             MIN(e.provider_artist_id) AS mbid, MIN(a.display_name) AS name \
             FROM local_artist_external_identities e \
             JOIN local_artists a ON a.id = e.local_artist_id \
             JOIN local_album_artists credit ON credit.local_artist_id = e.local_artist_id \
             JOIN local_albums b ON b.id = credit.local_album_id \
             WHERE e.provider = 'musicbrainz' AND e.provider_artist_id != '' \
             AND a.retired_into_artist_id IS NULL AND b.retired_into_album_id IS NULL \
             AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
             AND t.availability = 'indexed') \
             GROUP BY lower(e.provider_artist_id) ORDER BY 1";
    pub(super) row LibraryArtist for [LIBRARY_ARTISTS] {
        mbid_lower: String,
        mbid: String,
        name: String,
    }
    /// Last check time per checked artist.
    pub(super) CURSOR_AGES = "SELECT artist_mbid_lower, last_checked_at FROM catalog_artist_event_checks \
             WHERE last_checked_at IS NOT NULL";
    pub(super) row CursorAge for [CURSOR_AGES] {
        artist_mbid_lower: String,
        last_checked_at: Millis,
    }
    /// Users following an artist.
    pub(super) FOLLOWERS = "SELECT user_id FROM acquire_followed_artists WHERE artist_mbid_lower = ? \
             ORDER BY user_id";
    /// Cached Ticketmaster resolution.
    pub(super) TM_RESOLUTION = "SELECT attraction_id, match_basis, resolved_at FROM catalog_artist_tm_attractions \
             WHERE artist_mbid_lower = ?";
    pub(super) row TmColumns for [TM_RESOLUTION] {
        attraction_id: Option<String>,
        match_basis: TmBasis,
        resolved_at: Millis,
    }
    /// When the Skiddle resolution was cached.
    pub(super) SKIDDLE_RESOLVED_AT = "SELECT resolved_at FROM catalog_artist_skiddle_resolutions WHERE artist_mbid_lower = ?";
    /// Cached Skiddle artist ids.
    pub(super) SKIDDLE_IDS = "SELECT skiddle_artistid FROM catalog_artist_skiddle_ids WHERE artist_mbid_lower = ? \
             ORDER BY skiddle_artistid";
    /// Cache a Ticketmaster resolution.
    pub(super) SAVE_TM_RESOLUTION = "INSERT INTO catalog_artist_tm_attractions \
                     (artist_mbid_lower, attraction_id, match_basis, resolved_at) \
                     VALUES (?1, ?2, ?3, ?4) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
                     attraction_id = excluded.attraction_id, \
                     match_basis = excluded.match_basis, resolved_at = excluded.resolved_at";
    /// Forget an artist's Skiddle ids.
    pub(super) DELETE_SKIDDLE_IDS = "DELETE FROM catalog_artist_skiddle_ids WHERE artist_mbid_lower = ?1";
    /// Cache one Skiddle id.
    pub(super) INSERT_SKIDDLE_ID = "INSERT OR IGNORE INTO catalog_artist_skiddle_ids \
                         (artist_mbid_lower, skiddle_artistid) VALUES (?1, ?2)";
    /// Stamp the Skiddle resolution.
    pub(super) SAVE_SKIDDLE_RESOLUTION = "INSERT INTO catalog_artist_skiddle_resolutions (artist_mbid_lower, resolved_at) \
                     VALUES (?1, ?2) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
                     resolved_at = excluded.resolved_at";
    /// The (source, listing) keys stored for one artist.
    pub(super) EVENT_KEYS = "SELECT source, source_event_id FROM catalog_live_event_feed WHERE artist_mbid_lower = ?";
    pub(super) row EventKey for [EVENT_KEYS] {
        source: EventSource,
        source_event_id: String,
    }
    /// Delete one feed row.
    pub(super) DELETE_EVENT = "DELETE FROM catalog_live_event_feed WHERE source = ?1 AND source_event_id = ?2 \
                         AND artist_mbid_lower = ?3";
    /// Insert or update one feed row, keeping when it was first seen.
    pub(super) UPSERT_EVENT = "INSERT INTO catalog_live_event_feed (source, source_event_id, \
                         artist_mbid_lower, artist_name, event_name, venue_name, city, region, \
                         country_code, latitude, longitude, starts_at, local_date, status, \
                         ticket_url, match_confidence, discovered_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                         ?15, ?16, ?17, ?17) \
                         ON CONFLICT(source, source_event_id, artist_mbid_lower) DO UPDATE SET \
                         artist_name = excluded.artist_name, event_name = excluded.event_name, \
                         venue_name = excluded.venue_name, city = excluded.city, \
                         region = excluded.region, country_code = excluded.country_code, \
                         latitude = excluded.latitude, longitude = excluded.longitude, \
                         starts_at = excluded.starts_at, local_date = excluded.local_date, \
                         status = excluded.status, ticket_url = excluded.ticket_url, \
                         match_confidence = excluded.match_confidence, \
                         updated_at = excluded.updated_at";
    /// Delete feed rows dated before a day.
    pub(super) PRUNE_EVENTS = "DELETE FROM catalog_live_event_feed WHERE local_date < ?1";
    /// A user's cities in picker order.
    pub(super) CITIES = "SELECT city_name, country_code, latitude, longitude, radius_km \
             FROM catalog_event_cities WHERE user_id = ? ORDER BY position";
    pub(super) row CityColumns for [CITIES] {
        city_name: String,
        country_code: Option<String>,
        latitude: f64,
        longitude: f64,
        radius_km: f64,
    }
    /// Forget a user's cities.
    pub(super) DELETE_CITIES = "DELETE FROM catalog_event_cities WHERE user_id = ?1";
    /// Add one city; a repeated coordinate keeps its first entry.
    pub(super) INSERT_CITY = "INSERT INTO catalog_event_cities (user_id, city_name, country_code, \
                         latitude, longitude, radius_km, position) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT(user_id, latitude, longitude) DO NOTHING";
    /// When the user last opened the events page.
    pub(super) SEEN_AT = "SELECT seen_at FROM catalog_event_seen_marks WHERE user_id = ?";
    /// Stamp the user's seen marker.
    pub(super) MARK_SEEN = "INSERT INTO catalog_event_seen_marks (user_id, seen_at) VALUES (?1, ?2) \
                     ON CONFLICT(user_id) DO UPDATE SET seen_at = excluded.seen_at";
    /// Stamp an artist's sweep cursor.
    pub(super) UPSERT_CURSOR = "INSERT INTO catalog_artist_event_checks (artist_mbid_lower, last_checked_at, last_status, \
         last_error) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
         last_checked_at = excluded.last_checked_at, last_status = excluded.last_status, \
         last_error = excluded.last_error";
    /// The feed row columns.
    fragment FEED_COLUMNS = "f.source, f.source_event_id, f.artist_mbid_lower, f.artist_name, \
    f.event_name, f.venue_name, f.city, f.region, f.country_code, f.latitude, f.longitude, \
    f.starts_at, f.local_date, f.status, f.ticket_url, f.match_confidence";
    /// Feed rows for one user's follows, carrying the follow's MBID.
    fragment FOLLOWED_FEED = "SELECT " + FEED_COLUMNS + ", u.artist_mbid AS link_mbid FROM catalog_live_event_feed f \
                 JOIN acquire_followed_artists u ON u.artist_mbid_lower = f.artist_mbid_lower \
                 AND u.user_id = ? WHERE f.local_date >= ?";
    /// The whole feed, with lowercase MBIDs.
    fragment WHOLE_FEED = "SELECT " + FEED_COLUMNS + ", f.artist_mbid_lower AS link_mbid FROM catalog_live_event_feed f \
                 WHERE f.local_date >= ?";
    /// Only rows first seen after a time.
    fragment DISCOVERED_AFTER = " AND f.discovered_at > ?";
    /// Oldest first.
    fragment FEED_ORDER = " ORDER BY f.local_date ASC, f.event_name ASC";
    /// Feed rows on or after a day.
    family FEED = feed;
    pub(super) row FeedColumns for family [feed] {
        source: EventSource,
        source_event_id: String,
        artist_mbid_lower: String,
        artist_name: String,
        event_name: String,
        venue_name: Option<String>,
        city: Option<String>,
        region: Option<String>,
        country_code: Option<String>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        starts_at: Option<String>,
        local_date: String,
        status: ConcertStatus,
        ticket_url: Option<String>,
        match_confidence: Confidence,
        link_mbid: String,
    }
}

/// Feed rows on or after a day, oldest first: one user's follows or the
/// whole feed, and optionally only rows first seen after a time.
pub(super) fn feed(for_user: bool, discovered_after: bool) -> String {
    let mut sql = String::from(if for_user { FOLLOWED_FEED } else { WHOLE_FEED });
    if discovered_after {
        sql.push_str(DISCOVERED_AFTER);
    }
    sql.push_str(FEED_ORDER);
    sql
}
