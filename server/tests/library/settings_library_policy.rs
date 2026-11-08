//! Library policy routes over a real catalog database: a removed root is
//! offered back with its path and restored through the revision-checked
//! save, also while a scan has marked all its files missing but its folder
//! is there, the save refuses to drop every root under a live catalog, and
//! the previews count catalog rows the way v2 did. The script preview
//! shows a script on the example albums as it is written.

use crate::common::ScratchDir;
use axum::http::StatusCode;

use droppedneedle::settings::models::{
    LibraryPolicyApplyRequest, LibraryRestoreRootsRequest, LibrarySettingsSaveRequest,
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
