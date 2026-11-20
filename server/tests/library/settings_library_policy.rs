//! Library policy routes over a real catalog database: a removed root is
//! offered back with its path and restored through the revision-checked
//! save, also while a scan has marked all its files missing but its folder
//! is there, the save refuses to drop every root under a live catalog, and
//! the previews count catalog rows the way v2 did. The script preview
//! shows a script on the example albums as it is written.

use crate::common::ScratchDir;
use axum::http::StatusCode;

use droppedneedle::settings::models::{
    DropRemovedRootTracksRequest, LibraryPolicyApplyRequest, LibraryRestoreRootsRequest,
    LibrarySettingsSaveRequest,
};
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::runtime_config::secret_sections::IdentificationPolicy;
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
use serde_json::json;

#[tokio::test]
async fn removed_root_restores_and_previews_count_the_catalog() {
    let scratch = ScratchDir::new("settings-library-policy");
    let music = scratch.join("music");
    std::fs::create_dir_all(music.join("A")).expect("music dir builds");
    let runtime = crate::common::scratch_runtime(&scratch.join("app.db")).await;
    // Two catalog rows under a root the settings no longer list; one of
    // them went missing on disk.
    let seed = format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
           VALUES ('ar1', 'A', 'a', 'person', 0, 0);
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
           album_artist_id, grouping_source, created_at, updated_at)
           VALUES ('al1', 'r-old', 'g', 'T', 't', 'ar1', 'automatic', 0, 0);
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
           path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
           album_title, album_title_folded, file_format, ingest_source, imported_at,
           membership_source, availability)
           VALUES ('t1', 'al1', 'r-old', '{root}/A/one.flac', 'A/one.flac', 'h1', 1, 0, 's',
             'One', 'one', 'T', 't', 'flac', 'scan', 0, 'automatic', 'indexed'),
                  ('t2', 'al1', 'r-old', '{root}/A/two.flac', 'A/two.flac', 'h2', 1, 0, 's',
             'Two', 'two', 'T', 't', 'flac', 'scan', 0, 'automatic', 'missing');",
        root = music.display()
    );
    runtime
        .db()
        .write(Lane::Foreground, "policy seed", move |tx| {
            tx.execute_batch(&seed).map_err(DbError::from)
        })
        .await
        .expect("catalog seeds");

    let platform = crate::common::TestPlatform::on(runtime.db(), &scratch)
        .build()
        .await;
    let settings = crate::common::bundles::settings_closed(&platform);
    let policy = settings.library_policy();

    let offered = policy.restorable_roots().await.expect("restorable reads");
    assert_eq!(offered.restorable_roots.len(), 1);
    assert_eq!(offered.restorable_roots[0].root_id, "r-old");
    assert_eq!(
        offered.restorable_roots[0].path,
        music.to_string_lossy().as_ref()
    );
    assert_eq!(offered.restorable_roots[0].indexed_file_count, 2);
    let revision = offered.policy_revision;

    // Saving with no roots would orphan the catalog; a stale revision is
    // still a 409 first.
    let empty = |revision: &str| -> LibrarySettingsSaveRequest {
        serde_json::from_value(json!({
            "settings": {"library_roots": []},
            "expected_policy_revision": revision,
        }))
        .expect("request decodes")
    };
    let stale = policy.save(empty("stale")).await.expect_err("stale fails");
    assert!(matches!(stale, SettingsError::StaleRevision { .. }));
    let refused = policy
        .save(empty(&revision))
        .await
        .expect_err("guard refuses");
    assert!(matches!(refused, SettingsError::InvalidInput { .. }));

    let stale = policy
        .restore_roots(LibraryRestoreRootsRequest {
            expected_policy_revision: "stale".to_owned(),
            paths: None,
        })
        .await
        .expect_err("stale restore fails");
    assert!(matches!(stale, SettingsError::StaleRevision { .. }));

    let restored = policy
        .restore_roots(LibraryRestoreRootsRequest {
            expected_policy_revision: revision,
            paths: None,
        })
        .await
        .expect("restores");
    let roots = &restored.settings.library_roots;
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].id, "r-old");
    assert_eq!(roots[0].label, "music");
    assert_eq!(roots[0].policy, IdentificationPolicy::Automatic);
    let offered = policy.restorable_roots().await.expect("restorable reads");
    assert!(offered.restorable_roots.is_empty());

    // The tree counts indexed files only; the apply preview counts every
    // row the reconcile would revisit, missing ones included.
    let tree = policy.policy_tree().await.expect("tree reads");
    assert_eq!(tree.roots[0].indexed_file_count, Some(1));
    assert_eq!(tree.roots[0].on_disk_file_count, Some(1));
    let preview = policy
        .preview_apply(LibraryPolicyApplyRequest {
            scope_ids: Vec::new(),
            expected_policy_revision: restored.policy_revision.clone(),
        })
        .await
        .expect("apply preview reads");
    assert_eq!(preview.estimated_file_count, 2);
    let unknown = policy
        .preview_apply(LibraryPolicyApplyRequest {
            scope_ids: vec!["nope".to_owned()],
            expected_policy_revision: restored.policy_revision,
        })
        .await
        .expect_err("unknown scope fails");
    assert!(matches!(unknown, SettingsError::InvalidInput { .. }));

    // A second removed root whose rows a scan marked all missing is still
    // offered while its folder is on disk.
    let back = scratch.join("back");
    std::fs::create_dir_all(back.join("A")).expect("second music dir builds");
    let seed = format!(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
           album_artist_id, grouping_source, created_at, updated_at)
           VALUES ('al2', 'r-back', 'g2', 'T', 't', 'ar1', 'automatic', 0, 0);
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
           path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
           album_title, album_title_folded, file_format, ingest_source, imported_at,
           membership_source, availability)
           VALUES ('t3', 'al2', 'r-back', '{root}/A/one.flac', 'A/one.flac', 'h3', 1, 0, 's',
             'One', 'one', 'T', 't', 'flac', 'scan', 0, 'automatic', 'missing');",
        root = back.display()
    );
    runtime
        .db()
        .write(Lane::Foreground, "missing root seed", move |tx| {
            tx.execute_batch(&seed).map_err(DbError::from)
        })
        .await
        .expect("missing root seeds");
    let offered = policy.restorable_roots().await.expect("restorable reads");
    assert_eq!(offered.restorable_roots.len(), 1, "all-missing root");
    assert_eq!(offered.restorable_roots[0].root_id, "r-back");
}

/// A changed import naming script is checked on save and refused with its
/// line and column; a good one is saved with the path it gives an example
/// track. A stored script that no longer passes never blocks a save that
/// leaves it alone.
#[tokio::test]
async fn an_import_naming_script_is_checked_only_when_changed() {
    let scratch = ScratchDir::new("settings-library-policy-naming");
    let runtime = crate::common::scratch_runtime(&scratch.join("app.db")).await;
    let platform = crate::common::TestPlatform::on(runtime.db(), &scratch)
        .build()
        .await;
    let settings = crate::common::bundles::settings_closed(&platform);
    let policy = settings.library_policy();
    let save = |revision: &str, script: &str, enabled: bool| -> LibrarySettingsSaveRequest {
        serde_json::from_value(json!({
            "settings": {"naming_script": script, "enabled": enabled},
            "expected_policy_revision": revision,
        }))
        .expect("request decodes")
    };
    let current = settings.service().get_library().await.expect("reads");
    assert_eq!(
        current.naming_example.as_deref(),
        Some("Radiohead/OK Computer (1997)/0102 Paranoid Android.flac")
    );

    let refused = policy
        .save(save(
            &current.policy_revision,
            "{albumartist}/{nope(title)}",
            true,
        ))
        .await
        .expect_err("a bad script is refused");
    let SettingsError::InvalidInput { message } = refused else {
        panic!("not a 400: {refused:?}");
    };
    assert!(message.contains("line 1, column"), "{message}");

    let saved = policy
        .save(save(
            &current.policy_revision,
            "{initial}/{albumartist}/{track:02d} {title}.{ext}",
            true,
        ))
        .await
        .expect("a good script saves");
    assert_eq!(
        saved.naming_example.as_deref(),
        Some("R/Radiohead/02 Paranoid Android.flac")
    );

    // A script stored before today's checks: a save that changes
    // something else goes through.
    let stored = settings
        .service()
        .update_library(|mut library| {
            library.naming_script = "{albumartist}/{unknown_name}/{title}.{ext}".to_owned();
            Ok(library)
        })
        .await
        .expect("stored as is");
    let kept = policy
        .save(save(
            &stored.policy_revision,
            "{albumartist}/{unknown_name}/{title}.{ext}",
            false,
        ))
        .await
        .expect("a save that leaves the script alone goes through");
    assert!(!kept.settings.enabled);
}

#[tokio::test]
async fn the_script_preview_shows_each_example_album_and_a_fix_for_a_misspelt_name() {
    let app = crate::common::TestApp::start("settings-script-preview").await;
    let admin = app.login_admin().await;
    let headers = [("authorization", admin.as_str())];
    let url = "/api/v3/settings/library/script-preview";
    let preview = |kind: &str, source: &str| {
        let body = json!({"kind": kind, "source": source});
        app.call("POST", url, &headers, Some(body))
    };
    let sample = |body: &serde_json::Value, name: &str| {
        body["samples"]
            .as_array()
            .and_then(|samples| samples.iter().find(|sample| sample["sample"] == name))
            .cloned()
            .unwrap_or_else(|| panic!("no {name} sample: {body}"))
    };

    let (status, body) = preview("import_naming", "{albumartist}/{titel}.{ext}").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["fault"]["line"], 1, "{body}");
    assert_eq!(body["fault"]["column"], 15, "{body}");
    assert_eq!(
        body["fault"]["suggestion"],
        json!({"found": "titel", "name": "title"})
    );
    assert_eq!(
        sample(&body, "one_disc")["values"]["title"],
        "Paranoid Android"
    );

    // The ready-made blocks: the disc number on a multi-disc album only,
    // and the album artist's disambiguation in brackets.
    let script = concat!(
        "{albumartist}{conditional(is_empty(albumartist_disambiguation), \"\", ",
        "concat(\" (\", albumartist_disambiguation, \")\"))}/",
        "{conditional(equals(total_discs, \"1\"), \"\", concat(pad(disc, 2), \"-\"))}",
        "{track:02d} {title}.{ext}"
    );
    let (status, body) = preview("import_naming", script).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["fault"].is_null(), "{body}");
    let path = |name: &str| sample(&body, name)["path"].clone();
    assert_eq!(path("one_disc"), "Radiohead/02 Paranoid Android.flac");
    assert_eq!(path("two_discs"), "The Smashing Pumpkins/02-02 Bodies.flac");
    assert_eq!(
        path("name_shared"),
        "Alice Cooper (1964-1975 American rock band)/01 Under My Wheels.mp3"
    );

    let (status, body) = preview("naming", "{albumartist}/{album_disambiguation}/{title}").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let one = sample(&body, "one_disc");
    assert_eq!(
        one["empty_name_parts"],
        json!(["Radiohead", "", "Paranoid Android"])
    );
    assert!(one["path"].is_null(), "{one}");
    assert_eq!(
        sample(&body, "two_discs")["path"],
        "The Smashing Pumpkins/remastered/Bodies"
    );

    let (status, body) = preview("tagging", "delete comment").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        sample(&body, "one_disc")["tags"],
        json!([{"name": "comment", "before": ["Ripped with EAC"], "after": []}])
    );

    // A tagging script reads the worked-out values and writes `publisher`
    // to the label.
    let (status, body) = preview(
        "tagging",
        "set comment = concat(year, \" \", artist_display)\nset publisher = \"Capitol\"",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        sample(&body, "one_disc")["tags"],
        json!([
            {"name": "comment", "before": ["Ripped with EAC"], "after": ["1997 Radiohead"]},
            {"name": "label", "before": ["Parlophone"], "after": ["Capitol"]},
        ])
    );

    // The example albums have no genre: `primary_genre` reads as absent,
    // so setting the genre from it changes nothing. An empty `set` leaves
    // the comment as it is.
    let (status, body) = preview("tagging", "set genre = primary_genre\nset comment = \"\"").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(sample(&body, "one_disc")["tags"], json!([]), "{body}");
    app.stop().await;
}

/// Removed roots listing shows both overlapping (non-restorable) and standalone
/// (restorable) removed roots with indexed tracks. Dropping tracks for an
/// overlapping removed root chunks across >500 tracks, marks only that root's
/// tracks missing, updates read-model triggers and catalog revision, clears
/// residual scan state, and preserves active roots and play history.
#[tokio::test]
async fn removed_roots_lists_overlapping_and_standalone_and_drops_tracks_in_chunks() {
    let scratch = ScratchDir::new("settings-removed-roots-drop");
    let active_dir = scratch.join("music");
    let standalone_dir = scratch.join("old-archive");
    std::fs::create_dir_all(active_dir.join("A")).expect("active dir");
    std::fs::create_dir_all(standalone_dir.join("B")).expect("standalone dir");

    let runtime = crate::common::scratch_runtime(&scratch.join("app.db")).await;
    let active_str = active_dir.to_string_lossy().into_owned();
    let standalone_str = standalone_dir.to_string_lossy().into_owned();

    runtime
        .db()
        .write(Lane::Foreground, "seed removed roots", move |tx| {
            tx.execute_batch(
                "INSERT INTO auth_users (id, display_name, created_at) VALUES ('u1', 'Admin', 1);
                 INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
                   VALUES ('ar1', 'Artist', 'artist', 'group', 0, 0);
                 INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
                   album_artist_id, grouping_source, created_at, updated_at)
                   VALUES ('al-active', 'r-active', 'g-active', 'Active', 'active', 'ar1', 'automatic', 0, 0),
                          ('al-overlap', 'r-old-overlap', 'g-overlap', 'Overlap', 'overlap', 'ar1', 'automatic', 0, 0),
                          ('al-standalone', 'r-standalone', 'g-standalone', 'Standalone', 'standalone', 'ar1', 'automatic', 0, 0);",
            )?;
            // 2 tracks on configured root r-active
            for n in 0..2 {
                tx.execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
                       path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
                       album_title, album_title_folded, file_format, ingest_source, imported_at,
                       membership_source, availability)
                     VALUES (?1, 'al-active', 'r-active', ?2, ?3, ?4, 1, 0, 's', 'T', 't',
                       'Active', 'active', 'flac', 'scan', 0, 'automatic', 'indexed')",
                    rusqlite::params![
                        format!("ta-{n}"),
                        format!("{active_str}/A/t{n}.flac"),
                        format!("A/t{n}.flac"),
                        format!("ha-{n}"),
                    ],
                )?;
            }
            // 505 tracks (> 500 chunk size) on removed overlapping root r-old-overlap
            for n in 0..505 {
                tx.execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
                       path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
                       album_title, album_title_folded, file_format, ingest_source, imported_at,
                       membership_source, availability)
                     VALUES (?1, 'al-overlap', 'r-old-overlap', ?2, ?3, ?4, 1, 0, 's', 'T', 't',
                       'Overlap', 'overlap', 'flac', 'scan', 0, 'automatic', 'indexed')",
                    rusqlite::params![
                        format!("to-{n:04}"),
                        format!("{active_str}/A/old-{n:04}.flac"),
                        format!("A/old-{n:04}.flac"),
                        format!("ho-{n:04}"),
                    ],
                )?;
            }
            // 3 tracks on standalone removed root r-standalone
            for n in 0..3 {
                tx.execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
                       path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
                       album_title, album_title_folded, file_format, ingest_source, imported_at,
                       membership_source, availability)
                     VALUES (?1, 'al-standalone', 'r-standalone', ?2, ?3, ?4, 1, 0, 's', 'T', 't',
                       'Standalone', 'standalone', 'flac', 'scan', 0, 'automatic', 'indexed')",
                    rusqlite::params![
                        format!("ts-{n}"),
                        format!("{standalone_str}/B/s{n}.flac"),
                        format!("B/s{n}.flac"),
                        format!("hs-{n}"),
                    ],
                )?;
            }
            // Play history + residual scan state on r-old-overlap
            tx.execute_batch(
                "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id,
                   artist_name, track_name, played_at)
                   VALUES ('p1', 'u1', 'to-0000', 'al-overlap', 'Artist', 'T', 100);
                 INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at)
                   VALUES ('r-old-overlap', '.', 100), ('r-active', '.', 100);
                 INSERT INTO library_scan_unjudged_tracks (local_track_id, held_at)
                   VALUES ('to-0000', 100);
                 INSERT INTO library_scan_mounts (root_id, relative_path)
                   VALUES ('r-old-overlap', 'Share');
                 INSERT INTO library_folder_digests (root_id, relative_path, digest, audio_files)
                   VALUES ('r-old-overlap', '.', 42, 505);",
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("seed succeeds");

    let platform = crate::common::TestPlatform::on(runtime.db(), &scratch)
        .build()
        .await;
    let settings = crate::common::bundles::settings_closed(&platform);
    // Configure r-active at active_dir with label "Music".
    let initial = settings.service().get_library().await.expect("get library");
    let saved = settings
        .library_policy()
        .save(
            serde_json::from_value(json!({
                "settings": {
                    "library_roots": [{
                        "id": "r-active",
                        "path": active_dir.to_string_lossy(),
                        "label": "Music",
                        "policy": "automatic",
                        "rules": []
                    }]
                },
                "expected_policy_revision": initial.policy_revision,
            }))
            .expect("save request"),
        )
        .await
        .expect("save r-active");

    let policy = settings.library_policy();
    // Restorable roots only shows r-standalone (r-old-overlap overlaps "Music").
    let restorable = policy.restorable_roots().await.expect("restorable");
    assert_eq!(restorable.restorable_roots.len(), 1);
    assert_eq!(restorable.restorable_roots[0].root_id, "r-standalone");

    // Removed roots lists BOTH r-old-overlap and r-standalone.
    let removed = policy.removed_roots().await.expect("removed roots");
    assert_eq!(removed.policy_revision, saved.policy_revision);
    assert_eq!(removed.removed_roots.len(), 2);
    let overlap = &removed.removed_roots[0];
    assert_eq!(overlap.root_id, "r-old-overlap");
    assert_eq!(overlap.indexed_track_count, 505);
    assert!(!overlap.restorable);
    assert_eq!(overlap.overlapping_root_label.as_deref(), Some("Music"));
    assert_eq!(
        overlap.path.as_deref(),
        Some(active_dir.to_string_lossy().as_ref())
    );
    let standalone = &removed.removed_roots[1];
    assert_eq!(standalone.root_id, "r-standalone");
    assert_eq!(standalone.indexed_track_count, 3);
    assert!(standalone.restorable);
    assert_eq!(standalone.overlapping_root_label, None);

    let rev_before: i64 = runtime
        .db()
        .read("rev before", |conn| {
            conn.query_row(
                "SELECT value FROM library_catalog_revision WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(DbError::from)
        })
        .await
        .expect("rev before");

    let dropped = policy
        .drop_removed_root_tracks(DropRemovedRootTracksRequest {
            root_id: "r-old-overlap".to_owned(),
            expected_policy_revision: removed.policy_revision.clone(),
        })
        .await
        .expect("drop tracks");
    assert_eq!(dropped.root_id, "r-old-overlap");
    assert_eq!(dropped.marked_missing_count, 505);

    let (
        overlap_missing,
        overlap_has_indexed,
        active_indexed,
        standalone_indexed,
        plays,
        held_old,
        held_active,
        unjudged_old,
        mounts_old,
        digests_old,
        rev_after,
    ): (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) = runtime
        .db()
        .read("verify drop", |conn| {
            let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
            Ok::<_, DbError>((
                count("SELECT COUNT(*) FROM local_tracks WHERE root_id = 'r-old-overlap' AND availability = 'missing' AND missing_at IS NOT NULL")?,
                count("SELECT has_indexed_track FROM local_albums WHERE id = 'al-overlap'")?,
                count("SELECT COUNT(*) FROM local_tracks WHERE root_id = 'r-active' AND availability = 'indexed'")?,
                count("SELECT COUNT(*) FROM local_tracks WHERE root_id = 'r-standalone' AND availability = 'indexed'")?,
                count("SELECT COUNT(*) FROM library_play_history WHERE id = 'p1'")?,
                count("SELECT COUNT(*) FROM library_scan_held_scopes WHERE root_id = 'r-old-overlap'")?,
                count("SELECT COUNT(*) FROM library_scan_held_scopes WHERE root_id = 'r-active'")?,
                count("SELECT COUNT(*) FROM library_scan_unjudged_tracks WHERE local_track_id = 'to-0000'")?,
                count("SELECT COUNT(*) FROM library_scan_mounts WHERE root_id = 'r-old-overlap'")?,
                count("SELECT COUNT(*) FROM library_folder_digests WHERE root_id = 'r-old-overlap'")?,
                count("SELECT value FROM library_catalog_revision WHERE singleton = 1")?,
            ))
        })
        .await
        .expect("verify");

    assert_eq!(overlap_missing, 505);
    assert_eq!(overlap_has_indexed, 0);
    assert_eq!(active_indexed, 2, "active root tracks untouched");
    assert_eq!(standalone_indexed, 3, "other removed root untouched");
    assert_eq!(plays, 1, "play history preserved");
    assert_eq!(held_old, 0);
    assert_eq!(held_active, 1, "active root hold untouched");
    assert_eq!(unjudged_old, 0);
    assert_eq!(mounts_old, 0);
    assert_eq!(digests_old, 0);
    assert!(rev_after > rev_before, "catalog revision bumped");

    // Removed roots list now only contains r-standalone.
    let after = policy.removed_roots().await.expect("removed after");
    assert_eq!(after.removed_roots.len(), 1);
    assert_eq!(after.removed_roots[0].root_id, "r-standalone");
}

/// Dropping removed-root tracks refuses stale policy revisions (409),
/// configured roots (400), and active or finishing scans (409).
#[tokio::test]
async fn drop_removed_root_tracks_enforces_revision_configured_and_active_scan_guards() {
    let scratch = ScratchDir::new("settings-removed-roots-guards");
    let active_dir = scratch.join("music");
    std::fs::create_dir_all(&active_dir).expect("active dir");

    let runtime = crate::common::scratch_runtime(&scratch.join("app.db")).await;
    let active_str = active_dir.to_string_lossy().into_owned();
    runtime
        .db()
        .write(Lane::Foreground, "seed guards", move |tx| {
            tx.execute_batch(&format!(
                "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
                   VALUES ('ar1', 'Artist', 'artist', 'group', 0, 0);
                 INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
                   album_artist_id, grouping_source, created_at, updated_at)
                   VALUES ('al1', 'r-old', 'g1', 'T', 't', 'ar1', 'automatic', 0, 0);
                 INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
                   path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
                   album_title, album_title_folded, file_format, ingest_source, imported_at,
                   membership_source, availability)
                   VALUES ('t1', 'al1', 'r-old', '{active_str}/a.flac', 'a.flac', 'h1', 1, 0, 's',
                     'One', 'one', 'T', 't', 'flac', 'scan', 0, 'automatic', 'indexed');"
            ))
            .map_err(DbError::from)
        })
        .await
        .expect("seed");

    let platform = crate::common::TestPlatform::on(runtime.db(), &scratch)
        .build()
        .await;
    let settings = crate::common::bundles::settings_closed(&platform);
    let initial = settings.service().get_library().await.expect("initial");
    let saved = settings
        .library_policy()
        .save(
            serde_json::from_value(json!({
                "settings": {
                    "library_roots": [{
                        "id": "r-active",
                        "path": active_dir.to_string_lossy(),
                        "label": "Music",
                        "policy": "automatic",
                        "rules": []
                    }]
                },
                "expected_policy_revision": initial.policy_revision,
            }))
            .expect("save request"),
        )
        .await
        .expect("save");
    let policy = settings.library_policy();

    // 1. Stale revision -> 409 StaleRevision.
    let stale = policy
        .drop_removed_root_tracks(DropRemovedRootTracksRequest {
            root_id: "r-old".to_owned(),
            expected_policy_revision: "stale-rev".to_owned(),
        })
        .await
        .expect_err("stale refused");
    assert!(matches!(stale, SettingsError::StaleRevision { .. }));

    // 2. Configured root -> 400 InvalidInput.
    let configured = policy
        .drop_removed_root_tracks(DropRemovedRootTracksRequest {
            root_id: "r-active".to_owned(),
            expected_policy_revision: saved.policy_revision.clone(),
        })
        .await
        .expect_err("configured refused");
    assert!(matches!(configured, SettingsError::InvalidInput { .. }));

    // 3. Active scan (`discovering`) -> 409 Conflict.
    runtime
        .db()
        .write(Lane::Foreground, "seed active scan", |tx| {
            tx.execute_batch(
                "INSERT INTO library_scan_runs (id, kind, trigger, state, phase, aggregate_scope,
                   queued_at, updated_at)
                 VALUES ('run-active', 'incremental', 'manual', 'discovering', 'discovering', 'all', 1, 1);",
            )
            .map_err(DbError::from)
        })
        .await
        .expect("seed active scan");
    let active_err = policy
        .drop_removed_root_tracks(DropRemovedRootTracksRequest {
            root_id: "r-old".to_owned(),
            expected_policy_revision: saved.policy_revision.clone(),
        })
        .await
        .expect_err("active scan refused");
    assert!(matches!(active_err, SettingsError::Conflict { .. }));

    // 4. Ended scan still owing end passes (`next_end_pass IS NOT NULL`) -> 409 Conflict.
    runtime
        .db()
        .write(Lane::Foreground, "seed finishing scan", |tx| {
            tx.execute_batch(
                "UPDATE library_scan_runs
                 SET state = 'completed', terminal_at = 2, next_end_pass = 'scan_end_held_scopes'
                 WHERE id = 'run-active';",
            )
            .map_err(DbError::from)
        })
        .await
        .expect("seed finishing scan");
    let finishing_err = policy
        .drop_removed_root_tracks(DropRemovedRootTracksRequest {
            root_id: "r-old".to_owned(),
            expected_policy_revision: saved.policy_revision.clone(),
        })
        .await
        .expect_err("finishing scan refused");
    assert!(matches!(finishing_err, SettingsError::Conflict { .. }));

    // 5. Once end passes clear, drop succeeds.
    runtime
        .db()
        .write(Lane::Foreground, "clear end pass", |tx| {
            tx.execute_batch(
                "UPDATE library_scan_runs SET next_end_pass = NULL WHERE id = 'run-active';",
            )
            .map_err(DbError::from)
        })
        .await
        .expect("clear end pass");
    let ok = policy
        .drop_removed_root_tracks(DropRemovedRootTracksRequest {
            root_id: "r-old".to_owned(),
            expected_policy_revision: saved.policy_revision,
        })
        .await
        .expect("drop succeeds once scan finishes");
    assert_eq!(ok.marked_missing_count, 1);
}
