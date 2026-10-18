//! The concerts feed's statements.

droppedneedle_kernel::sql! {
    /// Every distinct followed artist, keyed by lowercase MBID.
    pub(super) FOLLOWED_ARTISTS = "SELECT artist_mbid_lower, MIN(artist_mbid) AS artist_mbid, \
             MIN(artist_name) AS artist_name FROM user_followed_artists \
             GROUP BY artist_mbid_lower ORDER BY artist_mbid_lower";
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
    /// Last check time per checked artist.
    pub(super) CURSOR_AGES = "SELECT artist_mbid_lower, last_checked_at FROM artist_event_check \
             WHERE last_checked_at IS NOT NULL";
    /// Users following an artist.
    pub(super) FOLLOWERS = "SELECT user_id FROM user_followed_artists WHERE artist_mbid_lower = ? \
             ORDER BY user_id";
    /// Cached Ticketmaster resolution.
    pub(super) TM_RESOLUTION = "SELECT attraction_id, match_basis, resolved_at FROM artist_tm_attraction \
             WHERE artist_mbid_lower = ?";
    /// When the Skiddle resolution was cached.
    pub(super) SKIDDLE_RESOLVED_AT = "SELECT resolved_at FROM artist_skiddle_resolution WHERE artist_mbid_lower = ?";
    /// Cached Skiddle artist ids.
    pub(super) SKIDDLE_IDS = "SELECT skiddle_artistid FROM artist_skiddle_ids WHERE artist_mbid_lower = ? \
             ORDER BY skiddle_artistid";
    /// Cache a Ticketmaster resolution.
    pub(super) SAVE_TM_RESOLUTION = "INSERT INTO artist_tm_attraction \
                     (artist_mbid_lower, attraction_id, match_basis, resolved_at) \
                     VALUES (?1, ?2, ?3, ?4) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
                     attraction_id = excluded.attraction_id, \
                     match_basis = excluded.match_basis, resolved_at = excluded.resolved_at";
    /// Forget an artist's Skiddle ids.
    pub(super) DELETE_SKIDDLE_IDS = "DELETE FROM artist_skiddle_ids WHERE artist_mbid_lower = ?1";
    /// Cache one Skiddle id.
    pub(super) INSERT_SKIDDLE_ID = "INSERT OR IGNORE INTO artist_skiddle_ids \
                         (artist_mbid_lower, skiddle_artistid) VALUES (?1, ?2)";
    /// Stamp the Skiddle resolution.
    pub(super) SAVE_SKIDDLE_RESOLUTION = "INSERT INTO artist_skiddle_resolution (artist_mbid_lower, resolved_at) \
                     VALUES (?1, ?2) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
                     resolved_at = excluded.resolved_at";
    /// The (source, listing) keys stored for one artist.
    pub(super) EVENT_KEYS = "SELECT source, source_event_id FROM live_event_feed WHERE artist_mbid_lower = ?";
    /// Delete one feed row.
    pub(super) DELETE_EVENT = "DELETE FROM live_event_feed WHERE source = ?1 AND source_event_id = ?2 \
                         AND artist_mbid_lower = ?3";
    /// Insert or update one feed row, keeping when it was first seen.
    pub(super) UPSERT_EVENT = "INSERT INTO live_event_feed (source, source_event_id, \
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
    pub(super) PRUNE_EVENTS = "DELETE FROM live_event_feed WHERE local_date < ?1";
    /// A user's cities in picker order.
    pub(super) CITIES = "SELECT city_name, country_code, latitude, longitude, radius_km \
             FROM user_event_cities WHERE user_id = ? ORDER BY position";
    /// Forget a user's cities.
    pub(super) DELETE_CITIES = "DELETE FROM user_event_cities WHERE user_id = ?1";
    /// Add one city; a repeated coordinate keeps its first entry.
    pub(super) INSERT_CITY = "INSERT INTO user_event_cities (user_id, city_name, country_code, \
                         latitude, longitude, radius_km, position) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT(user_id, latitude, longitude) DO NOTHING";
    /// When the user last opened the events page.
    pub(super) SEEN_AT = "SELECT seen_at FROM user_event_seen WHERE user_id = ?";
    /// Stamp the user's seen marker.
    pub(super) MARK_SEEN = "INSERT INTO user_event_seen (user_id, seen_at) VALUES (?1, ?2) \
                     ON CONFLICT(user_id) DO UPDATE SET seen_at = excluded.seen_at";
    /// Stamp an artist's sweep cursor.
    pub(super) UPSERT_CURSOR = "INSERT INTO artist_event_check (artist_mbid_lower, last_checked_at, last_status, \
         last_error) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
         last_checked_at = excluded.last_checked_at, last_status = excluded.last_status, \
         last_error = excluded.last_error";
    /// The feed row columns, read by `event_from_row`.
    fragment FEED_COLUMNS = "f.source, f.source_event_id, f.artist_mbid_lower, f.artist_name, \
    f.event_name, f.venue_name, f.city, f.region, f.country_code, f.latitude, f.longitude, \
    f.starts_at, f.local_date, f.status, f.ticket_url, f.match_confidence";
    /// Feed rows for one user's follows, carrying the follow's MBID.
    fragment FOLLOWED_FEED = "SELECT " + FEED_COLUMNS + ", u.artist_mbid AS link_mbid FROM live_event_feed f \
                 JOIN user_followed_artists u ON u.artist_mbid_lower = f.artist_mbid_lower \
                 AND u.user_id = ? WHERE f.local_date >= ?";
    /// The whole feed, with lowercase MBIDs.
    fragment WHOLE_FEED = "SELECT " + FEED_COLUMNS + ", f.artist_mbid_lower AS link_mbid FROM live_event_feed f \
                 WHERE f.local_date >= ?";
    /// Only rows first seen after a time.
    fragment DISCOVERED_AFTER = " AND f.discovered_at > ?";
    /// Oldest first.
    fragment FEED_ORDER = " ORDER BY f.local_date ASC, f.event_name ASC";
    /// Feed rows on or after a day.
    family FEED = feed;
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
