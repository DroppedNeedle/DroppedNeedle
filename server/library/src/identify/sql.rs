//! The SQL of the identify module.

use crate::identity::sql::PERSONS;
use crate::identity::sql::REVISABLE;

droppedneedle_kernel::sql! {
    pub(super) MARK_STALE = "UPDATE library_artist_reconciliation_state SET input_revision = '' \
         WHERE local_album_id = ?1 AND input_revision <> ''";
    pub(super) MARK_RELEASE_STALE = "UPDATE library_artist_reconciliation_state SET input_revision = '' \
         WHERE input_revision <> '' AND local_album_id IN (SELECT e.local_album_id \
         FROM local_album_external_identities e JOIN local_albums a ON a.id = e.local_album_id \
         WHERE e.provider = 'musicbrainz' AND a.retired_into_album_id IS NULL \
         AND lower(e.release_mbid) = ?1)";
    pub(super) RELEASES_WAITING = "SELECT lower(trim(e.release_mbid)) AS release FROM library_artist_reconciliation_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         JOIN local_album_external_identities e ON e.local_album_id = a.id \
          AND e.provider = 'musicbrainz' \
         WHERE s.state = 'waiting_for_identity' AND s.reason_code = ?1 \
         AND a.retired_into_album_id IS NULL AND trim(COALESCE(e.release_mbid, '')) <> '' \
         AND NOT EXISTS (SELECT 1 FROM library_management_metadata_snapshots m \
          WHERE m.provider = 'musicbrainz' AND m.entity_kind = 'release' \
          AND m.entity_id = lower(trim(e.release_mbid)) AND m.input_hash = ?2) \
         GROUP BY release ORDER BY min(s.updated_at), release LIMIT ?3";
    pub(super) RELEASE_SETTLED = "UPDATE library_artist_reconciliation_state SET reason_code = ?2, updated_at = ?3 \
         WHERE state = 'waiting_for_identity' AND reason_code = ?1 \
         AND local_album_id IN (SELECT local_album_id FROM local_album_external_identities \
         WHERE provider = 'musicbrainz' AND lower(release_mbid) = ?4) \
         RETURNING local_album_id";
    pub(super) PROVE = "SELECT t.id, i.release_track_mbid, i.row_revision FROM local_tracks t \
             JOIN local_track_external_identities i \
               ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
             WHERE t.local_album_id = ?1 AND +t.availability = 'indexed' \
             AND lower(trim(COALESCE(i.release_mbid, ''))) = ?2 \
             AND trim(COALESCE(i.release_track_mbid, '')) <> '' ORDER BY t.id";
    pub(super) STORED_PROOFS = "SELECT subject_kind, subject_id, credit_position, evidence_hash, \
             source_local_artist_id, local_artist_id, artist_mbid \
             FROM library_artist_credit_proofs WHERE local_album_id = ?1";
    pub(super) DROP_PROOF = "DELETE FROM library_artist_credit_proofs \
                 WHERE subject_kind = ?1 AND subject_id = ?2 AND credit_position = ?3";
    pub(super) WRITE_PROOF = "INSERT INTO library_artist_credit_proofs (subject_kind, subject_id, \
             local_album_id, local_track_id, credit_position, source_local_artist_id, \
             local_artist_id, artist_mbid, canonical_name, credited_name, sort_name, \
             join_phrase, release_mbid, release_track_mbid, album_identity_revision, \
             track_identity_revision, evidence_hash, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
             ?16, ?17, ?17) \
             ON CONFLICT (subject_kind, subject_id, credit_position) DO UPDATE SET \
             local_album_id = excluded.local_album_id, local_track_id = excluded.local_track_id, \
             source_local_artist_id = excluded.source_local_artist_id, \
             local_artist_id = excluded.local_artist_id, artist_mbid = excluded.artist_mbid, \
             canonical_name = excluded.canonical_name, credited_name = excluded.credited_name, \
             sort_name = excluded.sort_name, join_phrase = excluded.join_phrase, \
             release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             album_identity_revision = excluded.album_identity_revision, \
             track_identity_revision = excluded.track_identity_revision, \
             evidence_hash = excluded.evidence_hash, updated_at = excluded.updated_at, \
             row_revision = library_artist_credit_proofs.row_revision + 1 \
             WHERE library_artist_credit_proofs.evidence_hash IS NOT excluded.evidence_hash";
    pub(super) CLEAR_RECONCILIATION_STATE = "DELETE FROM library_artist_reconciliation_state WHERE local_album_id = ?1";
    pub(super) WRITE_RECONCILIATION_STATE = "INSERT INTO library_artist_reconciliation_state (local_album_id, input_revision, state, \
         reason_code, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (local_album_id) DO UPDATE SET input_revision = excluded.input_revision, \
         state = excluded.state, reason_code = excluded.reason_code, \
         updated_at = excluded.updated_at \
         WHERE library_artist_reconciliation_state.input_revision IS NOT excluded.input_revision \
         OR library_artist_reconciliation_state.state IS NOT excluded.state \
         OR library_artist_reconciliation_state.reason_code IS NOT excluded.reason_code";
    pub(super) ALBUM_IS_LIVE = "SELECT retired_into_album_id IS NULL FROM local_albums WHERE id = ?1";
    pub(super) ALBUM_RELEASE_AND_REVISION = "SELECT release_mbid, row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = 'musicbrainz'";
    pub(super) ARTISTS_CREDITED_ON_ALBUM = "SELECT local_artist_id FROM local_album_artists WHERE local_album_id = ?1 \
             UNION SELECT c.local_artist_id FROM local_track_artists c \
             JOIN local_tracks t ON t.id = c.local_track_id WHERE t.local_album_id = ?1";
    pub(super) ARTIST_CREATED_AND_ELIGIBLE = "SELECT created_at, retired_into_artist_id IS NULL \
                 AND kind <> 'various_artists' AND id NOT IN (?2, ?3) \
                 FROM local_artists WHERE id = ?1";
    pub(super) ARTIST_LINK_AND_SOURCE = "SELECT lower(provider_artist_id), decision_source \
                 FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1 AND provider = 'musicbrainz'";
    pub(super) ARTISTS_LINKED_TO_MBID = "SELECT local_artist_id FROM local_artist_external_identities \
                 WHERE provider = 'musicbrainz' AND lower(provider_artist_id) = ?1";
    pub(super) TOUCH_ARTIST = "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) ARTIST_REVISION = "SELECT row_revision FROM local_artists WHERE id = ?1";
    pub(super) ARTIST_LINK_WITH_SELECTOR = "SELECT lower(i.provider_artist_id), i.decision_source, u.id, i.selected_at \
             FROM local_artist_external_identities i \
             LEFT JOIN auth_users u ON u.id = i.selected_by_user_id \
             WHERE i.local_artist_id = ?1 AND i.provider = 'musicbrainz'";
    pub(super) PERSON_TARGET = "SELECT retired_into_artist_id IS NULL AND kind <> 'various_artists' \
             AND id NOT IN (?2, ?3) FROM local_artists WHERE id = ?1";
    pub(super) FOLLOWS_OUT = "SELECT DISTINCT a.id, a.display_name FROM local_artist_merge_candidates m \
         JOIN local_artists a ON a.id = m.right_artist_id \
         WHERE m.left_artist_id = ?1 AND m.state = 'open' AND m.reason_code IN (?2, ?3, ?4) \
         AND a.retired_into_artist_id IS NULL ORDER BY a.display_name, a.id LIMIT 10";
    pub(super) FOLLOWS_IN = "SELECT a.id, a.display_name FROM local_artist_merge_candidates m \
         JOIN local_artists a ON a.id = m.left_artist_id \
         WHERE m.right_artist_id = ?1 AND m.state = 'open' AND m.reason_code = ?2 \
         AND (?3 IS NULL OR m.id = 'credits-moved:' || m.left_artist_id || ':' || ?3) \
         AND a.retired_into_artist_id IS NULL ORDER BY a.display_name, a.id LIMIT 10";
    pub(super) SET_BY_PERSON = "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
         provider_artist_id, decision_source, selected_by_user_id, selected_at) \
         VALUES (?1, 'musicbrainz', ?2, 'manual', (SELECT id FROM auth_users WHERE id = ?3), ?4) \
         ON CONFLICT (local_artist_id, provider) DO UPDATE SET \
         provider_artist_id = excluded.provider_artist_id, decision_source = 'manual', \
         attempt_id = NULL, selected_by_user_id = excluded.selected_by_user_id, \
         selected_at = excluded.selected_at, \
         row_revision = local_artist_external_identities.row_revision + 1";
    pub(super) CLEAR_BY_PERSON = "DELETE FROM local_artist_external_identities \
         WHERE local_artist_id = ?1 AND provider = 'musicbrainz'";
    pub(super) MERGED_INTO = "SELECT canonical_mbid FROM library_identify_artist_id_checks \
         WHERE artist_mbid = ?1 AND state = ?2";
    pub(super) NOMINATE = "INSERT INTO library_identify_artist_id_checks (artist_mbid, state, nominated_at) \
             VALUES (?1, ?4, ?2) ON CONFLICT (artist_mbid) DO UPDATE SET \
             state = excluded.state, nominated_at = excluded.nominated_at, checked_at = NULL \
             WHERE library_identify_artist_id_checks.state = ?5 \
             AND library_identify_artist_id_checks.checked_at < ?3";
    pub(super) PENDING = "SELECT artist_mbid FROM library_identify_artist_id_checks \
         WHERE state = ?2 ORDER BY nominated_at DESC LIMIT ?1";
    pub(super) KEEP = "UPDATE library_identify_artist_id_checks SET state = ?3, canonical_mbid = NULL, \
         checked_at = ?2 WHERE artist_mbid = ?1";
    pub(super) ARTIST_ID_CHECK_IN_STATE = "SELECT EXISTS (SELECT 1 FROM library_identify_artist_id_checks \
             WHERE artist_mbid = ?1 AND state = ?2)";
    pub(super) NOT_FOLLOWED = "SELECT EXISTS (SELECT 1 FROM library_identify_artist_id_checks \
             WHERE artist_mbid = ?1) OR EXISTS (SELECT 1 FROM library_identify_artist_id_checks \
             WHERE canonical_mbid = ?1)";
    pub(super) SUGGEST_MERGE = "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
         right_artist_id, reason_code, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'SHARED_PROVIDER_IDENTITY', ?4, ?4)";
    pub(super) SETTLE_ARTIST_ID_CHECK = "UPDATE library_identify_artist_id_checks SET state = ?4, canonical_mbid = ?2, \
         checked_at = ?3 WHERE artist_mbid = ?1";
    pub(super) REPOINT_ARTIST_ID_CHECKS = "UPDATE library_identify_artist_id_checks SET canonical_mbid = ?2 \
         WHERE canonical_mbid = ?1";
    pub(super) ALBUMS_WITH_PROOF_OF_MBID = "SELECT DISTINCT local_album_id FROM library_artist_credit_proofs \
             WHERE artist_mbid = ?1";
    pub(super) REPOINT_PROOFS_TO_MBID = "UPDATE library_artist_credit_proofs SET artist_mbid = ?2 WHERE artist_mbid = ?1";
    pub(super) REPOINT_CONVERGED_MBID = "UPDATE local_artists SET converged_mbid = ?2 WHERE converged_mbid = ?1";
    pub(super) ARTISTS_CREDITED_WITH_MBID = "SELECT local_artist_id FROM local_track_artists WHERE artist_mbid = ?2 \
             UNION SELECT local_artist_id FROM local_album_artists WHERE artist_mbid = ?2 \
             UNION SELECT source_local_artist_id FROM library_artist_credit_proofs \
              WHERE artist_mbid = ?2 AND source_local_artist_id IS NOT NULL \
             UNION SELECT local_artist_id FROM local_artist_external_identities \
              WHERE provider = 'musicbrainz' AND lower(provider_artist_id) = ?1";
    pub(super) FOLLOW_PERSON_LINKS = "UPDATE local_artist_external_identities SET provider_artist_id = ?2, \
             row_revision = row_revision + 1 WHERE local_artist_id = ?1 AND provider = 'musicbrainz'";
    pub(super) ALBUM_INPUT_REVISION = "SELECT id, stat_revision FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY id";
    pub(super) OLDEST_WAITING_JOB = "SELECT id FROM library_identify_jobs \
             WHERE local_album_id = ?1 AND state IN ('queued','deferred') \
             ORDER BY created_ms LIMIT 1";
    pub(super) REVISE_WAITING_JOB = "UPDATE OR IGNORE library_identify_jobs SET input_revision = ?1, \
             idempotency_key = ?2, updated_ms = ?3 WHERE id = ?4";
    pub(super) INPUT_ALREADY_HANDLED = "SELECT EXISTS(SELECT 1 FROM library_identify_jobs \
             WHERE idempotency_key = ?1 AND state IN ('running','succeeded'))";
    pub(super) ADD_OFFERED_JOB = "INSERT INTO library_identify_jobs (id, local_album_id, kind, priority, state, \
         attempts, not_before_ms, input_revision, idempotency_key, created_ms, updated_ms) \
         VALUES (?1, ?2, 'automatic', ?3, 'queued', 0, ?4, ?5, ?6, ?4, ?4) \
         ON CONFLICT DO NOTHING";
    pub(super) WAITING_JOB_FOR_RETRY = "SELECT id FROM library_identify_jobs \
             WHERE local_album_id = ?1 AND state IN ('queued','deferred') \
             ORDER BY input_revision <> ?2, created_ms LIMIT 1";
    pub(super) BRING_RETRY_FORWARD = "UPDATE OR IGNORE library_identify_jobs SET input_revision = ?1, \
             idempotency_key = ?2, not_before_ms = MIN(not_before_ms, ?3), \
             priority = MIN(priority, ?5), updated_ms = ?3 WHERE id = ?4";
    pub(super) BUMP_IDENTIFICATION_STREAM = "INSERT INTO library_event_stream_revisions (stream_kind, value) \
         VALUES ('identification', 1) \
         ON CONFLICT (stream_kind) DO UPDATE SET value = value + 1";
    pub(super) SAVE_TRACK_IDENTITY = "SELECT local_album_id FROM local_tracks WHERE id = ?1";
    pub(super) EDITION_CHANGE_UNDONE = "SELECT EXISTS (SELECT 1 FROM library_catalog_actions \
                 WHERE (local_album_id = ?1 \
                     OR local_album_id IN (SELECT alias FROM local_album_aliases \
                         WHERE local_album_id = ?1 AND kind = 'merged_album') \
                     OR local_album_id IN (SELECT id FROM local_albums \
                         WHERE retired_into_album_id = ?1)) \
                 AND action_kind = 'undo_automatic_edition' \
                 AND json_extract(after_json, '$.outcome') = 'restored' \
                 AND lower(json_extract(before_json, '$.identity.release_group_mbid')) \
                     = lower(?2))";
    pub(super) MATCH_FLAG = "SELECT state, reason_code, release_mbid, candidates_json \
                     FROM library_album_match_state WHERE local_album_id = ?1";
    pub(super) ALBUM_FACTS = "SELECT title, COALESCE(album_artist_name, ''), is_compilation, year \
                     FROM local_albums WHERE id = ?1";
    pub(super) ALBUM_FACT_TRACKS = "SELECT t.id, t.title, COALESCE(t.artist_name, ''), t.track_number, \
                 t.disc_number, t.duration_seconds, t.embedded_recording_mbid, \
                 t.embedded_release_track_mbid, t.embedded_release_mbid, \
                 t.embedded_release_group_mbid, t.membership_locked, t.root_id, \
                 t.relative_path, t.stat_revision, h.media, h.barcode, h.catalog_number, \
                 h.release_country, h.total_discs, t.duration_measured \
                 FROM local_tracks t LEFT JOIN local_track_edition_tags h \
                 ON h.local_track_id = t.id \
                 WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
                 ORDER BY t.disc_number, t.track_number, t.relative_path";
    pub(super) FINGERPRINT = "SELECT fingerprint, duration_seconds FROM audio_fingerprint_outcomes \
                     WHERE local_track_id = ?1 AND stat_revision = ?2 \
                     AND fingerprinter_version = ?3 AND fingerprint IS NOT NULL";
    pub(super) DROP_STALE_FINGERPRINT = "DELETE FROM audio_fingerprint_outcomes WHERE local_track_id = ?1 \
                         AND (stat_revision != ?2 OR fingerprinter_version != ?3)";
    pub(super) WRITE_FINGERPRINT = "INSERT INTO audio_fingerprint_outcomes (id, local_track_id, \
                         stat_revision, fingerprinter_version, state, fingerprint, \
                         duration_seconds, first_attempt_at, last_attempt_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
                         ON CONFLICT (local_track_id, stat_revision, fingerprinter_version) \
                         DO UPDATE SET state = excluded.state, \
                         fingerprint = excluded.fingerprint, \
                         duration_seconds = excluded.duration_seconds, \
                         last_attempt_at = excluded.last_attempt_at, \
                         attempt_count = attempt_count + 1";
    pub(super) DROP_RELEASE_DOCUMENT = "DELETE FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND entity_id = ?3";
    pub(super) WRITE_RELEASE_DOCUMENT = "INSERT INTO library_management_metadata_snapshots (id, provider, \
             entity_kind, entity_id, input_hash, canonical_payload_json, payload_sha256, \
             fetched_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    pub(super) PRUNE_EXPIRED_DOCUMENTS = "DELETE FROM library_management_metadata_snapshots \
         WHERE provider = ?1 AND entity_kind = ?2 AND expires_at < ?3 \
         AND payload_sha256 NOT IN (SELECT kept.payload_sha256 \
         FROM library_management_metadata_snapshots AS kept \
         WHERE kept.provider = ?1 AND kept.entity_kind = ?2 AND kept.entity_id IN ( \
         SELECT lower(release_mbid) FROM local_album_external_identities \
         WHERE release_mbid IS NOT NULL \
         UNION SELECT lower(json_extract(candidate.value, '$.release_mbid')) \
         FROM library_identify_reviews, json_each(candidates_json) AS candidate \
         WHERE state = ?4 AND json_extract(candidate.value, '$.release_mbid') IS NOT NULL))";
    pub(super) STORED_DOCUMENT = "SELECT canonical_payload_json, payload_sha256, fetched_at, expires_at \
             FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
             AND input_hash = ?4 ORDER BY fetched_at DESC LIMIT 1";
    pub(super) DOCUMENTS_WITH_PAYLOAD = "SELECT entity_id, input_hash, fetched_at, expires_at \
             FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND payload_sha256 = ?3";
    pub(super) DROP_DOCUMENTS_WITH_PAYLOAD = "DELETE FROM library_management_metadata_snapshots \
         WHERE provider = ?1 AND entity_kind = ?2 AND payload_sha256 = ?3";
    pub(super) COPY_FAVORITES_TO_ITEM = "INSERT OR IGNORE INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         SELECT user_id, item_kind, ?1, created_at FROM library_user_favorites \
         WHERE item_kind = ?2 AND item_id = ?3";
    pub(super) COPY_FAVORITE_NAMES_TO_ITEM = "INSERT OR IGNORE INTO library_user_favorite_names (user_id, item_kind, item_id, \
         display_name) SELECT user_id, item_kind, ?1, display_name \
         FROM library_user_favorite_names WHERE item_kind = ?2 AND item_id = ?3";
    pub(super) DROP_ITEM_FAVORITES = "DELETE FROM library_user_favorites WHERE item_kind = ?1 AND item_id = ?2";
    pub(super) COPY_RATINGS_TO_ITEM = "INSERT OR IGNORE INTO library_user_ratings (user_id, item_kind, item_id, rating, \
         rated_at) SELECT user_id, item_kind, ?1, rating, rated_at FROM library_user_ratings \
         WHERE item_kind = ?2 AND item_id = ?3";
    pub(super) DROP_ITEM_RATINGS = "DELETE FROM library_user_ratings WHERE item_kind = ?1 AND item_id = ?2";
    pub(super) CLAIM = "UPDATE library_identify_jobs SET state = 'running', \
                         lease_expires_ms = ?1, updated_ms = ?2 WHERE id = ?3";
    pub(super) RECOVER = "UPDATE library_identify_jobs SET state = 'queued', \
                         lease_expires_ms = NULL, updated_ms = ?1 WHERE state = 'running'";
    pub(super) FILE_REVIEW = "INSERT OR REPLACE INTO library_identify_reviews (id, local_album_id, \
                         reason_code, candidates_json, state, resolved_by_user_id, \
                         selected_candidate_key, created_ms, updated_ms) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)";
    pub(super) SET_STATE = "UPDATE library_identify_reviews SET state = ?1, \
                         resolved_by_user_id = ?2, selected_candidate_key = ?3, updated_ms = ?4 \
                         WHERE id = ?5";
    pub(super) APPROVE_TX = "UPDATE library_identify_reviews SET state = 'approved', \
         resolved_by_user_id = ?1, selected_candidate_key = ?2, updated_ms = ?3 \
         WHERE id = ?4 AND state = 'pending' AND (?5 IS NULL OR row_revision = ?5)";
    /// SQL that is true while the proof row `p` still counts: its album is
    /// live and its match accepted and unchanged, the credit it proves still
    /// points at its row, and for a track, the track is indexed on the album
    /// and still mapped to the same release track.
    pub(crate) fragment CURRENT_PROOF = "EXISTS (SELECT 1 FROM local_albums proof_album \
     JOIN local_album_external_identities proof_identity \
       ON proof_identity.local_album_id = proof_album.id \
      AND proof_identity.provider = 'musicbrainz' \
     WHERE proof_album.id = p.local_album_id AND proof_album.retired_into_album_id IS NULL \
     AND proof_identity.row_revision = p.album_identity_revision \
     AND lower(trim(COALESCE(proof_identity.release_mbid, ''))) = lower(trim(p.release_mbid)) \
     AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
       WHERE s.local_album_id = proof_album.id) \
     AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
       WHERE c.local_album_id = proof_album.id)) \
     AND CASE p.subject_kind \
     WHEN 'album' THEN EXISTS (SELECT 1 FROM local_album_artists c \
       WHERE c.local_album_id = p.local_album_id AND c.position = p.credit_position \
       AND c.local_artist_id = p.source_local_artist_id) \
     ELSE EXISTS (SELECT 1 FROM local_tracks t \
       JOIN local_track_external_identities ti \
         ON ti.local_track_id = t.id AND ti.provider = 'musicbrainz' \
       JOIN local_track_artists c ON c.local_track_id = t.id AND c.position = p.credit_position \
       WHERE t.id = p.local_track_id AND t.local_album_id = p.local_album_id \
       AND t.availability = 'indexed' AND c.local_artist_id = p.source_local_artist_id \
       AND ti.row_revision = p.track_identity_revision \
       AND lower(trim(COALESCE(ti.release_mbid, ''))) = lower(trim(p.release_mbid)) \
       AND lower(trim(COALESCE(ti.release_track_mbid, ''))) = lower(trim(p.release_track_mbid))) \
     END";
    /// SQL for the stamp of album `a`: the version of the rules below, its
    /// match (release, who made it, its revision and when), whether the match
    /// is accepted, and the count, revisions and latest time of its tracks'
    /// mappings. Any change to them moves it. A change to the credits, which
    /// it leaves out, clears it instead ([`mark_stale`]).
    ///
    /// Any change to what a pass proves bumps the leading version, so every
    /// album is worked through again once.
    pub(super) fragment STAMP = "'rules-2|' || COALESCE((SELECT lower(trim(COALESCE(e.release_mbid, ''))) || '|' \
     || e.decision_source || '|' || e.row_revision || '|' || e.selected_at || '|' \
     || (EXISTS (SELECT 1 FROM library_album_match_state s WHERE s.local_album_id = a.id) \
      OR EXISTS (SELECT 1 FROM library_custom_edition_active c WHERE c.local_album_id = a.id)) \
     FROM local_album_external_identities e \
     WHERE e.local_album_id = a.id AND e.provider = 'musicbrainz'), '-') \
     || '|' || (SELECT count(*) || '|' || COALESCE(sum(i.row_revision), 0) || '|' \
     || COALESCE(max(i.selected_at), 0) FROM local_tracks t \
     JOIN local_track_external_identities i ON i.local_track_id = t.id \
      AND i.provider = 'musicbrainz' WHERE t.local_album_id = a.id)";
    /// The albums whose stamp moved since their last pass (or that have none),
    /// the albums with a state but no match any more, and the albums waiting
    /// for a release document that is now on file (`DOCUMENT_HASH`, the
    /// current document version's hash), at most `limit`.
    pub(super) STALE_ALBUMS = "SELECT a.id FROM local_album_external_identities e \
     JOIN local_albums a ON a.id = e.local_album_id \
     LEFT JOIN library_artist_reconciliation_state s ON s.local_album_id = a.id \
     WHERE e.provider = 'musicbrainz' AND a.retired_into_album_id IS NULL \
     AND s.input_revision IS NOT (" + STAMP + ") \
     UNION ALL SELECT s.local_album_id FROM library_artist_reconciliation_state s \
     WHERE NOT EXISTS (SELECT 1 FROM local_albums a \
      JOIN local_album_external_identities e ON e.local_album_id = a.id \
       AND e.provider = 'musicbrainz' \
      WHERE a.id = s.local_album_id AND a.retired_into_album_id IS NULL) \
     UNION ALL SELECT s.local_album_id FROM library_artist_reconciliation_state s \
     JOIN local_album_external_identities e ON e.local_album_id = s.local_album_id \
      AND e.provider = 'musicbrainz' \
     WHERE s.state = 'waiting_for_identity' AND s.reason_code = 'RELEASE_DOCUMENT_MISSING' \
     AND EXISTS (SELECT 1 FROM library_management_metadata_snapshots m \
      WHERE m.provider = 'musicbrainz' AND m.entity_kind = 'release' \
      AND m.entity_id = lower(trim(e.release_mbid)) AND m.input_hash = ?2) \
     LIMIT ?1";
    /// The ids row `?1` is proven to be: up to two of its current proofs'
    /// ids, and the lowest and highest ids its credits' files hold.
    pub(super) ROW_IDS = "SELECT DISTINCT mbid FROM ( \
     SELECT (SELECT min(artist_mbid) FROM local_track_artists \
      WHERE local_artist_id = ?1 AND artist_mbid IS NOT NULL) AS mbid \
     UNION ALL SELECT (SELECT max(artist_mbid) FROM local_track_artists \
      WHERE local_artist_id = ?1 AND artist_mbid IS NOT NULL) \
     UNION ALL SELECT (SELECT min(artist_mbid) FROM local_album_artists \
      WHERE local_artist_id = ?1 AND artist_mbid IS NOT NULL) \
     UNION ALL SELECT (SELECT max(artist_mbid) FROM local_album_artists \
      WHERE local_artist_id = ?1 AND artist_mbid IS NOT NULL) \
     UNION ALL SELECT mbid FROM (SELECT DISTINCT p.artist_mbid AS mbid \
      FROM library_artist_credit_proofs p WHERE p.source_local_artist_id = ?1 \
      AND (" + CURRENT_PROOF + ") LIMIT 2)) WHERE mbid IS NOT NULL";
    /// Whether row `?1` is credited on a live album waiting for the document
    /// of its accepted release (reasons `?2`, `?3` and `?4`).
    pub(super) WAITS_FOR_DOCUMENT = "SELECT EXISTS (SELECT 1 FROM local_album_artists c \
     JOIN library_artist_reconciliation_state s ON s.local_album_id = c.local_album_id \
     JOIN local_albums a ON a.id = c.local_album_id \
     WHERE c.local_artist_id = ?1 AND a.retired_into_album_id IS NULL \
     AND s.state = 'waiting_for_identity' AND s.reason_code IN (?2, ?3, ?4)) \
     OR EXISTS (SELECT 1 FROM local_track_artists c \
     JOIN local_tracks t ON t.id = c.local_track_id \
     JOIN library_artist_reconciliation_state s ON s.local_album_id = t.local_album_id \
     JOIN local_albums a ON a.id = t.local_album_id \
     WHERE c.local_artist_id = ?1 AND +t.availability = 'indexed' \
     AND a.retired_into_album_id IS NULL \
     AND s.state = 'waiting_for_identity' AND s.reason_code IN (?2, ?3, ?4))";
    /// Store one identify job. A live job on the same album and input
    /// revision (the partial unique index on its key) keeps its place.
    pub(crate) INSERT_JOB = "INSERT INTO library_identify_jobs (id, local_album_id, kind, \
     priority, state, attempts, not_before_ms, input_revision, idempotency_key, \
     requested_by_user_id, failure_code, created_ms, updated_ms) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12) ON CONFLICT DO NOTHING";
    pub(super) fragment JOB_COLUMNS = "id, local_album_id, kind, priority, state, attempts, not_before_ms, \
     input_revision, requested_by_user_id, failure_code, lease_expires_ms";
    /// Land a claimed job, only while the claim that ran it still holds it:
    /// a lease another worker took over after this one lapsed lands nothing.
    pub(super) LAND_JOB = "UPDATE library_identify_jobs SET state = ?1, attempts = ?2, \
     not_before_ms = ?3, input_revision = ?4, idempotency_key = ?5, failure_code = ?6, \
     priority = ?7, updated_ms = ?8, lease_expires_ms = NULL \
     WHERE id = ?9 AND state = 'running' AND lease_expires_ms = ?10";
    pub(super) fragment REVIEW_COLUMNS = "id, local_album_id, reason_code, candidates_json, state, \
     resolved_by_user_id, selected_candidate_key";
    pub(super) SETTLED_REASON = "SELECT s.reason_code FROM library_artist_reconciliation_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         WHERE s.local_album_id = ?1 AND s.reason_code IN (?2, ?3) \
         AND s.input_revision = (" + STAMP + ")";
    pub(super) STORE_STATE = "SELECT " + STAMP + " FROM local_albums a WHERE a.id = ?1";
    pub(super) ENQUEUE = "SELECT " + JOB_COLUMNS + " FROM library_identify_jobs \
                             WHERE idempotency_key = ?1 AND state IN ('queued','running','deferred')";
    pub(super) NEXT_CLAIMABLE_JOB = "SELECT " + JOB_COLUMNS + " FROM library_identify_jobs \
                                 WHERE ((state IN ('queued','deferred') AND not_before_ms <= ?1) \
                                 OR (state = 'running' AND lease_expires_ms <= ?1)) \
                                 AND NOT EXISTS (SELECT 1 FROM library_work_control \
                                     WHERE queue_kind = 'identification' AND state = 'paused') \
                                 ORDER BY priority, created_ms, rowid LIMIT 1";
    pub(super) JOB = "SELECT " + JOB_COLUMNS + " FROM library_identify_jobs WHERE id = ?1";
    pub(super) JOBS_FOR_ALBUM = "SELECT " + JOB_COLUMNS + " FROM library_identify_jobs \
                 WHERE local_album_id = ?1 ORDER BY created_ms, rowid";
    pub(super) GET = "SELECT " + REVIEW_COLUMNS + " FROM library_identify_reviews WHERE id = ?1";
    pub(super) PENDING_FOR_ALBUM = "SELECT " + REVIEW_COLUMNS + " FROM library_identify_reviews \
                 WHERE local_album_id = ?1 AND state = 'pending' ORDER BY created_ms, id";
    /// The rows the app linked, in id order after `?1`, at most `?2`. Every
    /// link is MusicBrainz's (a CHECK), so the walk goes by the primary key.
    pub(super) AUTOMATIC_LINKS_FROM = "SELECT local_artist_id FROM local_artist_external_identities \
         WHERE decision_source IN " + REVISABLE + " AND local_artist_id > ?1 \
         ORDER BY local_artist_id LIMIT ?2";
    pub(super) PERSON_HOLDS = "SELECT EXISTS (SELECT 1 FROM local_artist_external_identities \
         WHERE provider = 'musicbrainz' AND decision_source IN " + PERSONS + " \
         AND lower(provider_artist_id) = ?1 AND local_artist_id <> ?2)";
    pub(super) SETTLE_HOLDERS = "SELECT local_artist_id FROM local_artist_external_identities \
                     WHERE provider = 'musicbrainz' AND decision_source IN " + REVISABLE + " \
                     AND local_artist_id IN ( \
                     SELECT local_artist_id FROM local_artist_external_identities \
                      WHERE provider = 'musicbrainz' AND lower(provider_artist_id) = ?1 \
                     UNION SELECT local_artist_id FROM local_track_artists WHERE artist_mbid = ?1 \
                     UNION SELECT local_artist_id FROM local_album_artists WHERE artist_mbid = ?1 \
                     UNION SELECT p.source_local_artist_id FROM library_artist_credit_proofs p \
                      WHERE p.artist_mbid = ?1 AND p.source_local_artist_id IS NOT NULL \
                      AND (" + CURRENT_PROOF + "))";
    pub(super) DROP_REVISABLE_LINK = "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1 \
                 AND provider = 'musicbrainz' AND decision_source IN " + REVISABLE + " \
                 AND lower(provider_artist_id) <> ?2";
    pub(super) WRITE_SETTLED_LINK = "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                     provider_artist_id, decision_source, selected_at) \
                     VALUES (?1, 'musicbrainz', ?2, 'automatic', ?3) \
                     ON CONFLICT (local_artist_id, provider) DO UPDATE SET \
                     provider_artist_id = excluded.provider_artist_id, \
                     decision_source = 'automatic', attempt_id = NULL, \
                     selected_by_user_id = NULL, selected_at = excluded.selected_at, \
                     row_revision = local_artist_external_identities.row_revision + 1 \
                     WHERE local_artist_external_identities.decision_source \
                     IN " + REVISABLE;
    pub(super) APP_LINK = "SELECT provider_artist_id FROM local_artist_external_identities \
         WHERE local_artist_id = ?1 AND provider = 'musicbrainz' \
         AND decision_source IN " + REVISABLE;
    pub(super) DROP_APP_LINK = "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1 \
             AND provider = 'musicbrainz' AND decision_source IN " + REVISABLE + " \
             RETURNING provider_artist_id";
    pub(super) SHARING = "SELECT a.id, a.display_name FROM local_artists a WHERE a.id <> ?2 AND ( \
         a.id IN (SELECT local_artist_id FROM local_artist_external_identities \
          WHERE provider = 'musicbrainz' AND lower(provider_artist_id) = ?1) \
         OR (a.retired_into_artist_id IS NULL AND a.kind <> 'various_artists' \
          AND a.id NOT IN (?3, ?4) \
          AND a.id IN (SELECT local_artist_id FROM local_track_artists WHERE artist_mbid = ?1 \
           UNION SELECT local_artist_id FROM local_album_artists WHERE artist_mbid = ?1 \
           UNION SELECT p.source_local_artist_id FROM library_artist_credit_proofs p \
            WHERE p.artist_mbid = ?1 AND p.source_local_artist_id IS NOT NULL \
            AND (" + CURRENT_PROOF + ")) \
          AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities l \
           WHERE l.local_artist_id = a.id AND l.provider = 'musicbrainz' \
           AND l.decision_source IN " + PERSONS + " AND lower(l.provider_artist_id) <> ?1))) \
         ORDER BY a.display_name, a.id LIMIT 10";
    pub(super) ARTISTS_PERSON_LINKED_TO_MBID = "SELECT local_artist_id FROM local_artist_external_identities \
             WHERE provider = 'musicbrainz' AND decision_source IN " + PERSONS + " \
             AND lower(provider_artist_id) = ?1";
    pub(super) PERSON_LINK_ELSEWHERE = "SELECT EXISTS (SELECT 1 FROM local_artist_external_identities \
         WHERE provider = 'musicbrainz' AND decision_source IN " + PERSONS + " \
         AND lower(provider_artist_id) = ?1 AND local_artist_id <> ?2)";
    pub(super) PERSON_LINKED_ARTIST = "SELECT local_artist_id FROM local_artist_external_identities \
             WHERE provider = 'musicbrainz' AND decision_source IN " + PERSONS + " \
             AND lower(provider_artist_id) = ?1 AND local_artist_id <> ?2 \
             ORDER BY local_artist_id LIMIT 1";
    pub(super) ARTIST_PERSON_LINKS = "SELECT lower(provider_artist_id) FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1 AND provider = 'musicbrainz' \
                 AND decision_source IN " + PERSONS;
    pub(super) ARTISTS_REVISABLY_LINKED_TO_MBID = "SELECT local_artist_id FROM local_artist_external_identities \
                 WHERE provider = 'musicbrainz' AND decision_source IN " + REVISABLE + " \
                 AND lower(provider_artist_id) = ?1 AND local_artist_id <> ?2";
    pub(super) fragment CREDIT_ROWS = "SELECT c.position, c.local_artist_id, c.credited_name, a.display_name, \
         a.retired_into_artist_id IS NULL AND a.kind <> 'various_artists' \
         AND a.id NOT IN (?2, ?3), c.artist_mbid";
    /// Album `?1`'s credits; `?2` and `?3` are the sentinel ids.
    pub(super) ALBUM_CREDIT_ROWS = CREDIT_ROWS + " FROM local_album_artists c JOIN local_artists a ON a.id = c.local_artist_id \
         WHERE c.local_album_id = ?1 ORDER BY c.position";
    /// Track `?1`'s credits; `?2` and `?3` are the sentinel ids.
    pub(super) TRACK_CREDIT_ROWS = CREDIT_ROWS + " FROM local_track_artists c JOIN local_artists a ON a.id = c.local_artist_id \
         WHERE c.local_track_id = ?1 ORDER BY c.position";
    pub(super) REPOINT_TRACK_CREDIT_MBID = "UPDATE local_track_artists SET artist_mbid = ?2 WHERE artist_mbid = ?1";
    pub(super) REPOINT_ALBUM_CREDIT_MBID = "UPDATE local_album_artists SET artist_mbid = ?2 WHERE artist_mbid = ?1";
    pub(super) RETARGET_ALBUM_PLAYS = "UPDATE library_play_history SET local_album_id = ?1 WHERE local_album_id = ?2";
    pub(super) RETARGET_ARTIST_PLAYS = "UPDATE library_play_history SET local_artist_id = ?1 WHERE local_artist_id = ?2";
    pub(super) RETARGET_TRACK_PLAYS = "UPDATE library_play_history SET local_track_id = ?1 WHERE local_track_id = ?2";
    pub(super) ALBUM_ALIASES = "SELECT alias FROM local_album_aliases WHERE local_album_id = ?1 ORDER BY alias";
    pub(super) ARTIST_ALIASES = "SELECT alias FROM local_artist_aliases WHERE local_artist_id = ?1 ORDER BY alias";
}
