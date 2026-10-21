//! The identity commands' statements, and the two sets of decision
//! sources every other statement takes from here.

droppedneedle_kernel::sql! {
    /// Decision sources automatic work may replace: identification's own
    /// rows and ids the tags carried. Checked against
    /// [`DecisionSource::is_revisable`](super::DecisionSource::is_revisable).
    pub(crate) fragment REVISABLE = "('automatic','embedded')";
    /// Decision sources that are a person's choice, made now or carried
    /// over from v2. Checked against
    /// [`DecisionSource::is_persons_choice`](super::DecisionSource::is_persons_choice).
    pub(crate) fragment PERSONS = "('manual','legacy_import')";

    /// Who decided the album's match (`?1`, provider `?2`).
    pub(super) ALBUM_SOURCE = "SELECT decision_source FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    /// Whether the album (`?1`) has a match (provider `?2`).
    pub(super) HAS_ALBUM_ROW = "SELECT EXISTS (SELECT 1 FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2)";
    /// Delete the album's match, whoever made it.
    pub(super) DELETE_ALBUM_ROW = "DELETE FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    /// Delete the album's match when it is revisable.
    pub(super) DELETE_REVISABLE_ALBUM_ROW = "DELETE FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2 AND decision_source IN " + REVISABLE;
    pub(super) FORGET_AUTOMATIC_UNDO =
        "DELETE FROM library_automatic_edition_undo WHERE local_album_id = ?1";
    pub(super) FORGET_CHOICE_UNDO =
        "DELETE FROM library_edition_choice_undo WHERE local_album_id = ?1";

    /// Seal an automatic album match (`?1`, provider `?2`, group `?3`,
    /// release `?4`, at `?5`), never over a person's.
    pub(super) SEAL_ALBUM = "INSERT INTO local_album_external_identities (local_album_id, \
             provider, release_group_mbid, release_mbid, decision_source, selected_at, \
             row_revision) VALUES (?1, ?2, ?3, ?4, 'automatic', ?5, 1) \
             ON CONFLICT (local_album_id, provider) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             release_mbid = excluded.release_mbid, decision_source = 'automatic', \
             matcher_version = NULL, attempt_id = NULL, selected_by_user_id = NULL, \
             selected_at = excluded.selected_at, row_revision = row_revision + 1 \
             WHERE local_album_external_identities.decision_source IN " + REVISABLE;
    /// Seal an automatic track match (`?1`, provider `?2`, recording `?3`,
    /// release `?4`, release track `?5`, at `?6`) on a file the album
    /// (`?7`) holds, never over a person's.
    pub(super) SEAL_TRACK = "INSERT INTO local_track_external_identities (local_track_id, \
             provider, recording_mbid, release_mbid, release_track_mbid, decision_source, \
             selected_at, row_revision) \
             SELECT t.id, ?2, ?3, ?4, ?5, 'automatic', ?6, 1 FROM local_tracks t \
             WHERE t.id = ?1 AND t.local_album_id = ?7 AND t.availability = 'indexed' \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             medium_position = NULL, release_track_position = NULL, \
             decision_source = 'automatic', attempt_id = NULL, \
             selected_at = excluded.selected_at, row_revision = row_revision + 1 \
             WHERE local_track_external_identities.decision_source IN " + REVISABLE;

    /// Accept an automatic exact edition (`?1`, provider `?2`, group `?3`,
    /// release `?4`, at `?5`).
    pub(super) ACCEPT_ALBUM = "INSERT INTO local_album_external_identities (local_album_id, \
             provider, release_group_mbid, release_mbid, decision_source, selected_at, \
             row_revision) VALUES (?1, ?2, ?3, ?4, 'automatic', ?5, 1) \
             ON CONFLICT (local_album_id, provider) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             release_mbid = excluded.release_mbid, decision_source = 'automatic', \
             selected_by_user_id = NULL, selected_at = excluded.selected_at, \
             row_revision = row_revision + 1";
    /// Place a file (`?1`) the album (`?9`) holds on its release track
    /// (provider `?2`, recording `?3`, release `?4`, release track `?5`,
    /// disc `?6`, position `?7`, at `?8`).
    pub(super) PLACE_TRACK = "INSERT INTO local_track_external_identities (local_track_id, \
             provider, recording_mbid, release_mbid, release_track_mbid, medium_position, \
             release_track_position, decision_source, selected_at, row_revision) \
             SELECT t.id, ?2, ?3, ?4, ?5, ?6, ?7, 'automatic', ?8, 1 FROM local_tracks t \
             WHERE t.id = ?1 AND t.local_album_id = ?9 AND t.availability = 'indexed' \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             medium_position = excluded.medium_position, \
             release_track_position = excluded.release_track_position, \
             decision_source = excluded.decision_source, selected_at = excluded.selected_at, \
             row_revision = row_revision + 1";
    /// Take a file (`?1`) the album (`?4`) holds off any release track, at
    /// `?2` (provider `?3`).
    pub(super) UNPLACE_TRACK = "UPDATE local_track_external_identities SET release_mbid = NULL, \
             release_track_mbid = NULL, medium_position = NULL, \
             release_track_position = NULL, selected_at = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1 AND provider = ?3 \
             AND local_track_id IN (SELECT id FROM local_tracks \
             WHERE local_album_id = ?4 AND availability = 'indexed')";
    pub(super) DROP_REMAP =
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1";

    /// Clear the album's match flag.
    pub(super) CLEAR_FLAG = "DELETE FROM library_album_match_state WHERE local_album_id = ?1";
    /// Set the album's match flag (`?1`, state `?2`, reason `?3`, release
    /// `?4`, candidates `?5`, at `?6`).
    pub(super) SET_FLAG = "INSERT INTO library_album_match_state (local_album_id, state, \
             reason_code, release_mbid, candidates_json, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (local_album_id) DO UPDATE SET state = excluded.state, \
             reason_code = excluded.reason_code, release_mbid = excluded.release_mbid, \
             candidates_json = excluded.candidates_json, updated_at = excluded.updated_at";

    /// Keep the automatic-edition undo for the album (`?5`, provider `?6`):
    /// what the seal replaced (`?2`, `?3`), keyed to the revisions it left,
    /// with its job (`?7`) and reason (`?8`), at `?4`. One live undo per
    /// album: a later seal replaces it.
    pub(super) KEEP_AUTOMATIC_UNDO = "INSERT INTO library_automatic_edition_undo (id, \
             local_album_id, job_id, prior_identity_json, prior_track_identities_json, \
             expected_post_album_revision, expected_post_identity_revision, reason_code, \
             created_at) \
             SELECT ?1, a.id, ?7, ?2, ?3, a.row_revision, i.row_revision, ?8, ?4 \
             FROM local_albums a JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?6 WHERE a.id = ?5 \
             ON CONFLICT (local_album_id) DO UPDATE SET id = excluded.id, \
             job_id = excluded.job_id, evidence_id = NULL, \
             prior_identity_json = excluded.prior_identity_json, \
             prior_track_identities_json = excluded.prior_track_identities_json, \
             expected_post_album_revision = excluded.expected_post_album_revision, \
             expected_post_identity_revision = excluded.expected_post_identity_revision, \
             reason_code = excluded.reason_code, created_at = excluded.created_at, \
             consumed_at = NULL, consumed_action_id = NULL";
}
