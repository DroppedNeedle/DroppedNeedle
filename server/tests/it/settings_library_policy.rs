//! Library policy routes over a real catalog database: a removed root is
//! offered back with its path and restored through the revision-checked
//! save, the save refuses to drop every root under a live catalog, and
//! the previews count catalog rows the way v2 did.

use crate::common::{FIXED_ID, FixedIdGenerator, ScratchDir};

use std::sync::Arc;

use droppedneedle::auth::users::memory::TestRig;
use droppedneedle::db::{DbConfig, Lane, OpError, open_runtime};
use droppedneedle::runtime_config::secret_sections::IdentificationPolicy;
use droppedneedle::settings::error::SettingsError;
use droppedneedle::settings::library_catalog::SqliteLibraryPolicyCatalog;
use droppedneedle::settings::models::{
    LibraryPolicyApplyRequest, LibraryRestoreRootsRequest, LibrarySettingsSaveRequest,
};
use droppedneedle::settings::wiring::SettingsSetup;
use serde_json::json;

#[tokio::test]
async fn removed_root_restores_and_previews_count_the_catalog() {
    let scratch = ScratchDir::new("settings-library-policy");
    let music = scratch.join("music");
    std::fs::create_dir_all(music.join("A")).expect("music dir builds");
    let runtime = open_runtime(&DbConfig::new(&scratch.join("app.db")))
        .await
        .expect("runtime opens");
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
        .lane()
        .write(Lane::Foreground, "policy seed", move |tx| {
            tx.execute_batch(&seed).map_err(OpError::from)?;
            Ok(())
        })
        .await
        .expect("catalog seeds");

    let rig = TestRig::new().expect("rig builds");
    let settings =
        SettingsSetup::for_tests(Arc::new(FixedIdGenerator::new(FIXED_ID)), rig.deps.clone())
            .expect("settings bundle builds")
            .with_library_catalog(Arc::new(SqliteLibraryPolicyCatalog {
                pool: runtime.pool().clone(),
            }));
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

    // Saving with no roots would orphan the catalog.
    let empty: LibrarySettingsSaveRequest = serde_json::from_value(json!({
        "settings": {"library_roots": []},
        "expected_policy_revision": revision,
    }))
    .expect("request decodes");
    let refused = policy.save(empty).await.expect_err("guard refuses");
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

    let mapping = policy.path_mapping().await.expect("mapping reads");
    assert_eq!((mapping.source_count, mapping.mapped_count), (2, 2));
    assert!(!mapping.blocking);
    assert_eq!(
        mapping.items[0].relative_path.as_deref(),
        Some("A/one.flac")
    );
}
