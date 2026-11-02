use super::*;

/// Two rows of one album each hold a release a person chose, and the
/// two differ. Settling them is refused before anything is written:
/// neither match and no review changes.
#[test]
fn settling_two_different_chosen_matches_is_refused() {
    let dir = droppedneedle_kernel::db::scratch::ScratchDir::new("settle-match").unwrap();
    let path = dir.path().join("app.db");
    droppedneedle_kernel::schema::template::copy_to(&path).unwrap();
    let db = Db::open_for_tests(&path).unwrap();
    let seed = |tx: &Transaction<'_>| {
        tx.execute_batch(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('old', 'r', 'old', 'T', 't', '00000000-0000-4000-8000-000000000002', \
          'automatic', 1, 1), \
         ('new', 'r', 'new', 'T', 't', '00000000-0000-4000-8000-000000000002', \
          'automatic', 1, 1); \
         INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at) VALUES \
         ('old', 'musicbrainz', 'rg', 'rel-1', 'manual', 0), \
         ('new', 'musicbrainz', 'rg', 'rel-2', 'legacy_import', 0); \
         INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
         candidates_json, state, created_at, updated_at) VALUES \
         ('waiting', 'new', 'R', '[]', 'pending', 0, 0), \
         ('closed', 'new', 'R', '[]', 'dismissed', 0, 0);",
        )
    };
    db.write_blocking(Lane::Background, "test.seed", |tx| {
        Ok::<_, droppedneedle_kernel::db::DbError>(seed(tx)?)
    })
    .unwrap();
    let state = |conn: &Connection| -> Vec<(String, String)> {
        conn.prepare(
            "SELECT local_album_id, release_mbid || ' ' || decision_source || ' ' \
             || row_revision FROM local_album_external_identities \
             UNION ALL SELECT local_album_id, id FROM library_identify_reviews ORDER BY 1, 2",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    let before = db
        .read_blocking("test.before", |conn| {
            Ok::<_, droppedneedle_kernel::db::DbError>(state(conn))
        })
        .unwrap();
    assert_eq!(before.len(), 4);

    db.write_blocking(Lane::Background, "test.settle", |tx| {
        assert!(settle_followed_match(tx, "new", "old", 1.0).is_err());
        assert_eq!(state(tx), before);
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .unwrap();
}
