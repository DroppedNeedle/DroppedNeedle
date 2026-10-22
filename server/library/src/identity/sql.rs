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
    /// The track table, read through its album index. With no planner
    /// statistics (before the first ANALYZE) the list-order index led by
    /// `availability` looks as good, and would walk every indexed track.
    pub(super) fragment ALBUM_TRACKS = "local_tracks t \
             INDEXED BY idx_local_tracks_album_availability";

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

    /// Save an album's match. The write itself refuses to put an
    /// automatic or embedded row over a person's.
    pub(super) SAVE_ALBUM_IDENTITY = "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, release_mbid, decision_source, \
             selected_at, row_revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (local_album_id, provider) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             release_mbid = excluded.release_mbid, \
             decision_source = excluded.decision_source, \
             selected_at = excluded.selected_at, row_revision = excluded.row_revision \
             WHERE excluded.decision_source NOT IN " + REVISABLE + " \
             OR local_album_external_identities.decision_source IN " + REVISABLE;
    /// Save a track's match, guarded like the album's.
    pub(super) SAVE_TRACK_IDENTITY = "INSERT INTO local_track_external_identities \
             (local_track_id, provider, recording_mbid, release_track_mbid, decision_source, \
             selected_at, row_revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             recording_mbid = excluded.recording_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             decision_source = excluded.decision_source, \
             selected_at = excluded.selected_at, row_revision = excluded.row_revision \
             WHERE excluded.decision_source NOT IN " + REVISABLE + " \
             OR local_track_external_identities.decision_source IN " + REVISABLE;
    /// The album a track (`?1`) is on.
    pub(super) TRACK_ALBUM = "SELECT local_album_id FROM local_tracks WHERE id = ?1";

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
             AND local_track_id IN (SELECT t.id FROM " + ALBUM_TRACKS + " \
             WHERE t.local_album_id = ?4 AND t.availability = 'indexed')";
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

    /// The copy the library shows for one release group (`?1`, lowercase;
    /// provider `?2`): a copy picked by hand (`manual`) first, then the
    /// oldest, over live albums. Its release may be NULL. The unary plus
    /// keeps the planner off the retired-album index, as the catalog's album
    /// reads do.
    pub(super) COPY_SHOWN = "SELECT e.release_mbid, e.decision_source, \
             (SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id = b.id \
              AND t.availability = 'indexed') \
             FROM local_album_external_identities e \
             JOIN local_albums b ON b.id = e.local_album_id \
             WHERE lower(e.release_group_mbid) = ?1 AND e.provider = ?2 \
             AND +b.retired_into_album_id IS NULL \
             ORDER BY e.decision_source <> 'manual', b.created_at LIMIT 1";
    /// The copy acquisition fetches for one release group (`?1`, lowercase;
    /// provider `?2`): the first live copy that names a release, in the
    /// order of `COPY_SHOWN`.
    pub(super) COPY_WITH_RELEASE = "SELECT e.release_mbid, e.decision_source, \
             (SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id = b.id \
              AND t.availability = 'indexed') \
             FROM local_album_external_identities e \
             JOIN local_albums b ON b.id = e.local_album_id \
             WHERE b.retired_into_album_id IS NULL AND e.provider = ?2 \
             AND lower(e.release_group_mbid) = ?1 AND e.release_mbid IS NOT NULL \
             ORDER BY e.decision_source <> 'manual', b.created_at LIMIT 1";

    // -----------------------------------------------------------------
    // Reads.
    // -----------------------------------------------------------------

    /// One album's match (`?1`, provider `?2`).
    pub(super) ALBUM_IDENTITY = "SELECT release_group_mbid, release_mbid, decision_source, \
             row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    /// One track's match (`?1`, provider `?2`).
    pub(super) TRACK_IDENTITY = "SELECT recording_mbid, release_track_mbid, decision_source, \
             row_revision FROM local_track_external_identities \
             WHERE local_track_id = ?1 AND provider = ?2";
    /// The album's match row (`?1`, provider `?2`), every column an undo
    /// puts back.
    pub(super) ALBUM_ROW = "SELECT release_group_mbid, release_mbid, decision_source, \
             matcher_version, attempt_id, selected_by_user_id, selected_at, row_revision \
             FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2";
    /// The rows of the album's (`?1`) indexed files (provider `?2`).
    pub(super) TRACK_ROWS = "SELECT i.local_track_id, i.recording_mbid, i.release_mbid, \
             i.release_track_mbid, i.medium_position, i.release_track_position, \
             i.decision_source, i.attempt_id, i.selected_at \
             FROM local_track_external_identities i \
             JOIN " + ALBUM_TRACKS + " ON t.id = i.local_track_id \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' AND i.provider = ?2 \
             ORDER BY i.local_track_id";
    /// One file's row (`?1`, provider `?2`).
    pub(super) TRACK_ROW = "SELECT local_track_id, recording_mbid, release_mbid, \
             release_track_mbid, medium_position, release_track_position, decision_source, \
             attempt_id, selected_at FROM local_track_external_identities \
             WHERE local_track_id = ?1 AND provider = ?2";
    /// The album match's row revision (`?1`, provider `?2`).
    pub(super) IDENTITY_REVISION = "SELECT row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    /// The album's (`?1`) indexed files.
    pub(super) INDEXED_TRACKS = "SELECT t.id FROM " + ALBUM_TRACKS + " \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' ORDER BY t.id";
    /// The album's (`?1`) indexed files placed on a track of release `?2`
    /// (provider `?3`).
    pub(super) MAPPED_TRACKS = "SELECT t.id FROM " + ALBUM_TRACKS + " \
             JOIN local_track_external_identities i \
             ON i.local_track_id = t.id AND i.provider = ?3 \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
             AND lower(i.release_mbid) = lower(?2) \
             AND trim(COALESCE(i.release_track_mbid, '')) <> '' ORDER BY t.id";
    /// The album's (`?1`) files whose match by hand stands, with the
    /// release track each holds (provider `?2`). The row the hand match
    /// wrote is `manual`, so only such a row stands for it.
    pub(super) HAND_MATCHES_STANDING = "SELECT h.local_track_id, i.release_track_mbid \
             FROM library_track_hand_matches h \
             JOIN " + ALBUM_TRACKS + " ON t.id = h.local_track_id \
             JOIN local_track_external_identities i \
             ON i.local_track_id = h.local_track_id AND i.provider = ?2 \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
             AND i.decision_source = 'manual' AND lower(i.release_mbid) = lower(h.release_mbid) \
             AND lower(i.release_track_mbid) = lower(h.release_track_mbid) \
             ORDER BY h.local_track_id";
    /// The revisions an automatic-edition undo of the album (`?1`,
    /// provider `?2`) is still live at: only for the row the automatic
    /// seal wrote.
    pub(super) LIVE_AUTOMATIC_UNDO = "SELECT u.expected_post_album_revision, \
             u.expected_post_identity_revision FROM library_automatic_edition_undo u \
             JOIN local_albums a ON a.id = u.local_album_id \
             JOIN local_album_external_identities i \
             ON i.local_album_id = u.local_album_id AND i.provider = ?2 \
             WHERE u.local_album_id = ?1 AND u.consumed_at IS NULL \
             AND i.decision_source = 'automatic' \
             AND i.row_revision = u.expected_post_identity_revision \
             AND a.row_revision = u.expected_post_album_revision";
    /// Whether the album's (`?1`, provider `?2`) choice undo still answers
    /// to its identity.
    pub(super) UNDO_AVAILABLE = "SELECT EXISTS(SELECT 1 FROM library_edition_choice_undo u \
             LEFT JOIN local_album_external_identities i \
             ON i.local_album_id = u.local_album_id AND i.provider = ?2 \
             WHERE u.local_album_id = ?1 \
             AND COALESCE(i.row_revision, 0) = u.expected_identity_revision)";
    /// The newest stored release document (provider `?1`, kind `?2`, id
    /// `?3`, input hash `?4`) fetched at or after `?5`.
    pub(super) STORED_RELEASE = "SELECT canonical_payload_json \
             FROM library_management_metadata_snapshots \
             WHERE provider = ?1 AND entity_kind = ?2 AND entity_id = ?3 \
             AND input_hash = ?4 AND fetched_at >= ?5 ORDER BY fetched_at DESC LIMIT 1";

    // -----------------------------------------------------------------
    // The undo snapshot and the choice undo.
    // -----------------------------------------------------------------

    pub(super) MATCH_STATE_ROW = "SELECT state, reason_code, release_mbid, candidates_json, \
             updated_at FROM library_album_match_state WHERE local_album_id = ?1";
    pub(super) EXCLUSION_ROW = "SELECT reason, excluded_by_user_id, excluded_at, row_revision \
             FROM library_management_exclusions WHERE local_album_id = ?1";
    pub(super) CUSTOM_ACTIVE =
        "SELECT manifest_id FROM library_custom_edition_active WHERE local_album_id = ?1";
    pub(super) HAS_CUSTOM = "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active \
             WHERE local_album_id = ?1)";
    pub(super) REMAP_ROW = "SELECT release_mbid, chosen_by_user_id, origin, queued_at \
             FROM library_edition_remap_queue WHERE local_album_id = ?1";
    /// Keep the album's one choice undo (id `?1`, album `?2`, audit row
    /// `?3`, prior album row `?4`, tracks `?5`, match state `?6`, keyed to
    /// identity revision `?7`, at `?8`, hold `?9`).
    pub(super) KEEP_CHOICE_UNDO = "INSERT INTO library_edition_choice_undo (id, \
             local_album_id, action_id, prior_identity_json, prior_tracks_json, \
             prior_match_state_json, expected_identity_revision, created_at, prior_hold_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT (local_album_id) DO UPDATE SET id = excluded.id, \
             action_id = excluded.action_id, prior_identity_json = excluded.prior_identity_json, \
             prior_tracks_json = excluded.prior_tracks_json, \
             prior_match_state_json = excluded.prior_match_state_json, \
             expected_identity_revision = excluded.expected_identity_revision, \
             created_at = excluded.created_at, prior_hold_json = excluded.prior_hold_json";
    /// A choice undo carried through the identification its change queued.
    /// A record without the mark is not carried.
    pub(super) fragment CARRIED =
        "COALESCE(json_extract(prior_hold_json, '$.carried'), 0) = 1";
    /// Key the album's (`?1`) carried undo still at revision `?2` to `?3`
    /// and clear its mark.
    pub(super) CARRY_CHOICE_UNDO = "UPDATE library_edition_choice_undo \
             SET expected_identity_revision = ?3, \
             prior_hold_json = json_set(prior_hold_json, '$.carried', json('false')) \
             WHERE local_album_id = ?1 AND expected_identity_revision = ?2 AND " + CARRIED;
    /// Forget the album's (`?1`) choice undo unless it is carried and at
    /// revision `?2`.
    pub(super) FORGET_UNCARRIED_CHOICE_UNDO = "DELETE FROM library_edition_choice_undo \
             WHERE local_album_id = ?1 AND NOT (expected_identity_revision = ?2 AND " + CARRIED
             + ")";
    pub(super) CHOICE_UNDO = "SELECT id, prior_identity_json, prior_tracks_json, \
             prior_match_state_json, expected_identity_revision, prior_hold_json \
             FROM library_edition_choice_undo WHERE local_album_id = ?1";
    pub(super) CHOICE_UNDO_AT = "SELECT id, prior_match_state_json \
             FROM library_edition_choice_undo \
             WHERE local_album_id = ?1 AND expected_identity_revision = ?2";
    pub(super) CONSUME_CHOICE_UNDO = "DELETE FROM library_edition_choice_undo WHERE id = ?1";
    pub(super) DELETE_EXCLUSION =
        "DELETE FROM library_management_exclusions WHERE local_album_id = ?1";
    /// Put back the album's (`?1`) "leave unmanaged" (reason `?2`, by `?3`
    /// or else the one undoing, `?5`, at `?4`).
    pub(super) RESTORE_EXCLUSION = "INSERT INTO library_management_exclusions (local_album_id, \
             reason, excluded_by_user_id, excluded_at, row_revision) \
             SELECT ?1, ?2, chooser, ?4, 1 FROM (SELECT COALESCE( \
             (SELECT id FROM auth_users WHERE id = ?3), \
             (SELECT id FROM auth_users WHERE id = ?5)) AS chooser) \
             WHERE chooser IS NOT NULL \
             ON CONFLICT (local_album_id) DO UPDATE SET reason = excluded.reason, \
             excluded_by_user_id = excluded.excluded_by_user_id, \
             excluded_at = excluded.excluded_at, row_revision = row_revision + 1";
    pub(super) DROP_CUSTOM =
        "DELETE FROM library_custom_edition_active WHERE local_album_id = ?1";
    /// Make manifest `?2` the album's (`?1`) custom edition again, at `?3`,
    /// unless the manifest is gone.
    pub(super) RESTORE_CUSTOM = "INSERT INTO library_custom_edition_active (local_album_id, \
             manifest_id, activated_at, row_revision) SELECT ?1, id, ?3, 1 \
             FROM library_custom_edition_manifests WHERE id = ?2 AND local_album_id = ?1 \
             ON CONFLICT (local_album_id) DO UPDATE SET manifest_id = excluded.manifest_id, \
             activated_at = excluded.activated_at, row_revision = row_revision + 1";
    /// Put back the album's (`?1`) queued placement (release `?2`, chosen
    /// by `?3`, queued at `?4`, origin `?5`), to run at once.
    pub(super) RESTORE_REMAP = "INSERT INTO library_edition_remap_queue (local_album_id, \
             release_mbid, chosen_by_user_id, queued_at, origin) \
             VALUES (?1, ?2, (SELECT id FROM auth_users WHERE id = ?3), ?4, ?5) \
             ON CONFLICT (local_album_id) DO UPDATE SET release_mbid = excluded.release_mbid, \
             chosen_by_user_id = excluded.chosen_by_user_id, queued_at = excluded.queued_at, \
             attempts = 0, not_before = 0, last_code = NULL, origin = excluded.origin";
    pub(super) PUT_MATCH_STATE = "INSERT INTO library_album_match_state (local_album_id, \
             state, reason_code, release_mbid, candidates_json, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    /// Write a snapshot track row back (`?1` .. `?10`); an existing row
    /// takes the next revision, and a pruned attempt restores as none.
    pub(super) RESTORE_TRACK_ROW = "INSERT INTO local_track_external_identities \
             (local_track_id, provider, recording_mbid, release_mbid, release_track_mbid, \
             medium_position, release_track_position, decision_source, attempt_id, \
             selected_at, row_revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, \
             (SELECT id FROM library_identification_attempts WHERE id = ?9), ?10, 1) \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             medium_position = excluded.medium_position, \
             release_track_position = excluded.release_track_position, \
             decision_source = excluded.decision_source, attempt_id = excluded.attempt_id, \
             selected_at = excluded.selected_at, row_revision = row_revision + 1";
    pub(super) DELETE_TRACK_ROW = "DELETE FROM local_track_external_identities \
             WHERE local_track_id = ?1 AND provider = ?2";
    /// Write a snapshot album row back (`?1` .. `?9`).
    pub(super) RESTORE_ALBUM_ROW = "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, release_mbid, decision_source, \
             matcher_version, attempt_id, selected_by_user_id, selected_at, row_revision) \
             VALUES (?1, ?9, ?2, ?3, ?4, ?5, \
             (SELECT id FROM library_identification_attempts WHERE id = ?6), \
             (SELECT id FROM auth_users WHERE id = ?7), ?8, 1) \
             ON CONFLICT (local_album_id, provider) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             release_mbid = excluded.release_mbid, \
             decision_source = excluded.decision_source, \
             matcher_version = excluded.matcher_version, attempt_id = excluded.attempt_id, \
             selected_by_user_id = excluded.selected_by_user_id, \
             selected_at = excluded.selected_at, row_revision = row_revision + 1";

    // -----------------------------------------------------------------
    // The automatic-edition undo.
    // -----------------------------------------------------------------

    pub(super) AUTOMATIC_UNDO = "SELECT id, prior_identity_json, prior_track_identities_json, \
             expected_post_album_revision, expected_post_identity_revision \
             FROM library_automatic_edition_undo \
             WHERE local_album_id = ?1 AND consumed_at IS NULL";
    pub(super) ALBUM_REVISION = "SELECT row_revision FROM local_albums WHERE id = ?1";
    /// Put the prior album row back over the automatic one (`?1` .. `?9`).
    pub(super) PUT_BACK_ALBUM_ROW = "UPDATE local_album_external_identities \
             SET release_group_mbid = ?2, release_mbid = ?3, decision_source = ?4, \
             matcher_version = ?5, \
             attempt_id = (SELECT id FROM library_identification_attempts WHERE id = ?6), \
             selected_by_user_id = (SELECT id FROM auth_users WHERE id = ?7), \
             selected_at = ?8, row_revision = row_revision + 1 \
             WHERE local_album_id = ?1 AND provider = ?9";
    pub(super) CONSUME_AUTOMATIC_UNDO = "UPDATE library_automatic_edition_undo \
             SET consumed_at = ?2, consumed_action_id = ?3 WHERE id = ?1";

    // -----------------------------------------------------------------
    // A person's commands.
    // -----------------------------------------------------------------

    /// Seal the album (`?1`, provider `?2`) as a person's: group `?3`,
    /// release `?4` (or none), chosen by `?5` when an account, at `?6`.
    pub(super) SEAL_PERSONS_ALBUM = "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, release_mbid, decision_source, \
             selected_by_user_id, selected_at, row_revision) VALUES (?1, ?2, ?3, ?4, 'manual', \
             (SELECT id FROM auth_users WHERE id = ?5), ?6, 1) \
             ON CONFLICT (local_album_id, provider) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             release_mbid = excluded.release_mbid, decision_source = 'manual', \
             selected_by_user_id = excluded.selected_by_user_id, \
             selected_at = excluded.selected_at, row_revision = row_revision + 1";
    /// Place a file (`?1`) the album (`?9`) holds on its release track as
    /// the person's (provider `?2`, recording `?3`, release `?4`, release
    /// track `?5`, disc `?6`, position `?7`, at `?8`).
    pub(super) PLACE_CHOSEN_TRACK = "INSERT INTO local_track_external_identities \
             (local_track_id, provider, recording_mbid, release_mbid, release_track_mbid, \
             medium_position, release_track_position, decision_source, selected_at, \
             row_revision) \
             SELECT t.id, ?2, ?3, ?4, ?5, ?6, ?7, 'manual', ?8, 1 FROM local_tracks t \
             WHERE t.id = ?1 AND t.local_album_id = ?9 AND t.availability = 'indexed' \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             medium_position = excluded.medium_position, \
             release_track_position = excluded.release_track_position, \
             decision_source = excluded.decision_source, selected_at = excluded.selected_at, \
             row_revision = row_revision + 1";
    /// Take a file (`?1`) the album (`?4`) holds off any release track, as
    /// the person's, at `?2` (provider `?3`).
    pub(super) UNPLACE_CHOSEN_TRACK = "UPDATE local_track_external_identities \
             SET release_mbid = NULL, release_track_mbid = NULL, medium_position = NULL, \
             release_track_position = NULL, decision_source = 'manual', selected_at = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1 AND provider = ?3 \
             AND local_track_id IN (SELECT t.id FROM " + ALBUM_TRACKS + " \
             WHERE t.local_album_id = ?4 AND t.availability = 'indexed')";
    /// Take every indexed file of the album (`?1`) off its release track
    /// as the person's, at `?2` (provider `?3`).
    pub(super) UNPLACE_ALBUM_TRACKS = "UPDATE local_track_external_identities \
             SET release_mbid = NULL, release_track_mbid = NULL, medium_position = NULL, \
             release_track_position = NULL, decision_source = 'manual', selected_at = ?2, \
             row_revision = row_revision + 1 WHERE provider = ?3 AND local_track_id IN \
             (SELECT t.id FROM " + ALBUM_TRACKS + " \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed')";
    /// Make the album's (`?1`) revisable match the person's (`?2`), at
    /// `?3` (provider `?4`); a v2 choice already is one and stays as it is.
    pub(super) CONFIRM_ALBUM = "UPDATE local_album_external_identities \
             SET decision_source = 'manual', \
             selected_by_user_id = (SELECT id FROM auth_users WHERE id = ?2), selected_at = ?3, \
             row_revision = row_revision + 1 WHERE local_album_id = ?1 AND provider = ?4 \
             AND decision_source IN " + REVISABLE;
    /// Make the revisable rows of the album's (`?1`) indexed files the
    /// person's, at `?2` (provider `?3`).
    pub(super) CONFIRM_TRACKS = "UPDATE local_track_external_identities \
             SET decision_source = 'manual', selected_at = ?2, row_revision = row_revision + 1 \
             WHERE provider = ?3 AND decision_source IN " + REVISABLE + " \
             AND local_track_id IN (SELECT t.id FROM " + ALBUM_TRACKS + " \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed')";
    /// Hand the album's (`?1`) match back to automatic choice, at `?2`
    /// (provider `?3`).
    pub(super) HAND_BACK_ALBUM = "UPDATE local_album_external_identities \
             SET decision_source = 'automatic', selected_by_user_id = NULL, selected_at = ?2, \
             row_revision = row_revision + 1 WHERE local_album_id = ?1 AND provider = ?3";
    /// Hand the person's rows of the album's (`?1`) indexed files back to
    /// automatic choice, at `?2` (provider `?3`); rows the tags carried
    /// keep their source.
    pub(super) HAND_BACK_TRACKS = "UPDATE local_track_external_identities \
             SET decision_source = 'automatic', selected_at = ?2, row_revision = row_revision + 1 \
             WHERE provider = ?3 AND decision_source IN " + PERSONS + " \
             AND local_track_id IN (SELECT t.id FROM " + ALBUM_TRACKS + " \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed')";
    /// The album (`?1`) keeps its release group as the person's (`?2`),
    /// at `?3`, and loses its exact release (provider `?4`).
    pub(super) LEAVE_ALBUM_UNMANAGED = "UPDATE local_album_external_identities \
             SET release_mbid = NULL, decision_source = 'manual', selected_by_user_id = ?2, \
             selected_at = ?3, row_revision = row_revision + 1 \
             WHERE local_album_id = ?1 AND provider = ?4";
    /// Keep the album (`?1`) out of Library Management, by `?2` at `?3`.
    pub(super) EXCLUDE_ALBUM = "INSERT INTO library_management_exclusions (local_album_id, \
             reason, excluded_by_user_id, excluded_at, row_revision) \
             VALUES (?1, 'administrator_choice', ?2, ?3, 1) \
             ON CONFLICT (local_album_id) DO UPDATE SET reason = excluded.reason, \
             excluded_by_user_id = excluded.excluded_by_user_id, \
             excluded_at = excluded.excluded_at, row_revision = row_revision + 1";
    /// Delete the rows of the album's (`?1`) indexed files (provider `?2`).
    pub(super) DELETE_ALBUM_TRACK_ROWS = "DELETE FROM local_track_external_identities \
             WHERE provider = ?2 AND local_track_id IN \
             (SELECT t.id FROM " + ALBUM_TRACKS + " \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed')";
    /// Mark the album (`?1`) unmatched with reason `?2`, at `?3`.
    pub(super) MARK_UNMATCHED = "INSERT INTO library_album_match_state (local_album_id, state, \
             reason_code, release_mbid, candidates_json, updated_at) \
             VALUES (?1, 'unmatched', ?2, NULL, '[]', ?3) \
             ON CONFLICT (local_album_id) DO UPDATE SET state = 'unmatched', \
             reason_code = excluded.reason_code, release_mbid = NULL, candidates_json = '[]', \
             updated_at = excluded.updated_at";
    /// Clear the album's (`?1`) match flag when its reason is `?2`.
    pub(super) CLEAR_FLAG_WITH_REASON = "DELETE FROM library_album_match_state \
             WHERE local_album_id = ?1 AND reason_code = ?2";
    /// Clear the album's (`?1`) flag left by a chosen edition's placement
    /// that failed (`CHOSEN_EDITION_*`).
    pub(super) CLEAR_CHOSEN_FLAG = "DELETE FROM library_album_match_state \
             WHERE local_album_id = ?1 AND reason_code LIKE 'CHOSEN\\_EDITION\\_%' ESCAPE '\\'";
    /// Keep what a match by hand of file `?1` (release `?2`, release track
    /// `?3`, prior row `?4`, by `?5`, at `?6` ms) needs for its undo.
    pub(super) KEEP_HAND_MATCH = "INSERT INTO library_track_hand_matches (local_track_id, \
             release_mbid, release_track_mbid, prior_identity_json, matched_by_user_id, \
             matched_at) VALUES (?1, ?2, ?3, ?4, (SELECT id FROM auth_users WHERE id = ?5), ?6) \
             ON CONFLICT (local_track_id) DO UPDATE SET release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             prior_identity_json = excluded.prior_identity_json, \
             matched_by_user_id = excluded.matched_by_user_id, matched_at = excluded.matched_at";
    pub(super) FORGET_HAND_MATCH =
        "DELETE FROM library_track_hand_matches WHERE local_track_id = ?1";
    /// Write one track row (`?1` .. `?9`); an existing row takes the next
    /// revision.
    pub(super) UPSERT_TRACK = "INSERT INTO local_track_external_identities (local_track_id, \
             provider, recording_mbid, release_mbid, release_track_mbid, medium_position, \
             release_track_position, decision_source, selected_at, row_revision) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1) \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             medium_position = excluded.medium_position, \
             release_track_position = excluded.release_track_position, \
             decision_source = excluded.decision_source, selected_at = excluded.selected_at, \
             row_revision = row_revision + 1";

    // -----------------------------------------------------------------
    // Custom editions.
    // -----------------------------------------------------------------

    /// The album (`?1`) as a manifest freezes it, with its artist's match
    /// when a person chose it (provider `?2`).
    pub(super) CUSTOM_ALBUM_FACTS = "SELECT a.title, COALESCE(a.album_artist_name, ''), \
             i.provider_artist_id, a.album_artist_sort_name, a.year, a.original_release_date, \
             a.primary_genre, a.is_compilation, a.row_revision FROM local_albums a \
             LEFT JOIN local_artist_external_identities i \
             ON i.local_artist_id = a.album_artist_id AND i.provider = ?2 \
             AND i.decision_source IN " + PERSONS + " WHERE a.id = ?1";
    pub(super) TRACK_RECORDING = "SELECT recording_mbid FROM local_track_external_identities \
             WHERE local_track_id = ?1 AND provider = ?2";
    pub(super) TRACK_RECORDING_REVISION = "SELECT recording_mbid, row_revision \
             FROM local_track_external_identities WHERE local_track_id = ?1 AND provider = ?2";
    pub(super) NEXT_MANIFEST_VERSION = "SELECT COALESCE(MAX(version), 0) + 1 \
             FROM library_custom_edition_manifests WHERE local_album_id = ?1";
    pub(super) MANIFEST_TRACKS = "SELECT id, row_revision, stat_revision, \
             COALESCE(tag_revision, ''), title, COALESCE(artist_name, ''), album_title, \
             disc_number, track_number, embedded_artist_mbid, file_format, duration_seconds, \
             album_artist_name, year, genre, title_sort, artist_sort, album_sort, \
             album_artist_sort, disc_subtitle, is_compilation \
             FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed'";
    pub(super) INSERT_MANIFEST = "INSERT INTO library_custom_edition_manifests (id, \
             local_album_id, version, release_group_mbid, album_title, album_artist_name, \
             artist_mbid, album_metadata_json, source_album_revision, source_identity_revision, \
             input_revision, content_hash, selected_candidate_key, sealed_by_user_id, sealed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)";
    pub(super) INSERT_MANIFEST_TRACK = "INSERT INTO library_custom_edition_tracks (manifest_id, \
             ordinal, local_track_id, source_track_revision, source_identity_revision, \
             stat_revision, tag_revision, title, artist_name, album_title, album_artist_name, \
             disc_number, track_number, recording_mbid, artist_mbid, album_artist_mbid, \
             metadata_json, file_format, duration_seconds) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)";
    pub(super) ACTIVATE_CUSTOM = "INSERT INTO library_custom_edition_active (local_album_id, \
             manifest_id, activated_at, row_revision) VALUES (?1, ?2, ?3, 1) \
             ON CONFLICT (local_album_id) DO UPDATE SET manifest_id = excluded.manifest_id, \
             activated_at = excluded.activated_at, row_revision = row_revision + 1";

    // -----------------------------------------------------------------
    // Editions waiting for their files to be placed.
    // -----------------------------------------------------------------

    pub(super) NEXT_REMAP = "SELECT local_album_id, release_mbid, chosen_by_user_id, attempts, \
             origin FROM library_edition_remap_queue WHERE not_before <= ?1 \
             ORDER BY queued_at, local_album_id LIMIT 1";
    pub(super) REMAP_ASK = "SELECT release_mbid, origin FROM library_edition_remap_queue \
             WHERE local_album_id = ?1";
    /// Whether the album's (`?1`, provider `?2`) match is a person's and
    /// names release `?3`.
    pub(super) REMAP_HOLDS = "SELECT EXISTS(SELECT 1 FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2 AND decision_source IN " + PERSONS
             + " AND lower(release_mbid) = lower(?3))";
    pub(super) RETRY_REMAP = "UPDATE library_edition_remap_queue SET attempts = attempts + 1, \
             not_before = ?2, last_code = ?3 WHERE local_album_id = ?1";
    /// Hand the remap's own sealed row (`?1`, release `?4`, provider `?3`)
    /// back to automatic choice at `?2`. Only a row this app sealed as a
    /// person's (`manual`) goes back; a v2 choice is never handed back.
    pub(super) GIVE_UP_ALBUM = "UPDATE local_album_external_identities \
             SET decision_source = 'automatic', selected_by_user_id = NULL, selected_at = ?2, \
             row_revision = row_revision + 1 WHERE local_album_id = ?1 AND provider = ?3 \
             AND decision_source = 'manual' AND lower(release_mbid) = lower(?4)";
    /// The given-up choice's own unplaced rows of the album (`?1`) go back
    /// to automatic choice at `?2` (provider `?3`).
    pub(super) GIVE_UP_TRACKS = "UPDATE local_track_external_identities \
             SET decision_source = 'automatic', selected_at = ?2, row_revision = row_revision + 1 \
             WHERE provider = ?3 AND decision_source = 'manual' AND release_mbid IS NULL \
             AND local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = ?1)";
    pub(super) KEEP_UNPLACEABLE = "INSERT INTO library_album_match_state (local_album_id, \
             state, reason_code, release_mbid, candidates_json, updated_at) \
             VALUES (?1, 'unmatched', ?2, ?3, '[]', ?4) \
             ON CONFLICT (local_album_id) DO UPDATE SET state = excluded.state, \
             reason_code = excluded.reason_code, release_mbid = excluded.release_mbid, \
             updated_at = excluded.updated_at";
    pub(super) QUEUE_REMAP = "INSERT INTO library_edition_remap_queue (local_album_id, \
             release_mbid, chosen_by_user_id, queued_at, origin) \
             VALUES (?1, lower(?2), ?3, ?4, ?5) \
             ON CONFLICT (local_album_id) DO UPDATE SET release_mbid = excluded.release_mbid, \
             chosen_by_user_id = excluded.chosen_by_user_id, queued_at = excluded.queued_at, \
             attempts = 0, not_before = 0, last_code = NULL, origin = excluded.origin";
    /// The release a person's match of the album (`?1`, provider `?2`)
    /// names, while no custom edition is active on it.
    pub(super) PLACEMENT_RELEASE = "SELECT lower(release_mbid) \
             FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2 AND decision_source IN " + PERSONS
             + " AND trim(release_mbid) <> '' \
             AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = ?1)";
    /// Whether an indexed file of the album (`?1`) is not placed on release
    /// `?3` (provider `?2`).
    pub(super) HAS_UNPLACED_FILE = "SELECT EXISTS(SELECT 1 FROM " + ALBUM_TRACKS + " \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' AND NOT EXISTS \
             (SELECT 1 FROM local_track_external_identities i WHERE i.local_track_id = t.id \
             AND i.provider = ?2 AND lower(i.release_mbid) = ?3 \
             AND i.release_track_mbid IS NOT NULL))";
    pub(super) QUEUE_PLACEMENT = "INSERT INTO library_edition_remap_queue (local_album_id, \
             release_mbid, chosen_by_user_id, queued_at, origin) VALUES (?1, ?2, NULL, ?3, ?4) \
             ON CONFLICT (local_album_id) DO NOTHING";
    pub(super) ALBUM_RELEASE = "SELECT release_mbid FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    /// The album's (`?1`) indexed files already on release `?3` (provider
    /// `?2`), with the release track each holds.
    pub(super) HELD_ON_RELEASE = "SELECT t.id, i.release_track_mbid FROM " + ALBUM_TRACKS + " \
             JOIN local_track_external_identities i \
             ON i.local_track_id = t.id AND i.provider = ?2 \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
             AND lower(i.release_mbid) = lower(?3) AND i.release_track_mbid IS NOT NULL";

    // -----------------------------------------------------------------
    // Two rows of one album: a scan's follow and a merge.
    // -----------------------------------------------------------------

    /// The album's (`?1`, provider `?2`) match as settling two rows reads
    /// it: group and release folded, and who decided it.
    pub(super) MATCH_OF = "SELECT lower(release_group_mbid), \
             lower(NULLIF(trim(release_mbid), '')), decision_source \
             FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2";
    pub(super) MANAGEMENT_QUEUED = "SELECT EXISTS(SELECT 1 \
             FROM library_management_automatic_queue WHERE local_album_id = ?1)";
    /// Move the reviews a person closed on `?1` to `?2`.
    pub(super) MOVE_CLOSED_REVIEWS = "UPDATE library_identify_reviews SET local_album_id = ?2 \
             WHERE local_album_id = ?1 AND state <> 'pending'";
    pub(super) DROP_REVIEWS = "DELETE FROM library_identify_reviews WHERE local_album_id = ?1";
    pub(super) DROP_PENDING_REVIEWS = "DELETE FROM library_identify_reviews \
             WHERE local_album_id = ?1 AND state = 'pending'";
    /// Close the album's (`?1`) waiting reviews as dismissed by nobody, at
    /// `?2` ms.
    pub(super) DISMISS_PENDING_REVIEWS = "UPDATE library_identify_reviews \
             SET state = 'dismissed', resolved_by_user_id = NULL, updated_ms = ?2 \
             WHERE local_album_id = ?1 AND state = 'pending'";
    pub(super) REMAP_TO_CARRY = "SELECT release_mbid, chosen_by_user_id, origin \
             FROM library_edition_remap_queue WHERE local_album_id = ?1";
    /// A match revision neither album (`?1`, `?2`) has had.
    pub(super) NEXT_SHARED_REVISION = "SELECT MAX(row_revision) + 1 \
             FROM local_album_external_identities WHERE local_album_id IN (?1, ?2)";
    /// Delete the album's (`?1`) match rows of every provider.
    pub(super) DELETE_ALBUM_ROWS = "DELETE FROM local_album_external_identities \
             WHERE local_album_id = ?1";
    /// Put the match of `?1` on `?2` at revision `?3`.
    pub(super) CARRY_ALBUM_ROW = "UPDATE local_album_external_identities \
             SET local_album_id = ?2, row_revision = ?3 WHERE local_album_id = ?1";
    pub(super) MOVE_MATCH_STATE = "UPDATE library_album_match_state SET local_album_id = ?2 \
             WHERE local_album_id = ?1";
    pub(super) SET_REMAP_ORIGIN = "UPDATE library_edition_remap_queue SET origin = ?2 \
             WHERE local_album_id = ?1";

    // -----------------------------------------------------------------
    // Corrections, repairs and imports.
    // -----------------------------------------------------------------

    /// Move the edition of `?1` (provider `?3`) onto `?2`, at its next
    /// revision.
    pub(super) MOVE_EDITION_ROW = "UPDATE local_album_external_identities \
             SET local_album_id = ?2, row_revision = row_revision + 1 \
             WHERE local_album_id = ?1 AND provider = ?3";
    /// Drop the match state of `?2` when `?1` has one to move onto it.
    pub(super) MAKE_ROOM_FOR_MATCH_STATE = "DELETE FROM library_album_match_state \
             WHERE local_album_id = ?2 AND EXISTS (SELECT 1 FROM library_album_match_state \
             WHERE local_album_id = ?1)";
    /// Drop the queued placement of `?2` when `?1` has one to move onto it.
    pub(super) MAKE_ROOM_FOR_REMAP = "DELETE FROM library_edition_remap_queue \
             WHERE local_album_id = ?2 AND EXISTS (SELECT 1 FROM library_edition_remap_queue \
             WHERE local_album_id = ?1)";
    pub(super) MOVE_REMAP = "UPDATE library_edition_remap_queue SET local_album_id = ?2 \
             WHERE local_album_id = ?1";
    /// Take file `?3` off a release track unless that track is on release
    /// `?2` (none: off any release; provider `?1`).
    pub(super) SETTLE_TRACK = "DELETE FROM local_track_external_identities \
             WHERE provider = ?1 AND release_mbid IS NOT NULL \
             AND (?2 IS NULL OR lower(release_mbid) <> lower(?2)) AND local_track_id = ?3";
    /// Who decided the album's (`?1`, provider `?3`) match when it names
    /// release `?2`.
    pub(super) SOURCE_NAMING_RELEASE = "SELECT decision_source \
             FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?3 AND release_mbid = ?2";
    /// A person's link of the album (`?1`, provider `?2`, group `?3`,
    /// release `?4`, matcher `?5`, attempt `?6`, by `?7`, at `?8`).
    pub(super) LINK_ALBUM = "INSERT INTO local_album_external_identities (local_album_id, \
             provider, release_group_mbid, release_mbid, decision_source, matcher_version, \
             attempt_id, selected_by_user_id, selected_at) \
             VALUES (?1, ?2, ?3, ?4, 'manual', ?5, ?6, ?7, ?8)";
    /// A person's link of file `?1` of album `?10` on its release track
    /// (provider `?2`, recording `?3`, release `?4`, release track `?5`,
    /// disc `?6`, position `?7`, attempt `?8`, at `?9`).
    pub(super) LINK_TRACK = "INSERT INTO local_track_external_identities (local_track_id, \
             provider, recording_mbid, release_mbid, release_track_mbid, medium_position, \
             release_track_position, decision_source, attempt_id, selected_at) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'manual', ?8, ?9 \
             WHERE EXISTS (SELECT 1 FROM local_tracks WHERE id = ?1 AND local_album_id = ?10) \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             medium_position = excluded.medium_position, \
             release_track_position = excluded.release_track_position, \
             decision_source = excluded.decision_source, attempt_id = excluded.attempt_id, \
             selected_at = excluded.selected_at, \
             row_revision = local_track_external_identities.row_revision + 1";

    // -----------------------------------------------------------------
    // Folding a split v2 album, and its undo.
    // -----------------------------------------------------------------

    /// Delete file `?1`'s v2 choice (provider `?2`).
    pub(super) DROP_LEGACY_TRACK_ROW = "DELETE FROM local_track_external_identities \
             WHERE local_track_id = ?1 AND provider = ?2 AND decision_source = 'legacy_import'";
    pub(super) GROUPING_LOCKED = "SELECT grouping_locked FROM local_albums WHERE id = ?1";
    /// The audit row `?1` of a fold (reason `?2`) and the album it kept.
    pub(super) FOLD_ACTION = "SELECT before_json, local_album_id FROM library_catalog_actions \
             WHERE id = ?1 AND reason_code = ?2";
    /// Whether album `?1` is retired into `?2`.
    pub(super) RETIRED_INTO = "SELECT EXISTS(SELECT 1 FROM local_albums \
             WHERE id = ?1 AND retired_into_album_id = ?2)";
    /// Whether file `?1` is still an indexed file of album `?2`.
    pub(super) INDEXED_ON = "SELECT EXISTS(SELECT 1 FROM local_tracks \
             WHERE id = ?1 AND local_album_id = ?2 AND availability = 'indexed')";
    /// Bring the album `?1` back from retirement (grouping lock `?2`, at
    /// `?3`).
    pub(super) UNRETIRE_ALBUM = "UPDATE local_albums SET retired_into_album_id = NULL, \
             grouping_locked = ?2, updated_at = ?3, row_revision = row_revision + 1 \
             WHERE id = ?1";
    /// Move file `?2` back onto album `?1`.
    pub(super) MOVE_TRACK_TO = "UPDATE local_tracks SET local_album_id = ?1, \
             row_revision = row_revision + 1 WHERE id = ?2";
    pub(super) TOUCH_ALBUM = "UPDATE local_albums SET updated_at = ?2, \
             row_revision = row_revision + 1 WHERE id = ?1";
    /// The album's (`?1`) takeover mark.
    pub(super) TAKEOVER_ROW = "SELECT cause, noticed_at, handed_back_at \
             FROM library_album_takeovers WHERE local_album_id = ?1";
    /// Put a takeover mark back on the album (`?1`, cause `?2`, noticed at
    /// `?3`, handed back at `?4`), unless it has one.
    pub(super) RESTORE_TAKEOVER = "INSERT INTO library_album_takeovers (local_album_id, \
             cause, noticed_at, handed_back_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (local_album_id) DO NOTHING";
    /// Whether file `?1` holds a person's match (provider `?2`), or was
    /// matched by hand.
    pub(super) PERSONS_TRACK = "SELECT EXISTS(SELECT 1 FROM local_track_external_identities \
             WHERE local_track_id = ?1 AND provider = ?2 AND decision_source IN " + PERSONS
             + ") OR EXISTS(SELECT 1 FROM library_track_hand_matches WHERE local_track_id = ?1)";
    /// Whether a fold of album `?1` was undone (audit kind `?2`).
    pub(super) FOLD_UNDONE = "SELECT EXISTS(SELECT 1 FROM library_catalog_actions \
             WHERE local_album_id = ?1 AND action_kind = ?2)";
    /// Forget that album `?1` was merged into `?2`.
    pub(super) DROP_MERGED_ALIAS = "DELETE FROM local_album_aliases \
             WHERE alias = ?1 AND local_album_id = ?2 AND kind = 'merged_album'";

    // -----------------------------------------------------------------
    // The checks a repair made, read again inside its command.
    // -----------------------------------------------------------------

    /// The live album's (`?1`) row revision, while it has an indexed file.
    pub(super) LIVE_ALBUM_REVISION = "SELECT row_revision FROM local_albums \
             WHERE id = ?1 AND retired_into_album_id IS NULL AND EXISTS \
             (SELECT 1 FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed')";
    /// The album's (`?1`, provider `?2`) match, for the identity hash.
    pub(super) ALBUM_HASH_ROW = "SELECT release_group_mbid, release_mbid, decision_source, \
             row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    /// The album's (`?1`) indexed files' matches (provider `?2`), for the
    /// identity hash.
    pub(super) TRACK_HASH_ROWS = "SELECT t.id, i.recording_mbid, i.release_mbid, \
             i.release_track_mbid, i.medium_position, i.release_track_position, \
             i.decision_source, i.row_revision FROM " + ALBUM_TRACKS + " \
             JOIN local_track_external_identities i \
             ON i.local_track_id = t.id AND i.provider = ?2 \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' ORDER BY t.id";
}
