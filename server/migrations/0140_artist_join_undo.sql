-- An administrator can undo a join of artist records made because they
-- share a name, recorded as a new kind of catalog action, so the table is
-- rebuilt with that kind in its check. No table references it and no
-- trigger or view names it, so the rebuild runs in the migration's own
-- transaction, keeping every row and rowid, then recreates its indexes.
--
-- Artist id checks keep MusicBrainz's disambiguation of a current id
-- ("US singer"), which the web UI shows where two records share a name:
-- NULL until MusicBrainz is asked, '' when it has none.

CREATE TABLE library_catalog_actions_next (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT UNIQUE,
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    action_kind TEXT NOT NULL CHECK(action_kind IN ('accept_management_track_mappings',
        'album_artist', 'album_artist_undone', 'artist_link_moved', 'artist_link_removed',
        'artist_link_set', 'automatic_exact_edition', 'automatic_match_sealed',
        'automatic_match_withdrawn', 'continue_tracks', 'custom_edition', 'edition_choice',
        'edition_choice_cleared', 'edition_choice_undone', 'edition_confirmed',
        'files_put_back', 'files_recycled', 'fold_undone', 'follow_merged_artist_id',
        'leave_unmanaged', 'management_hand_back_given_up', 'management_handed_back',
        'management_reenabled', 'management_stepped_back', 'merge', 'merge_album',
        'merge_artist', 'merge_artist_undone', 'move', 'new_copy_at_path',
        'other_music_at_path', 'remove_track', 'repair_detach', 'reset',
        'retag_dismissal_ended', 'retag_first_half', 'review_approve', 'review_detach',
        'review_exclude', 'review_reopened', 'review_restore', 'split',
        'track_match_undone', 'track_matched', 'undo_automatic_edition')),
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    before_json TEXT NOT NULL,
    after_json TEXT NOT NULL,
    reason_code TEXT,
    created_at INTEGER NOT NULL,
    CHECK(local_artist_id IS NOT NULL OR local_album_id IS NOT NULL OR local_track_id IS NOT NULL)
) STRICT;

INSERT INTO library_catalog_actions_next (rowid, id, idempotency_key, actor_user_id,
    action_kind, local_artist_id, local_album_id, local_track_id, operation_job_id,
    before_json, after_json, reason_code, created_at)
SELECT rowid, id, idempotency_key, actor_user_id, action_kind, local_artist_id,
    local_album_id, local_track_id, operation_job_id, before_json, after_json,
    reason_code, created_at
FROM library_catalog_actions;

DROP TABLE library_catalog_actions;

ALTER TABLE library_catalog_actions_next RENAME TO library_catalog_actions;

CREATE INDEX idx_library_catalog_actions_actor_user_id
    ON library_catalog_actions(actor_user_id);

CREATE INDEX idx_library_catalog_actions_album_kind
    ON library_catalog_actions(local_album_id, action_kind);

CREATE INDEX idx_library_catalog_actions_local_artist_id
    ON library_catalog_actions(local_artist_id);

CREATE INDEX idx_library_catalog_actions_local_track_id
    ON library_catalog_actions(local_track_id);

CREATE INDEX idx_library_catalog_actions_operation_job_id
    ON library_catalog_actions(operation_job_id);

CREATE INDEX idx_library_catalog_actions_undoes
    ON library_catalog_actions(json_extract(before_json, '$.undoes'))
    WHERE action_kind = 'files_put_back';

ALTER TABLE library_identify_artist_id_checks ADD COLUMN disambiguation TEXT;

PRAGMA user_version = 140;
