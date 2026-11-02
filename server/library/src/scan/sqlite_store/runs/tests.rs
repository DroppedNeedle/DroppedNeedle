use super::*;

fn column<T: rusqlite::types::FromSql>(conn: &rusqlite::Connection, sql: &str) -> Vec<T> {
    conn.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn compat_orphans_go_and_the_queue_keeps_its_place() {
    let (_dir, store) = scratch_store();
    let check = |conn: &Connection| -> Result<(), ScanStoreError> {
        conn.execute_batch(
            "INSERT INTO auth_users (id, display_name, created_at) VALUES
             ('u', 'U', 0), ('v', 'V', 0);
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at,
             updated_at) VALUES ('ar', 'Artist', 'artist', 'person', 1000, 1000);
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source,
             created_at, updated_at) VALUES
             ('al', 'r1', 'g1', 'Album', 'album', 'Artist', 'artist', 'ar', 'automatic',
              1000, 1000);
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
             album_title, album_title_folded, disc_number, track_number, duration_seconds,
             file_format, availability, ingest_source, imported_at, membership_source) VALUES
             ('a', 'al', 'r1', '/m/a', 'a', 'ha', 1, 1, 'sa', 'A', 'a', 'Album', 'album',
              1, 1, 1.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic'),
             ('c', 'al', 'r1', '/m/c', 'c', 'hc', 1, 1, 'sc', 'C', 'c', 'Album', 'album',
              1, 2, 1.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic'),
             ('d', 'al', 'r1', '/m/d', 'd', 'hd', 1, 1, 'sd', 'D', 'd', 'Album', 'album',
              1, 3, 1.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic');
         INSERT INTO compat_bookmarks (user_id, file_id, position_ms, created_at, changed_at)
             VALUES ('u', 'a', 1, 0, 0), ('u', 'gone', 2, 0, 0);
         INSERT INTO compat_play_queues (user_id, current_index, updated_at)
             VALUES ('u', 1, 0), ('v', 0, 0);
         INSERT INTO compat_play_queue_items (user_id, item_index, file_id) VALUES
             ('u', 0, 'a'), ('u', 1, 'gone'), ('u', 2, 'c'), ('u', 3, 'd'),
             ('v', 0, 'a');",
        )
        .unwrap();
        prune_compat_orphans(conn).unwrap();
        let bookmarks: Vec<String> = column(conn, "SELECT file_id FROM compat_bookmarks");
        assert_eq!(bookmarks, ["a"]);
        let queue: Vec<String> = column(
            conn,
            "SELECT file_id FROM compat_play_queue_items WHERE user_id = 'u' ORDER BY item_index",
        );
        assert_eq!(queue, ["a", "c", "d"]);
        let indexes: Vec<i64> = column(
            conn,
            "SELECT item_index FROM compat_play_queue_items WHERE user_id = 'u' ORDER BY item_index",
        );
        assert_eq!(indexes, [0, 1, 2]);
        let current: Vec<Option<i64>> = column(
            conn,
            "SELECT current_index FROM compat_play_queues ORDER BY user_id",
        );
        // The next surviving track becomes current; other queues are untouched.
        assert_eq!(current, [Some(1), Some(0)]);
        Ok(())
    };
    store
        .write_once(Lane::Foreground, "test", |tx| check(tx))
        .unwrap();
}
