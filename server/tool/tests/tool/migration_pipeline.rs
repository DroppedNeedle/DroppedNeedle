//! Migration pipeline end-to-end briefs: a live v2 fixture dir travels export →
//! validator → dry-run → import, then the migrated instance proves itself.
//!
//! Each brief builds its own scratch fixture: refusal of the orphaned
//! export; one journey over the repaired export (counts, follows, idempotent
//! re-import, compat re-auth, login with v2 passwords rehashing to
//! Argon2id); settings landed; and the v2 shapes that need a fixture of
//! their own. Nothing here touches the network; every database is a scratch
//! file.

use crate::common::Probe;
use std::path::{Path, PathBuf};

use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::{Crypto, Secret};
use droppedneedle_tool::test_support::fixture::{
    UNKNOWN_USER_ID, build_v2_fixture, delete_orphan_rows,
};

/// Scratch directory per brief, removed on drop.
fn scratch_dir(tag: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(&format!("pipeline-{tag}"))
}

fn export_request(v2_root: &Path, passphrase: &str) -> droppedneedle_tool::export::ExportRequest {
    droppedneedle_tool::export::ExportRequest {
        v2: droppedneedle_tool::export::v2dir::V2Dirs::under_root(v2_root),
        db_path: None,
        passphrase: Secret::new(passphrase.to_owned()),
        exported_at: None,
        v2_commit: None,
    }
}

fn issue_codes(report: &droppedneedle_tool::r#import::ValidationReport) -> (Vec<&str>, Vec<&str>) {
    (
        report.errors.iter().map(|issue| issue.code).collect(),
        report.warnings.iter().map(|issue| issue.code).collect(),
    )
}

const PASSPHRASE: &str = "operator-passphrase";

/// A fixture plus its first export, orphans included.
fn orphaned_export(root: &Path) -> (droppedneedle_tool::test_support::fixture::V2Fixture, String) {
    let v2_root = root.join("v2");
    let fixture = build_v2_fixture(&v2_root).expect("fixture builds");
    let export_path = root.join("export.json");
    droppedneedle_tool::export::export_v2_to_file(
        &export_request(&v2_root, PASSPHRASE),
        &export_path,
    )
    .expect("export writes");
    let text = std::fs::read_to_string(&export_path).expect("export reads back");
    (fixture, text)
}

/// A fixture plus its export after the operator repair: the orphan rows
/// are dropped from v2 and the file is re-exported clean.
fn repaired_export(root: &Path) -> (droppedneedle_tool::test_support::fixture::V2Fixture, String) {
    let (fixture, _) = orphaned_export(root);
    let export_path = root.join("export.json");
    assert_eq!(
        delete_orphan_rows(&root.join("v2").join("cache").join("library.db")).expect("repair runs"),
        2
    );
    droppedneedle_tool::export::export_v2_to_file(
        &export_request(&root.join("v2"), PASSPHRASE),
        &export_path,
    )
    .expect("re-export writes");
    let text = std::fs::read_to_string(&export_path).expect("re-export reads back");
    (fixture, text)
}

/// A migrated scratch v3 target with the conflict seeds: strangers holding
/// both fixture emails, and a Plex binding under a third account. Seeded
/// straight into the file: the offline import owns its writes while the
/// server is stopped.
fn seeded_target(root: &Path) -> (PathBuf, PathBuf) {
    let v3_root = root.join("v3");
    std::fs::create_dir_all(v3_root.join("config")).expect("v3 config dir");
    std::fs::create_dir_all(v3_root.join("cache")).expect("v3 cache dir");
    let db_path = v3_root.join("cache").join("library.db");
    droppedneedle_kernel::db::migrate_file(&db_path, &droppedneedle_kernel::schema::CHAIN)
        .expect("scratch schema migrates");
    seed_conflicts(&db_path);
    (v3_root, db_path)
}

#[tokio::test]
async fn orphaned_export_refuses_with_zero_writes() {
    let root = scratch_dir("refuses");
    let (_fixture, text) = orphaned_export(&root);
    let parsed = droppedneedle_tool::export::parse_export(&text).expect("envelope parses");
    assert_eq!(parsed.doc.users.len(), 2);
    assert_eq!(parsed.doc.follows.len(), 4);
    assert_eq!(parsed.doc.approvals.len(), 4);

    let root_value: serde_json::Value = serde_json::from_str(&text).expect("json parses");
    let validation = droppedneedle_tool::r#import::validate_export(&root_value);
    assert!(!validation.valid());
    let (errors, warnings) = issue_codes(&validation);
    assert!(errors.contains(&"DANGLING_USER_REF"), "errors: {errors:?}");
    assert!(
        warnings.contains(&"DANGLING_REVIEWER"),
        "warnings: {warnings:?}"
    );
    assert!(
        warnings.contains(&"REVOKED_APP_PASSWORD_KEPT"),
        "warnings: {warnings:?}"
    );

    let (v3_root, target_db) = seeded_target(&root);
    let counts_before = table_counts(&target_db);
    for dry_run in [true, false] {
        let report = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, dry_run).await;
        assert_eq!(
            report.exit.code,
            droppedneedle_tool::r#import::ExitCode::FailedValidation,
            "dry_run={dry_run}"
        );
    }
    assert_eq!(table_counts(&target_db), counts_before);
    assert!(!v3_root.join("config").join("config.json").exists());
}

/// The repaired fixture migrates whole: it validates with only warnings, a
/// dry run writes nothing and matches the import, every record lands with
/// its counts, reviewers and follows, a re-import writes nothing, and the
/// v2 secrets and passwords sign in.
#[tokio::test]
async fn a_repaired_v2_fixture_migrates_whole() {
    let root = scratch_dir("repaired");
    let (fixture, text) = repaired_export(&root);

    // The operator repair converges to a clean file.
    let root_value: serde_json::Value = serde_json::from_str(&text).expect("json parses");
    let validation = droppedneedle_tool::r#import::validate_export(&root_value);
    assert!(validation.valid(), "errors: {:?}", validation.errors);
    let (_, warnings) = issue_codes(&validation);
    assert!(warnings.contains(&"DANGLING_REVIEWER"));

    // A dry run writes nothing and reports what the import then does.
    let (v3_root, target_db) = seeded_target(&root);
    let counts_before = table_counts(&target_db);
    let dry_report = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, true).await;
    assert_eq!(
        dry_report.exit.code,
        droppedneedle_tool::r#import::ExitCode::OkWithDrops
    );
    assert_eq!(table_counts(&target_db), counts_before);
    assert!(!v3_root.join("config").join("config.json").exists());
    let report = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    assert_eq!(
        report.exit.code,
        droppedneedle_tool::r#import::ExitCode::OkWithDrops
    );
    assert_eq!(report.entities, dry_report.entities, "dry-run parity");
    assert_eq!(
        report.secrets_reencrypted, dry_report.secrets_reencrypted,
        "dry-run parity"
    );

    // Counts: two users in (both emails nulled onto the pre-seeded
    // strangers), two local providers in, the Plex binding kept on its
    // existing owner, three app passwords, three follows, three approvals
    // with one reviewer nulled.
    // v2's ISO times read back as the same instants, in milliseconds.
    let alice: (i64, Option<i64>) = target_db.row(
        "SELECT created_at, last_login_at FROM auth_users WHERE id = ?",
        rusqlite::params![&fixture.alice_id],
    );
    assert_eq!(alice, (1_704_067_200_000, Some(1_767_225_600_000)));
    let count = |entity: &str| report.entities.get(entity).expect("entity counted").clone();
    assert_eq!(count("user").imported, 2);
    assert_eq!(count("user").nulled_field, 2);
    assert_eq!(count("provider").imported, 2);
    assert_eq!(count("provider").conflict_kept_existing, 1);
    assert_eq!(count("app_password").imported, 3);
    assert_eq!(count("follow").imported, 3);
    assert_eq!(count("approval").imported, 3);
    assert_eq!(count("approval").nulled_field, 1);
    assert_eq!(count("event_city").imported, 2);
    assert_eq!(count("event_seen").imported, 1);
    // Nine settings seals (slskd, sabnzbd, two indexers, AudioDB, one
    // plugin token, one plugin setting with no manifest, the Last.fm key
    // pair), three app passwords and four per-user connections; v2's
    // global Last.fm session key is dropped and counts none.
    assert_eq!(report.secrets_reencrypted, 16);
    // The v2 instance Last.fm key pair opens under the v3 key, so linking
    // and scrobbling keep working after the move.
    let store = v3_store(&v3_root);
    let lastfm = store
        .get_raw::<droppedneedle_kernel::runtime_config::secret_sections::LastFmSettings>()
        .expect("lastfm settings read");
    assert_eq!(lastfm.api_key.expose(), "lastfm-public-key");
    assert_eq!(lastfm.shared_secret.expose(), "lastfm-shared-plaintext");
    drop(store);

    let revoked: Vec<(String, i64)> = target_db.rows(
        "SELECT name, revoked FROM auth_app_passwords WHERE revoked = 1",
        [],
    );
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].0, "old-laptop");

    // No sessions survive the import itself: the file carries none.
    let tokens: i64 = target_db.one("SELECT COUNT(*) FROM auth_tokens", []);
    assert_eq!(tokens, 0);

    // Bob's approval was reviewed by Alice: a valid reviewer survives
    // verbatim.
    let kept: (Option<String>, Option<String>) = target_db.row(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals WHERE user_id = ?",
        rusqlite::params![&fixture.bob_id],
    );
    assert_eq!(
        kept,
        (Some(fixture.alice_id.clone()), Some("Alice".to_owned()))
    );

    // Alice's second approval was reviewed by a deleted user: the id nulls
    // and the name stays as audit residue.
    let nulled: (Option<String>, Option<String>) = target_db.row(
        "SELECT reviewed_by_id, reviewed_by_name FROM auto_download_approvals \
         WHERE user_id = ? AND artist_mbid = ?",
        rusqlite::params![&fixture.alice_id, &fixture.artist_two_mbid],
    );
    assert_eq!(nulled, (None, Some("Gone".to_owned())));

    assert_follows_intact(&target_db, &fixture);

    // A re-import writes nothing: no record, no settings row, no audit row.
    let runs: i64 = target_db.one("SELECT COUNT(*) FROM import_runs", []);
    assert_eq!(runs, 1);
    let rows_before = settings_revisions(&target_db);
    assert!(!rows_before.is_empty());
    let policy = &v3_settings(&v3_root)["download_policy"];
    assert_eq!(policy["quality_min"], "mp3_320");
    assert_eq!(policy["quality_max"], "lossless");
    assert_eq!(policy["quality_cutoff"], "lossless");
    let steps: Vec<(&str, &str)> = policy["quality_recipe"]
        .as_array()
        .expect("the policy holds a recipe")
        .iter()
        .map(|step| {
            (
                step["format"].as_str().expect("format"),
                step["quality"].as_str().expect("quality"),
            )
        })
        .collect();
    assert_eq!(steps, [("flac", "hi_res"), ("mp3", "320_plus")]);
    let line = "the v2 quality range mp3_320 to lossless is now the recipe FLAC, every \
                resolution, then MP3 320_plus";
    let again = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    for report in [&report, &again] {
        assert!(
            report
                .items
                .iter()
                .any(|item| item.key == "download_policy.quality_recipe" && item.detail == line),
            "{}",
            report.to_json()
        );
    }
    for (entity, counts) in &again.entities {
        assert_eq!(counts.imported, 0, "entity {entity} wrote nothing twice");
    }
    assert_eq!(
        settings_revisions(&target_db),
        rows_before,
        "no settings row changes on a re-import"
    );
    let runs_after: i64 = target_db.one("SELECT COUNT(*) FROM import_runs", []);
    assert_eq!(runs_after, 1, "a pure re-import writes no audit row");

    // Compat clients re-authenticate on the surviving secrets, then the v2
    // passwords log in. The login rehash rewrites the provider rows, so it
    // runs after the re-import check.
    let db = Db::open_for_tests(&v3_root.join("cache").join("library.db")).expect("database opens");
    assert_compat_reauth(&db, &v3_root, &fixture).await;
    login_rehashes(
        &target_db,
        &db,
        "alice",
        &fixture.alice_id,
        &fixture.secrets.alice_password,
    )
    .await;
    login_rehashes(
        &target_db,
        &db,
        "bob",
        &fixture.bob_id,
        &fixture.secrets.bob_password,
    )
    .await;
}

#[tokio::test]
async fn settings_land_with_schedule_carry_and_no_plaintext() {
    const SCRIPT: &str = "{albumartist}/{album}{conditional(is_empty(slice(default(original_date, \
                          date), 0, 4)), \"\", concat(\" (\", slice(default(original_date, date), \
                          0, 4), \")\"))}/{track:02d} {title}.{ext}";
    let root = scratch_dir("settings");
    let (fixture, _) = repaired_export(&root);
    let template = "{albumartist}/{album} ({year})/{track:02d} {title}";
    let text = export_with_naming_template(&root, template);
    let (v3_root, target_db) = seeded_target(&root);
    let dry = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, true).await;
    let report = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    assert_settings_landed(&v3_root, &fixture);

    let library = &v3_settings(&v3_root)["library_settings"];
    assert_eq!(library["naming_script"], SCRIPT);
    assert!(library.get("naming_template").is_none());
    for (year, golden) in [
        (
            Some("1994-08-22"),
            "Portishead/Dummy (1994)/03 Sour Times.flac",
        ),
        (None, "Portishead/Dummy/03 Sour Times.flac"),
    ] {
        assert_eq!(render_import_path(SCRIPT, year), golden);
    }

    let rows_before = settings_revisions(&target_db);
    let again = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    assert_eq!(settings_revisions(&target_db), rows_before);
    let line =
        format!("the v2 naming template {template} is now the import naming script {SCRIPT}");
    for report in [&dry, &report, &again] {
        assert!(
            report
                .items
                .iter()
                .any(|item| item.key == "library_settings.naming_script" && item.detail == line),
            "{}",
            report.to_json()
        );
    }

    let stored = database_bytes(&v3_root);
    for secret in [
        fixture.secrets.slskd_key.as_str(),
        fixture.secrets.audiodb_key.as_str(),
        fixture.secrets.indexer_keys[0].as_str(),
        fixture.secrets.indexer_keys[1].as_str(),
        fixture.secrets.plugin_token.as_str(),
        fixture.secrets.alice_phone_secret.as_str(),
        fixture.secrets.alice_old_secret.as_str(),
        fixture.secrets.bob_legacy_secret.as_str(),
    ] {
        assert!(
            !stored.contains(secret),
            "the v3 database leaks a plaintext secret"
        );
    }
}

/// A template the script language can't place a file with lands as the
/// default script, with a line naming it, and the import goes on.
#[tokio::test]
async fn a_naming_template_that_cant_be_carried_falls_back_to_the_default() {
    use droppedneedle_kernel::runtime_config::secret_sections::DEFAULT_NAMING_SCRIPT;

    let root = scratch_dir("naming-fallback");
    let _ = repaired_export(&root);
    let text = export_with_naming_template(&root, "{album}/.{ext}");
    let (v3_root, target_db) = seeded_target(&root);
    let report = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    let library = &v3_settings(&v3_root)["library_settings"];
    assert_eq!(library["naming_script"], DEFAULT_NAMING_SCRIPT);
    assert!(library.get("naming_template").is_none());
    assert!(
        report.items.iter().any(|item| {
            item.key == "library_settings.naming_template"
                && item.outcome == "dropped_setting"
                && item.detail.contains("{album}/.{ext}")
        }),
        "{}",
        report.to_json()
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("{album}/.{ext}")),
        "{:?}",
        report.notes
    );
}

/// A top-level Jellyfin address and contact address reach the person: the
/// first in v3's settings, the second as a line in the import report, which
/// lists the config keys left behind.
#[tokio::test]
async fn top_level_config_keys_reach_the_import() {
    let root = scratch_dir("config-keys");
    let _ = repaired_export(&root);
    let config_path = root.join("v2").join("config").join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).expect("config reads"))
            .expect("config parses");
    config["jellyfin_url"] = "http://jellyfin.lan:8096".into();
    config["contact_email"] = "ops@example.org".into();
    std::fs::write(&config_path, config.to_string()).expect("config writes");
    let export_path = root.join("edited.json");
    droppedneedle_tool::export::export_v2_to_file(
        &export_request(&root.join("v2"), PASSPHRASE),
        &export_path,
    )
    .expect("export writes");
    let text = std::fs::read_to_string(&export_path).expect("export reads back");

    let (v3_root, target_db) = seeded_target(&root);
    let report = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    assert_eq!(
        v3_settings(&v3_root)["jellyfin_settings"]["jellyfin_url"],
        "http://jellyfin.lan:8096"
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.starts_with("Set CONTACT_EMAIL=ops@example.org in your environment")),
        "{:?}",
        report.notes
    );
    assert!(report.left_behind.iter().any(|item| {
        item.table == "contact_email"
            && item.kind == droppedneedle_tool::export::LeftBehindKind::Setting
    }));
}

/// The user data beyond accounts: every section lands with its v2 library
/// references settled against the carried catalog, a duplicate v2 playlist is counted
/// instead of breaking the import, sealed connections open under the v3
/// key, an interrupted carry resumes where it stopped, and a repeat import
/// writes nothing at all and brings back nothing the user deleted.
#[tokio::test]
async fn user_data_carries_resumes_and_repeats_as_noop() {
    use droppedneedle_tool::test_support::fixture::{AVATAR_BYTES, COVER_BYTES, PLAYLIST_ID};

    let root = scratch_dir("carry");
    let (fixture, text) = repaired_export(&root);
    let (v3_root, target_db) = seeded_target(&root);
    let rows = |table: &'static str| {
        let target_db = target_db.clone();
        async move { target_db.one::<i64>(&format!("SELECT COUNT(*) FROM {table}"), []) }
    };

    // A crash after three sections keeps them; the rest has not landed.
    // The second playlist from the same source is a conflict, and its entry
    // is left out with a count rather than failing the section.
    let crashed = run_pipeline_with(&text, PASSPHRASE, &v3_root, &target_db, false, Some(3)).await;
    assert_eq!(
        crashed.exit.code,
        droppedneedle_tool::r#import::ExitCode::FailedInternal
    );
    assert_eq!(rows("user_connections").await, 4);
    assert_eq!(rows("catalog_playlists").await, 2);
    assert_eq!(rows("catalog_playlist_tracks").await, 2);
    assert_eq!(rows("library_play_history").await, 0);
    let playlists = &crashed.entities["playlist"];
    assert_eq!(
        (playlists.imported, playlists.conflict_kept_existing),
        (2, 1)
    );
    let entries = &crashed.entities["playlist_track"];
    assert_eq!((entries.imported, entries.dropped_invalid), (2, 1));
    // The failed run's audit row holds the sections it did commit.
    let (exit, counts): (String, String) =
        target_db.row("SELECT exit_code, entity_counts FROM import_runs", []);
    assert_eq!(exit, "FAILED_INTERNAL");
    let counts: serde_json::Value = serde_json::from_str(&counts).expect("counts json");
    assert_eq!(counts["playlist_track"]["imported"], 2);

    let resumed = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    assert_eq!(
        resumed.exit.code,
        droppedneedle_tool::r#import::ExitCode::OkWithDrops,
        "{:?}",
        resumed.exit
    );
    let skipped: Vec<&str> = resumed
        .items
        .iter()
        .filter(|item| item.outcome == "already_applied")
        .map(|item| item.key.as_str())
        .collect();
    assert_eq!(skipped, ["connection", "playlist", "playlist_track"]);
    for (table, expected) in [
        ("catalog_playlists", 2),
        ("catalog_playlist_covers", 1),
        ("catalog_user_favorites", 2),
        ("library_play_history", 2),
        ("request_history", 1),
        ("request_history_requesters", 1),
        ("request_history_dismissals", 1),
        ("download_tasks", 1),
        ("download_attempts", 1),
        ("catalog_user_favorite_names", 2),
        ("wanted_watches", 2),
        ("wanted_seen_candidates", 1),
        ("user_quotas", 1),
        ("download_quarantine", 1),
        ("user_listening_prefs", 1),
        ("personal_mix_approvals", 1),
        ("app_section_prefs", 1),
        ("media_navidrome_folder_preferences", 1),
        ("catalog_new_release_seen_marks", 1),
        ("compat_play_queues", 1),
        ("compat_play_queue_items", 2),
        ("compat_bookmarks", 1),
        ("artist_known_releases", 1),
        ("acquire_new_release_feed", 1),
    ] {
        assert_eq!(rows(table).await, expected, "{table}");
    }
    // Library ids are settled once the catalog lands after the user data:
    // the history row now names the carried track, and only the queued
    // track v2 itself no longer had still waits.
    assert_eq!(rows("import_pending_links").await, 1);
    // v2's ISO request times land as milliseconds, and its dormant watch
    // lands watching and due.
    let request: (i64, i64) = target_db.row(
        "SELECT h.requested_at, r.requested_at FROM request_history h \
         JOIN request_history_requesters r USING (musicbrainz_id_lower)",
        [],
    );
    assert_eq!(request, (1_700_000_000_250, 1_700_000_000_250));
    // v2's REAL-second download, quarantine, seen and approval times land
    // as milliseconds.
    let times: (i64, i64, i64, i64, i64) = target_db.row(
        "SELECT (SELECT created_at FROM download_tasks), \
         (SELECT updated_at FROM download_attempts), \
         (SELECT quarantined_at FROM download_quarantine), \
         (SELECT first_seen_at FROM wanted_seen_candidates), \
         (SELECT requested_at FROM personal_mix_approvals)",
        [],
    );
    assert_eq!(
        times,
        (
            1_700_000_100_000,
            1_700_000_400_000,
            1_700_000_200_000,
            1_700_000_300_000,
            1_700_000_000_000
        )
    );
    // v2's check outcomes land in v3's spelling, which the wanted view reads.
    let outcomes: Vec<Option<String>> = target_db.column(
        "SELECT last_outcome FROM wanted_watches ORDER BY release_group_mbid_lower",
        [],
    );
    assert_eq!(
        outcomes,
        [Some("seen".to_owned()), Some("quiet".to_owned())]
    );
    let dormant: (String, i64, i64) = target_db.row(
        "SELECT state, created_at, next_check_at FROM wanted_watches \
         WHERE release_group_mbid_lower = ?",
        rusqlite::params![droppedneedle_tool::test_support::fixture::DORMANT_WATCH_MBID],
    );
    assert_eq!(
        (dormant.0.as_str(), dormant.1),
        ("watching", 1_600_000_000_500)
    );
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970")
        .as_millis();
    assert!(
        i128::from(dormant.2) <= i128::try_from(now_ms).expect("now fits"),
        "the dormant watch is due now"
    );
    let history: Option<String> = target_db.one(
        "SELECT local_track_id FROM library_play_history WHERE id = 'listen-1'",
        [],
    );
    assert_eq!(
        history.as_deref(),
        Some(droppedneedle_tool::test_support::fixture::V2_TRACK_ID)
    );
    let made: i64 = target_db.one(
        "SELECT created_at FROM catalog_playlists WHERE id = ?",
        rusqlite::params![PLAYLIST_ID],
    );
    assert_eq!(
        made, 1_748_736_000_000,
        "v2's ISO time lands in milliseconds"
    );
    let cover: Vec<u8> = target_db.one(
        "SELECT image FROM catalog_playlist_covers WHERE playlist_id = ?",
        rusqlite::params![PLAYLIST_ID],
    );
    assert_eq!(cover, COVER_BYTES);
    let avatar = v3_root
        .join("cache")
        .join("avatars")
        .join(format!("{}.png", fixture.alice_id));
    assert_eq!(std::fs::read(avatar).expect("avatar file"), AVATAR_BYTES);
    for (table, left) in [
        ("request_history", 1),
        ("download_tasks", 1),
        ("download_attempts", 2),
        ("auth_tokens", 1),
    ] {
        assert!(
            resumed
                .left_behind
                .iter()
                .any(|item| item.table == table && item.rows == left),
            "{table} listed as left behind"
        );
    }

    // Sealed links open under the v3 key, in the shapes v3 reads.
    let crypto = Crypto::load(&v3_root.join("config")).expect("v3 key loads");
    let link = |service: &'static str| {
        let target_db = target_db.clone();
        async move {
            target_db.one::<String>(
                "SELECT connection_data FROM user_connections WHERE service = ?",
                rusqlite::params![service],
            )
        }
    };
    let listenbrainz: serde_json::Value = serde_json::from_str(
        &crypto
            .decrypt(&link("listenbrainz").await)
            .expect("listenbrainz link opens"),
    )
    .expect("listenbrainz json");
    assert_eq!(
        listenbrainz["user_token"],
        fixture.secrets.listenbrainz_token.as_str()
    );
    // Spotify keeps v2's field names, sealed whole like the media servers,
    // and v3's Spotify store reads the carried link as it is.
    let spotify = {
        let db =
            Db::open_for_tests(&v3_root.join("cache").join("library.db")).expect("database opens");
        let key = Crypto::load(&v3_root.join("config")).expect("v3 key loads");
        droppedneedle_acquire::imports::spotify_store::SqliteSpotifyLinks::new(
            droppedneedle_acquire::db::AcquireDb::new(
                db,
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            ),
            std::sync::Arc::new(key),
        )
        .get(&fixture.bob_id)
        .await
        .expect("spotify link reads")
        .expect("spotify link carried")
    };
    assert_eq!(spotify.refresh_token, fixture.secrets.spotify_refresh);
    assert_eq!(spotify.spotify_user_id, "bob-sp-id");
    // 2025-06-01T01:00:00+00:00, v2's ISO expiry.
    assert_eq!(spotify.expires_at_unix, 1_748_739_600);
    let lastfm: serde_json::Value =
        serde_json::from_str(&link("lastfm").await).expect("last.fm doc");
    assert_eq!(lastfm["configured"], false);
    assert_eq!(
        crypto
            .decrypt(lastfm["session_key"].as_str().expect("session key"))
            .expect("session key opens"),
        fixture.secrets.lastfm_session
    );
    // A plugin setting sealed without a v2 manifest reads back plain.
    drop(crypto);
    let store = v3_store(&v3_root);
    let orphan = store
        .get_plugin_raw("orphan", &std::collections::HashSet::new())
        .expect("plugin reads")
        .into_settings();
    assert_eq!(
        orphan.get("mode").map(String::as_str),
        Some(fixture.secrets.orphan_plugin_mode.as_str())
    );

    // A repeat import writes nothing anywhere, and a playlist deleted in v3
    // since does not come back: each section is carried once.
    target_db.exec(
        "DELETE FROM catalog_playlists WHERE id = 'playlist-bob'",
        [],
    );
    let before = table_counts(&target_db);
    let again = run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    assert!(again.entities.values().all(|counts| counts.imported == 0));
    assert_eq!(table_counts(&target_db), before);
}

/// Pre-seed the target with the conflict cases: strangers holding both
/// fixture emails, and a Plex binding under a third account that collides
/// with Bob's.
fn seed_conflicts(target_db: &Path) {
    for (id, email, username) in [
        ("carol-id", "alice@example.com", "carol"),
        ("dave-id", "bob@example.com", "dave"),
    ] {
        target_db.exec(
            "INSERT INTO auth_users (id, display_name, email, role, created_at, username)
             VALUES (?, ?, ?, 'user', 1767225600000, ?)",
            rusqlite::params![id, username, email, username],
        );
    }
    target_db.exec(
        "INSERT INTO auth_users (id, display_name, role, created_at, username)
         VALUES ('mallory-id', 'Mallory', 'user', 1767225600000, 'mallory')",
        [],
    );
    target_db.exec(
        "INSERT INTO auth_providers (id, user_id, provider, provider_uid, provider_data,
                                     created_at)
         VALUES ('prov-mallory', 'mallory-id', 'plex', 'plex-uid-bob',
                 '{\"token\": \"mallory\"}', 1767225600000)",
        [],
    );
}

/// Row counts per migrated table, for the zero-writes proofs.
fn table_counts(target_db: &Path) -> Vec<(String, i64)> {
    let tables: Vec<String> = target_db.column(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
        [],
    );
    let mut counts = Vec::new();
    for table in tables {
        let total: i64 = target_db.one(&format!("SELECT COUNT(*) FROM \"{table}\""), []);
        counts.push((table, total));
    }
    counts
}

/// The settings stored in the target database, key by key.
fn v3_settings(v3_root: &Path) -> serde_json::Value {
    let db = rusqlite::Connection::open(v3_root.join("cache").join("library.db"))
        .expect("v3 database opens");
    let mut statement = db
        .prepare("SELECT key, value_json FROM config_sections")
        .expect("settings query prepares");
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("settings rows read");
    serde_json::Value::Object(
        rows.map(|row| {
            let (key, text) = row.expect("settings row reads");
            (key, serde_json::from_str(&text).expect("a row is JSON"))
        })
        .collect(),
    )
}

/// A settings store holding the target's settings under its key, for
/// reads through the store. The rows are copied in from a `config.json`
/// in a folder of its own, as a first start copies one.
fn v3_store(v3_root: &Path) -> droppedneedle_kernel::runtime_config::ConfigStore {
    let dir = v3_root.join("settings-read");
    std::fs::create_dir_all(&dir).expect("settings folder");
    std::fs::write(dir.join("config.json"), v3_settings(v3_root).to_string())
        .expect("settings copy writes");
    droppedneedle_kernel::runtime_config::ConfigStore::scratch(
        &dir,
        Crypto::load(&v3_root.join("config")).expect("v3 key loads"),
    )
    .expect("v3 settings open")
}

/// Every byte of the target database on disk, its WAL included.
fn database_bytes(v3_root: &Path) -> String {
    let db = v3_root.join("cache").join("library.db");
    let mut bytes = std::fs::read(&db).expect("v3 database reads");
    bytes.extend(std::fs::read(v3_root.join("cache").join("library.db-wal")).unwrap_or_default());
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Export the v2 fixture again with `template` as its library naming template.
fn export_with_naming_template(root: &Path, template: &str) -> String {
    let config_path = root.join("v2").join("config").join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).expect("config reads"))
            .expect("config parses");
    config["library_settings"]["naming_template"] = template.into();
    std::fs::write(&config_path, config.to_string()).expect("config writes");
    let export_path = root.join("edited.json");
    droppedneedle_tool::export::export_v2_to_file(
        &export_request(&root.join("v2"), PASSPHRASE),
        &export_path,
    )
    .expect("export writes");
    std::fs::read_to_string(&export_path).expect("export reads back")
}

/// The path import placement gives Portishead's "Sour Times" under `script`.
fn render_import_path(script: &str, date: Option<&str>) -> String {
    use droppedneedle_library::import::naming;
    use droppedneedle_library::matching::model::{CreditedArtist, ReleaseMedium};
    use droppedneedle_library::matching::{Release, ReleaseTrack};
    use droppedneedle_library::tags::read::HeaderInfo;

    let track = ReleaseTrack {
        title: "Sour Times".to_owned(),
        disc: 1,
        position: 3,
        ..ReleaseTrack::default()
    };
    let release = Release {
        release_group_id: "rg-id".to_owned(),
        title: "Dummy".to_owned(),
        artists: vec![CreditedArtist {
            id: "aa-id".to_owned(),
            name: "Portishead".to_owned(),
            sort_name: None,
            join: String::new(),
            disambiguation: None,
        }],
        date: date.map(str::to_owned),
        original_date: date.map(str::to_owned),
        media: vec![ReleaseMedium {
            position: 1,
            track_count: 11,
            ..ReleaseMedium::default()
        }],
        ..Release::default()
    };
    naming::render(
        script,
        &naming::variables(&release, &track, "flac", &HeaderInfo::default()),
        "/music",
    )
    .expect("the script places the file")
}

/// Every settings row's key, revision and change time.
fn settings_revisions(target_db: &Path) -> Vec<(String, i64, i64)> {
    target_db.rows(
        "SELECT key, revision, updated_at FROM config_sections ORDER BY key",
        [],
    )
}

/// Drive the real import pipeline (dry-run or real) over the scratch target.
async fn run_pipeline(
    export_text: &str,
    passphrase: &str,
    v3_root: &Path,
    target_db: &Path,
    dry_run: bool,
) -> droppedneedle_tool::r#import::ImportReport {
    run_pipeline_with(export_text, passphrase, v3_root, target_db, dry_run, None).await
}

/// [`run_pipeline`] with a simulated crash after some carried sections.
async fn run_pipeline_with(
    export_text: &str,
    passphrase: &str,
    v3_root: &Path,
    target_db: &Path,
    dry_run: bool,
    fault_after_sections: Option<usize>,
) -> droppedneedle_tool::r#import::ImportReport {
    let config_dir = v3_root.join("config");
    let crypto = if dry_run {
        Crypto::from_key_bytes(&[9u8; 32]).expect("dry key builds")
    } else {
        Crypto::load_or_generate(&config_dir)
            .expect("v3 key loads")
            .0
    };
    droppedneedle_tool::r#import::run_import(droppedneedle_tool::r#import::ImportRequest {
        export_bytes: export_text.as_bytes().to_vec(),
        passphrase: passphrase.to_owned(),
        db_path: target_db.to_owned(),
        legacy: droppedneedle_kernel::runtime_config::LegacyConfig::at(&config_dir, false),
        crypto,
        v2_config_path: Some(
            v3_root
                .join("..")
                .join("v2")
                .join("config")
                .join("config.json"),
        ),
        attachments_dir: Some(v3_root.join("..")),
        cache_dir: Some(v3_root.join("cache")),
        dry_run,
        fault_before_commit: false,
        fault_after_commit: false,
        fault_after_sections,
    })
    .await
}

/// Held downloads carry into the held list with their files: a track
/// download's under its song, an album's under its album, v2's library
/// holds as a taken destination and a reason v3 holds no file for as held
/// by an earlier version, each kept at least 90 days after the carry.
/// Decided rows stay behind, and a repeat import adds nothing.
#[tokio::test]
async fn held_downloads_carry_into_the_held_list() {
    let root = scratch_dir("held");
    let (fixture, _) = orphaned_export(&root);
    let v2_cache = root.join("v2").join("cache");
    let v2_db = v2_cache.join("library.db");
    delete_orphan_rows(&v2_db).expect("repair runs");
    let held_dir = v2_cache.join("held");
    std::fs::create_dir_all(&held_dir).expect("v2 held folder");
    std::fs::write(held_dir.join("album.flac"), b"album bytes").expect("held file");
    std::fs::write(held_dir.join("track.flac"), b"track bytes").expect("held file");
    let alice = &fixture.alice_id;
    let v2 = rusqlite::Connection::open(&v2_db).expect("v2 db opens");
    v2.execute_batch(&format!(
        "ALTER TABLE download_tasks ADD COLUMN download_type TEXT NOT NULL DEFAULT 'album';
         ALTER TABLE download_tasks ADD COLUMN recording_mbid TEXT;
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name,
             album_title, status, created_at, updated_at, download_type, recording_mbid)
         VALUES ('download-track', '{alice}', 'dddddddd-0000-4000-8000-000000000001',
                 'Artist', 'Song', 'failed', 1700000000.0, 1700000000.0, 'track',
                 'EEEEEEEE-0000-4000-8000-000000000001');
         CREATE TABLE held_imports (
             id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL,
             release_group_mbid TEXT, release_mbid TEXT, release_track_mbid TEXT,
             recording_mbid TEXT, track_number INTEGER, disc_number INTEGER,
             track_title TEXT, artist_name TEXT, artist_mbid TEXT, album_title TEXT,
             year INTEGER, held_path TEXT NOT NULL, original_filename TEXT,
             file_format TEXT, duration_seconds REAL, expected_duration_seconds REAL,
             reason TEXT NOT NULL, reason_detail TEXT, evidence_title TEXT,
             evidence_artist TEXT, evidence_score REAL,
             source TEXT NOT NULL DEFAULT 'soulseek', source_task_id TEXT,
             origin TEXT NOT NULL DEFAULT 'user', naming_template TEXT,
             management_retry_count INTEGER NOT NULL DEFAULT 0,
             management_next_retry_at REAL, status TEXT NOT NULL DEFAULT 'held',
             created_at REAL NOT NULL, resolved_at REAL, file_cleanup_completed_at REAL);
         INSERT INTO held_imports (user_id, release_group_mbid, recording_mbid, held_path,
             original_filename, reason, source_task_id, status, created_at)
         VALUES ('{alice}', 'DDDDDDDD-0000-4000-8000-000000000002', NULL,
                 '/app/cache/held/album.flac', 'CD1/01.flac', 'management:occupied',
                 'download-gone', 'held', 1600000000.25),
                ('{alice}', 'dddddddd-0000-4000-8000-000000000001',
                 'EEEEEEEE-0000-4000-8000-000000000001', '/app/cache/held/track.flac',
                 '01 Song.flac', 'wrong_track', 'download-track', 'held', 1600000001.0),
                ('{alice}', 'dddddddd-0000-4000-8000-000000000003', NULL,
                 '/app/cache/held/done.flac', 'done.flac', 'weak_match', NULL, 'imported',
                 1600000002.0);"
    ))
    .expect("v2 held rows seed");
    drop(v2);
    let export_path = root.join("export.json");
    droppedneedle_tool::export::export_v2_to_file(
        &export_request(&root.join("v2"), PASSPHRASE),
        &export_path,
    )
    .expect("export writes");
    let text = std::fs::read_to_string(&export_path).expect("export reads back");
    let (v3_root, target_db) = seeded_target(&root);

    let carried_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970")
        .as_millis();
    run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    let entries: Vec<(String, Option<String>, i64)> = target_db.rows(
        "SELECT group_key, recording_mbid, expires_at FROM acquire_held_entries ORDER BY id",
        [],
    );
    let keys: Vec<(&str, Option<&str>)> = entries
        .iter()
        .map(|(key, recording, _)| (key.as_str(), recording.as_deref()))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("album:dddddddd-0000-4000-8000-000000000002", None),
            (
                "track:eeeeeeee-0000-4000-8000-000000000001",
                Some("eeeeeeee-0000-4000-8000-000000000001")
            ),
        ]
    );
    let ninety_days = 90 * 86_400_000;
    for (key, _, expires_at) in &entries {
        assert!(
            i128::from(*expires_at) >= i128::try_from(carried_at).expect("now fits") + ninety_days,
            "{key} expires before 90 days after the carry"
        );
    }
    let files: Vec<(String, String, String, i64)> = target_db.rows(
        "SELECT path, original_name, reason, created_at FROM acquire_held_files ORDER BY id",
        [],
    );
    assert_eq!(
        files,
        vec![
            (
                "album.flac".to_owned(),
                "CD1/01.flac".to_owned(),
                "target_occupied".to_owned(),
                1_600_000_000_250
            ),
            (
                "track.flac".to_owned(),
                "01 Song.flac".to_owned(),
                "held_by_earlier_version".to_owned(),
                1_600_000_001_000
            ),
        ]
    );
    let v3_held = v3_root.join("cache").join("held");
    assert_eq!(
        std::fs::read(v3_held.join("album.flac")).expect("album copy"),
        b"album bytes"
    );
    assert_eq!(
        std::fs::read(v3_held.join("track.flac")).expect("track copy"),
        b"track bytes"
    );

    run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    let again: i64 = target_db.one("SELECT COUNT(*) FROM acquire_held_files", []);
    assert_eq!(again, 2, "a repeat import adds nothing");
}

/// Stray v2 values carry with safe ones and never stop the upgrade: a text
/// year lands NULL and a REAL track number as its whole number, unknown
/// download labels take their defaults, text durations and scores land
/// NULL, and an unknown request kind or held reason takes its safe value.
#[tokio::test]
async fn stray_v2_values_carry_as_null_or_default() {
    let root = scratch_dir("stray");
    let fixture = build_v2_fixture(&root.join("v2")).expect("fixture builds");
    let v2_cache = root.join("v2").join("cache");
    let v2_db = v2_cache.join("library.db");
    delete_orphan_rows(&v2_db).expect("repair runs");
    let held_dir = v2_cache.join("held");
    std::fs::create_dir_all(&held_dir).expect("v2 held folder");
    std::fs::write(held_dir.join("stray.flac"), b"held bytes").expect("held file");
    let alice = &fixture.alice_id;
    let v2 = rusqlite::Connection::open(&v2_db).expect("v2 db opens");
    v2.execute_batch(&format!(
        "ALTER TABLE download_tasks ADD COLUMN year INTEGER;
         ALTER TABLE download_tasks ADD COLUMN track_number INTEGER;
         ALTER TABLE download_tasks ADD COLUMN download_type TEXT;
         ALTER TABLE download_tasks ADD COLUMN download_client TEXT;
         ALTER TABLE download_tasks ADD COLUMN source TEXT;
         ALTER TABLE download_tasks ADD COLUMN origin TEXT;
         ALTER TABLE download_tasks ADD COLUMN track_duration_seconds REAL;
         ALTER TABLE download_tasks ADD COLUMN preflight_score REAL;
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name,
             album_title, status, year, track_number, created_at, updated_at)
         VALUES ('download-stray', '{alice}', 'dddddddd-0000-4000-8000-000000000004',
                 'Artist', 'Album', 'completed', 'unknown', 7.4, 1700000000.0,
                 1700000000.0);
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name,
             album_title, status, download_type, download_client, source, origin,
             created_at, updated_at)
         VALUES ('download-labels', '{alice}', 'dddddddd-0000-4000-8000-000000000005',
                 'Artist', 'Album', 'completed', 'mixtape', '', 'carrier-pigeon', 'mystery',
                 1700000000.0, 1700000000.0),
                ('download-conversion', '{alice}', 'dddddddd-0000-4000-8000-000000000006',
                 'Artist', 'Album', 'completed', 'album', 'slskd', 'soulseek',
                 'edition_conversion', 1700000000.0, 1700000000.0);
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name,
             album_title, status, track_duration_seconds, preflight_score, created_at,
             updated_at)
         VALUES ('download-stray-real', '{alice}', 'dddddddd-0000-4000-8000-000000000007',
                 'Artist', 'Album', 'completed', '3:42', 'high', 1700000000.0,
                 1700000000.0);
         INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name,
             album_title, requested_at, status, user_id, request_kind)
         VALUES ('eeeeeeee-0000-4000-8000-000000000005',
                 'eeeeeeee-0000-4000-8000-000000000005', 'Artist', 'Album', '1700000000',
                 'imported', '{alice}', 'playlist');
         CREATE TABLE held_imports (
             id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL,
             release_group_mbid TEXT, duration_seconds REAL,
             expected_duration_seconds REAL, evidence_score REAL,
             held_path TEXT NOT NULL, reason TEXT NOT NULL, source_task_id TEXT,
             status TEXT NOT NULL DEFAULT 'held', created_at REAL NOT NULL);
         INSERT INTO held_imports (user_id, release_group_mbid, duration_seconds,
             expected_duration_seconds, evidence_score, held_path, reason, source_task_id,
             status, created_at)
         VALUES ('{alice}', 'dddddddd-0000-4000-8000-000000000007', '3:42', 'long',
                 'strong', '/app/cache/held/stray.flac', 'unheard_of',
                 'download-stray-real', 'held', 1700000000.0);"
    ))
    .expect("v2 rows seed");
    drop(v2);
    let export_path = root.join("export.json");
    droppedneedle_tool::export::export_v2_to_file(
        &export_request(&root.join("v2"), PASSPHRASE),
        &export_path,
    )
    .expect("export writes");
    let text = std::fs::read_to_string(&export_path).expect("export reads back");
    let (v3_root, target_db) = seeded_target(&root);
    run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;

    // Whole-number columns.
    let landed: (Option<i64>, Option<i64>) = target_db.row(
        "SELECT year, track_number FROM download_tasks WHERE id = 'download-stray'",
        [],
    );
    assert_eq!(landed, (None, Some(7)), "whole numbers");

    // Download labels.
    let landed: (String, String, String, String) = target_db.row(
        "SELECT download_type, download_client, source, origin FROM download_tasks \
         WHERE id = 'download-labels'",
        [],
    );
    assert_eq!(
        landed,
        (
            "album".to_owned(),
            "slskd".to_owned(),
            "soulseek".to_owned(),
            "user".to_owned()
        ),
        "download labels"
    );
    let origin: String = target_db.one(
        "SELECT origin FROM download_tasks WHERE id = 'download-conversion'",
        [],
    );
    assert_eq!(origin, "edition", "download labels");

    // Reals and fixed sets.
    let download: (Option<f64>, Option<f64>) = target_db.row(
        "SELECT track_duration_seconds, preflight_score FROM download_tasks \
         WHERE id = 'download-stray-real'",
        [],
    );
    assert_eq!(download, (None, None), "reals");
    let kind: String = target_db.one(
        "SELECT request_kind FROM request_history \
         WHERE musicbrainz_id_lower = 'eeeeeeee-0000-4000-8000-000000000005'",
        [],
    );
    assert_eq!(kind, "album", "fixed sets");
    let held: (Option<f64>, Option<f64>, Option<f64>, String) = target_db.row(
        "SELECT duration_seconds, expected_duration_seconds, evidence_score, reason \
         FROM acquire_held_files WHERE path = 'stray.flac'",
        [],
    );
    assert_eq!(
        held,
        (None, None, None, "held_by_earlier_version".to_owned()),
        "reals and fixed sets"
    );
}

/// The listening preferences Alice lands with after `v2_sql` runs on the v2
/// database: the Last.fm flag, the Navidrome flag, visibility, personal mix.
async fn landed_listening_prefs(tag: &str, v2_sql: &str) -> (i64, i64, String, i64) {
    let root = scratch_dir(tag);
    build_v2_fixture(&root.join("v2")).expect("fixture builds");
    let v2_db = root.join("v2").join("cache").join("library.db");
    delete_orphan_rows(&v2_db).expect("repair runs");
    let v2 = rusqlite::Connection::open(&v2_db).expect("v2 db opens");
    v2.execute_batch(v2_sql).expect("v2 table changes");
    drop(v2);
    let export_path = root.join("export.json");
    droppedneedle_tool::export::export_v2_to_file(
        &export_request(&root.join("v2"), PASSPHRASE),
        &export_path,
    )
    .expect("export writes");
    let text = std::fs::read_to_string(&export_path).expect("export reads back");
    let (v3_root, target_db) = seeded_target(&root);
    run_pipeline(&text, PASSPHRASE, &v3_root, &target_db, false).await;
    target_db.row(
        "SELECT scrobble_to_lastfm, navidrome_handles_external_scrobbles, \
         now_playing_visibility, auto_request_personal_mix FROM user_listening_prefs",
        [],
    )
}

/// Listening preferences carry from any v2 table shape: one last started
/// before the later columns were added carries what it has with v3's
/// defaults for the rest, and a visibility that is not text carries as
/// hiding the song, never as showing it.
#[tokio::test]
async fn listening_prefs_carry_from_any_v2_shape() {
    for (label, v2_sql, expected) in [
        (
            "table without the later columns",
            "ALTER TABLE user_listening_prefs DROP COLUMN now_playing_visibility;
             ALTER TABLE user_listening_prefs DROP COLUMN navidrome_handles_external_scrobbles;
             ALTER TABLE user_listening_prefs DROP COLUMN auto_request_personal_mix;",
            (1, 1, "full".to_owned(), 0),
        ),
        (
            "visibility that is not text",
            "UPDATE user_listening_prefs SET now_playing_visibility = X'00';",
            (1, 1, "track_hidden".to_owned(), 0),
        ),
    ] {
        let landed = landed_listening_prefs("prefs", v2_sql).await;
        assert_eq!(landed, expected, "case: {label}");
    }
}

/// One user logs in with their v2 password through the production login
/// path; success persists the Argon2id rehash, and a wrong password still
/// fails.
async fn login_rehashes(target_db: &Path, db: &Db, username: &str, user_id: &str, password: &str) {
    use droppedneedle_auth::session::login::{
        LoginContext, LoginError, LoginRequest, LoginService, TransportParam,
    };
    use droppedneedle_auth::sqlite::{AuthDb, Sessions, Users};

    fn stored(target_db: &Path, user_id: &str) -> (String, String) {
        target_db.row(
            "SELECT hash_scheme, password_hash FROM auth_providers \
             WHERE user_id = ? AND provider = 'local'",
            rusqlite::params![user_id],
        )
    }
    let (scheme, hash) = stored(target_db, user_id);
    assert_eq!(scheme, "bcrypt");
    assert!(hash.starts_with("$2b$"), "bcrypt carried verbatim: {hash}");

    let auth_db = AuthDb::new(
        db,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let login = LoginService::new(
        Sessions::new(
            &auth_db,
            std::sync::Arc::new(droppedneedle_kernel::ids::UuidGenerator),
        ),
        droppedneedle_auth::passwords::Passwords::new(),
        Users::new(&auth_db),
    );
    let context = || LoginContext {
        user_agent: None,
        now_unix: 1_769_000_000,
    };
    let success = login
        .login(
            LoginRequest {
                username: username.to_owned(),
                password: password.to_owned(),
                transport: TransportParam::Bearer,
            },
            context(),
        )
        .await
        .expect("v2 password logs in");
    assert_eq!(success.user_id, user_id);

    let (scheme, hash) = stored(target_db, user_id);
    assert!(hash.starts_with("$argon2id$"), "rehash persisted: {hash}");
    assert_eq!(scheme, "argon2id", "tag flipped");

    let denied = login
        .login(
            LoginRequest {
                username: username.to_owned(),
                password: "wrong-password".to_owned(),
                transport: TransportParam::Bearer,
            },
            context(),
        )
        .await;
    assert!(matches!(denied, Err(LoginError::InvalidCredentials)));
}

/// Every imported follow reads back with its v2 values.
fn assert_follows_intact(
    target_db: &Path,
    fixture: &droppedneedle_tool::test_support::fixture::V2Fixture,
) {
    let rows: Vec<(String, String, String, i64, i64, i64)> = target_db.rows(
        "SELECT user_id, artist_mbid, artist_name, auto_download, followed_at, updated_at
         FROM acquire_followed_artists ORDER BY user_id, artist_mbid",
        [],
    );
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|row| row.0 == fixture.alice_id
        && row.1 == fixture.artist_one_mbid
        && row.2 == "Fixture Artist One"
        && row.3 == 1));
    assert!(
        rows.iter()
            .any(|row| row.0 == fixture.alice_id && row.1 == fixture.artist_two_mbid && row.3 == 0)
    );
    assert!(
        rows.iter()
            .any(|row| row.0 == fixture.bob_id && row.1 == fixture.artist_one_mbid)
    );
    assert!(rows.iter().all(|row| row.0 != UNKNOWN_USER_ID));
}

/// The compat re-auth production path: the surviving secret resolves by its
/// recomputed SHA-256, decrypts under the v3 key, stamps use; the revoked
/// secret resolves nowhere.
async fn assert_compat_reauth(
    db: &Db,
    v3_root: &Path,
    fixture: &droppedneedle_tool::test_support::fixture::V2Fixture,
) {
    use droppedneedle_auth::session::tokens::hash_token;
    use droppedneedle_auth::sqlite::{AppPasswords, AuthDb};

    let auth_db = AuthDb::new(
        db,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let store = AppPasswords::new(&auth_db);
    let crypto = Crypto::load(&v3_root.join("config")).expect("v3 key loads");

    let row = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.alice_phone_secret))
        .await
        .expect("lookup runs")
        .expect("surviving secret resolves");
    assert_eq!(row.user_id, fixture.alice_id);
    assert_eq!(row.name, "phone");
    assert_eq!(
        crypto
            .decrypt(&row.secret_encrypted)
            .expect("v3 ciphertext opens"),
        fixture.secrets.alice_phone_secret
    );
    store
        .touch(&row.secret_sha256, 1_769_000_100, Some("fixture-client"))
        .await
        .expect("touch runs");
    let touched = store
        .get_active_by_sha256(&row.secret_sha256)
        .await
        .expect("re-lookup runs")
        .expect("row still active");
    assert_eq!(touched.last_client.as_deref(), Some("fixture-client"));

    let revoked = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.alice_old_secret))
        .await
        .expect("revoked lookup runs");
    assert!(revoked.is_none(), "revocation survived the import");

    let legacy = store
        .get_active_by_sha256(&hash_token(&fixture.secrets.bob_legacy_secret))
        .await
        .expect("legacy lookup runs")
        .expect("legacy-plaintext secret resolves");
    assert_eq!(legacy.user_id, fixture.bob_id);
}

/// Settings assertions: the import carried the legacy frequency, secrets decrypt
/// under the v3 key, dropped fields stayed behind, order and ids survived.
fn assert_settings_landed(
    v3_root: &Path,
    fixture: &droppedneedle_tool::test_support::fixture::V2Fixture,
) {
    let config = v3_settings(v3_root);
    assert_eq!(
        config
            .get("instance_id")
            .and_then(serde_json::Value::as_str),
        Some(fixture.instance_id.as_str())
    );
    assert_eq!(
        config
            .get("library_scan_schedule")
            .and_then(|section| section.get("scan_frequency"))
            .and_then(serde_json::Value::as_str),
        Some("6hr"),
        "the import carried the legacy frequency"
    );
    let crypto = Crypto::load(&v3_root.join("config")).expect("v3 key loads");
    let slskd = config
        .get("download_client")
        .and_then(|section| section.get("api_key"))
        .and_then(serde_json::Value::as_str)
        .expect("slskd key present");
    assert_eq!(
        crypto.decrypt(slskd).expect("slskd key opens"),
        fixture.secrets.slskd_key
    );
    let audio = config
        .get("advanced_settings")
        .and_then(|section| section.get("audiodb_api_key"))
        .and_then(serde_json::Value::as_str)
        .expect("audiodb key present");
    assert_eq!(
        crypto.decrypt(audio).expect("audiodb key opens"),
        fixture.secrets.audiodb_key
    );
    assert!(
        config
            .get("advanced_settings")
            .and_then(|section| section.get("artist_discovery_warm_interval"))
            .is_none(),
        "dropped advanced field stayed behind"
    );
    let indexers = config
        .get("indexers")
        .and_then(serde_json::Value::as_array)
        .expect("indexers present");
    assert_eq!(indexers.len(), 2);
    assert_eq!(
        indexers[0].get("name").and_then(serde_json::Value::as_str),
        Some("first"),
        "indexer order survived"
    );
}
