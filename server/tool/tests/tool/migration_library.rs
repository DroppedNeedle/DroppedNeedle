//! The v2 library carry, end to end: a v2 album that Library Management
//! retagged and moved, and that a curator identified by hand, goes
//! through export and import, v3 scans it, and everything that cannot be
//! rebuilt is still there: the track and album ids (which every Subsonic
//! and Jellyfin id derives from), the manual identity, an edition pin as
//! the album's protected edition, open MusicBrainz contributions (one
//! still being verified, one waiting for review with its reason), and
//! the original file, which "restore original" brings back exactly as v2
//! first found it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, Secret};
use droppedneedle_library::identify::models::DecisionSource;
use droppedneedle_library::tags::save::{TagEdit, save_tags};
use droppedneedle_library::tags::{TagField, read_fields};
use droppedneedle_library::wiring::LibrarySetup;
use droppedneedle_tool::tooling::fixture::{
    MUSIC_ROOT_ID, V2_ALBUM_ID, V2_TRACK_ID, V2_TRACK_REL, build_v2_fixture, delete_orphan_rows,
};
use rusqlite::params;
use sha2::{Digest as _, Sha256};

use crate::common::ScratchDir;
use crate::common::library::library_at;

const PASSPHRASE: &str = "operator-passphrase";
/// The second track: v2 has it, its file changed since, so v3 re-reads it.
const SECOND_TRACK_ID: &str = "v2-track-3";
const SECOND_TRACK_REL: &str = "John Coltrane/Blue Train/02.flac";
/// An album v2 identified on its own and a curator then pinned to another
/// edition of the same release group.
const PINNED_ALBUM_ID: &str = "v2-album-giant";
/// An album whose MusicBrainz contribution v2 was still verifying.
const VERIFYING_ALBUM_ID: &str = "v2-album-ballads";
/// Why v2 sent the other open contribution to review.
const REVIEW_REASON: &str = "MUSICBRAINZ_RELEASE_NOT_PROPAGATED";
/// Where the first track's file was before v2 first managed it.
const ORIGINAL_REL: &str = "Incoming/blue train.flac";

fn plant(path: &Path, fixture: &str, fields: &[(TagField, &str)]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/library")
            .join(fixture),
        path,
    )
    .expect("fixture copies");
    let edits: Vec<TagEdit> = fields
        .iter()
        .map(|(field, value)| TagEdit::new(*field, vec![(*value).to_owned()]))
        .collect();
    save_tags(path, &edits).expect("tags planted");
}

/// v2's tag snapshot of the original file, in the shape v2 wrote it.
fn original_snapshot() -> Vec<u8> {
    let entry = |key: &str, value: &str| {
        serde_json::json!({"key": key, "values": [{"kind": "text", "text": value}],
                           "container": "list"})
    };
    serde_json::to_vec(&serde_json::json!({
        "snapshot_version": 1,
        "adapter_version": "1",
        "probe": {"extension": ".flac", "admitted": true, "detected_format": "flac",
                  "detected_class": "FLAC", "extension_matches": true},
        "metadata": {"fields": []},
        "artwork": [],
        "technical": {"duration_seconds": 1.0, "bitrate_bps": 0, "sample_rate_hz": 44100,
                      "channels": 2, "bit_depth": 16, "codec": "flac", "file_size_bytes": 1},
        "raw_tags": [],
        "native_tags": {"storage_kind": "vorbis_comments", "entries": [
            entry("album", "Blue Train"),
            entry("artist", "Coltrane"),
            entry("title", "Blue Train (original rip)"),
            entry("tracknumber", "1"),
        ]},
        "file_attributes": {"atime_ns": 0, "mtime_ns": 0, "permission_bits": 420},
    }))
    .expect("snapshot json")
}

/// The managed, manually identified album in v2: rows, baseline blob and
/// files. Returns the music root.
fn managed_album(v2_root: &Path, alice: &str) -> PathBuf {
    let music = v2_root.join("music");
    let first = music.join(V2_TRACK_REL);
    let second = music.join(SECOND_TRACK_REL);
    // As v2 left it: retagged from the release and organized.
    plant(
        &first,
        "flac_full_01.flac",
        &[
            (TagField::Title, "Blue Train"),
            (TagField::Artist, "John Coltrane"),
            (TagField::Album, "Blue Train"),
            (TagField::AlbumArtist, "John Coltrane"),
            (TagField::TrackNumber, "1"),
            (TagField::MusicBrainzReleaseId, "rel-blue"),
            (TagField::MusicBrainzRecordingId, "rec-1"),
        ],
    );
    plant(
        &second,
        "flac_full_02.flac",
        &[
            (TagField::Title, "Moment's Notice"),
            (TagField::Artist, "John Coltrane"),
            (TagField::Album, "Blue Train"),
            (TagField::AlbumArtist, "John Coltrane"),
            (TagField::TrackNumber, "2"),
        ],
    );
    let snapshot = original_snapshot();
    let sha = format!("{:x}", Sha256::digest(&snapshot));
    let blob_rel = format!("objects/{}/{}/{sha}.blob", &sha[..2], &sha[2..4]);
    let blob = v2_root
        .join("cache")
        .join("library-management")
        .join("blobs")
        .join(&blob_rel);
    std::fs::create_dir_all(blob.parent().expect("parent")).expect("blob dir");
    std::fs::write(&blob, &snapshot).expect("blob writes");

    let meta = std::fs::metadata(&first).expect("first stat");
    let mtime = droppedneedle_library::scan::mtime_ns_from_metadata(&meta);
    let db = rusqlite::Connection::open(v2_root.join("cache").join("library.db")).expect("v2 db");
    // The first file is unchanged since v2 indexed it; the second is not.
    db.execute(
        "UPDATE local_tracks SET file_size_bytes = ?1, file_mtime_ns = ?2, stat_revision = ?3, \
         embedded_release_mbid = 'rel-blue', embedded_recording_mbid = 'rec-1' WHERE id = ?4",
        params![
            meta.len() as i64,
            mtime,
            droppedneedle_library::scan::exact_stat_revision(meta.len(), mtime),
            V2_TRACK_ID
        ],
    )
    .expect("first track stat");
    db.execute_batch(&format!(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind,
             title, title_folded, artist_name, artist_name_folded, album_title,
             album_title_folded, album_artist_name, album_artist_name_folded, disc_number,
             track_number, file_format, availability, ingest_source, imported_at,
             membership_source, title_provenance, album_title_provenance,
             album_artist_provenance)
         VALUES ('{SECOND_TRACK_ID}', '{V2_ALBUM_ID}', '{MUSIC_ROOT_ID}', '/music/x',
                 '{SECOND_TRACK_REL}', 'h2', 1, 1, '1:1', 'exact', 'Moment''s Notice',
                 'moment''s notice', 'John Coltrane', 'john coltrane', 'Blue Train',
                 'blue train', 'John Coltrane', 'john coltrane', 1, 2, 'flac', 'indexed',
                 'scan', 1700000000.0, 'automatic', 'tag', 'tag', 'tag');
         INSERT INTO local_album_external_identities (local_album_id, release_group_mbid,
             release_mbid, decision_source, selected_by_user_id, selected_at)
         VALUES ('{V2_ALBUM_ID}', 'rg-blue', 'rel-blue', 'manual', '{alice}', 1700000500.0);
         INSERT INTO local_track_external_identities (local_track_id, recording_mbid,
             release_mbid, release_track_mbid, decision_source, selected_at)
         VALUES ('{V2_TRACK_ID}', 'rec-1', 'rel-blue', 'reltrack-1', 'manual', 1700000500.0),
                ('{SECOND_TRACK_ID}', 'rec-2', 'rel-blue', 'reltrack-2', 'manual',
                 1700000500.0);
         INSERT INTO library_management_blobs (sha256, kind, byte_length, relative_path,
             created_at)
         VALUES ('{sha}', 'tag_snapshot', {len}, '{blob_rel}', 1700000600.0);
         INSERT INTO library_management_baselines (id, local_track_id, original_root_id,
             original_relative_path, format, adapter_version, semantic_snapshot_blob_sha256,
             stat_revision, tag_revision, created_at)
         VALUES ('baseline-1', '{V2_TRACK_ID}', '{MUSIC_ROOT_ID}', '{ORIGINAL_REL}', 'flac',
                 '1', '{sha}', '1:1', 'tags', 1700000600.0);
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source,
             created_at, updated_at)
         VALUES ('{PINNED_ALBUM_ID}', '{MUSIC_ROOT_ID}', 'k', 'Giant Steps', 'giant steps',
                 'John Coltrane', 'john coltrane', 'v2-artist-1', 'automatic',
                 1700000000.0, 1700000000.0);
         INSERT INTO local_album_external_identities (local_album_id, release_group_mbid,
             release_mbid, decision_source, selected_at)
         VALUES ('{PINNED_ALBUM_ID}', 'rg-giant', 'rel-giant-us', 'automatic', 1700000500.0);
         INSERT INTO library_album_release_pins (local_album_id, release_group_mbid,
             release_mbid, set_by_user_id, set_at)
         VALUES ('{PINNED_ALBUM_ID}', 'rg-giant', 'rel-giant-jp', '{alice}',
                 '2025-06-01T00:00:00Z');
         INSERT INTO library_track_management_state (local_track_id, baseline_id,
             managed_root_id, last_managed_at, last_outcome)
         VALUES ('{V2_TRACK_ID}', 'baseline-1', '{MUSIC_ROOT_ID}', 1700000600.0, 'applied');
         INSERT INTO library_contribution_drafts (id, local_album_id, created_by_user_id,
             state, album_row_revision, input_revision, local_snapshot_json,
             resolved_draft_json, source_selection_json, seeded_at, created_at, updated_at)
         VALUES ('contrib-open', '{PINNED_ALBUM_ID}', '{alice}', 'seeded', 1, 'r1', '{{}}',
                 '{{}}', '{{}}', 1700000700.0, 1700000700.0, 1700000700.0),
                ('contrib-done', '{V2_ALBUM_ID}', '{alice}', 'linked', 1, 'r1', '{{}}',
                 '{{}}', '{{}}', NULL, 1700000100.0, 1700000200.0),
                ('contrib-review', '{V2_ALBUM_ID}', '{alice}', 'needs_review', 1, 'r1',
                 '{{}}', '{{}}', '{{}}', 1700000700.0, 1700000300.0, 1700000900.0),
                ('contrib-verifying', '{VERIFYING_ALBUM_ID}', '{alice}', 'verifying', 1,
                 'r1', '{{}}', '{{}}', '{{}}', 1700000700.0, 1700000300.0, 1700000900.0);
         UPDATE library_contribution_drafts SET result_release_mbid = 'rel-ballads',
             result_source = 'manual', result_received_at = 1700000800.0
         WHERE id IN ('contrib-review', 'contrib-verifying');
         INSERT INTO library_contribution_verification_jobs (id, contribution_id, state,
             attempt_count, not_before, last_failure_code, created_at, updated_at,
             terminal_at)
         VALUES ('job-review-old', 'contrib-review', 'needs_review', 10, 0,
                 'ATTACHMENT_NAME_MISMATCH', 1700000400.0, 1700000500.0, 1700000500.0),
                ('job-review', 'contrib-review', 'needs_review', 10, 0, '{REVIEW_REASON}',
                 1700000800.0, 1700000900.0, 1700000900.0),
                ('job-verifying', 'contrib-verifying', 'running', 1, 1700000800.0, NULL,
                 1700000800.0, 1700000850.0, NULL);
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source,
             created_at, updated_at)
         VALUES ('{VERIFYING_ALBUM_ID}', '{MUSIC_ROOT_ID}', 'kb', 'Ballads', 'ballads',
                 'John Coltrane', 'john coltrane', 'v2-artist-1', 'automatic',
                 1700000000.0, 1700000000.0);
         INSERT INTO library_contribution_callback_tokens (token_hash, contribution_id,
             requested_by_user_id, expires_at, created_at)
         VALUES ('token-open', 'contrib-open', '{alice}', 4000000000.0, 1700000700.0);
         INSERT INTO youtube_links (album_id, video_id, album_name, artist_name, embed_url,
             created_at, is_manual, track_count)
         VALUES ('rg-blue', 'vid-album', 'Blue Train', 'John Coltrane', 'https://e/a',
                 '2025-06-01T00:00:00Z', 1, 2);
         INSERT INTO youtube_track_links (album_id, track_number, disc_number, album_name,
             track_name, video_id, artist_name, embed_url, created_at)
         VALUES ('rg-blue', 1, 1, 'Blue Train', 'Blue Train', 'vid-1', 'John Coltrane',
                 'https://e/1', '2025-06-01T00:00:00Z');
         INSERT INTO ignored_releases (user_id, release_group_mbid_lower, release_group_mbid,
             artist_mbid, release_name, artist_name, ignored_at)
         VALUES ('{alice}', 'rg-hidden', 'RG-HIDDEN', 'artist-hidden', 'Hidden', 'Someone',
                 1700000800.0);",
        len = snapshot.len(),
    ))
    .expect("library rows");
    music
}

pub(crate) async fn drain_scans(library: &LibrarySetup) {
    use droppedneedle_library::scan::{ScanKind, ScanRequest, ScanTrigger};
    let registry = library.core.live_registry();
    library
        .coordinator
        .request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Manual,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        })
        .expect("scan requested");
    library.scans.scan_startup_recovery().await;
    for _ in 0..100 {
        library.scans.supervisor_tick().await;
        if library.coordinator.current().is_empty() {
            break;
        }
    }
    assert!(library.coordinator.current().is_empty(), "scans drain");
}

#[tokio::test]
async fn managed_identified_album_keeps_ids_identity_and_original() {
    let scratch = ScratchDir::new("migration-library");
    let root = scratch.to_path_buf();
    let v2_root = root.join("v2");
    let fixture = build_v2_fixture(&v2_root).expect("fixture builds");
    delete_orphan_rows(&v2_root.join("cache").join("library.db")).expect("repair runs");
    let music = managed_album(&v2_root, &fixture.alice_id);
    let first = music.join(V2_TRACK_REL);

    let export_path = root.join("export.json");
    droppedneedle_tool::export::export_v2_to_file(
        &droppedneedle_tool::export::ExportRequest {
            v2: droppedneedle_tool::export::v2dir::V2Dirs::under_root(&v2_root),
            db_path: None,
            passphrase: Secret::new(PASSPHRASE.to_owned()),
            exported_at: None,
            v2_commit: None,
        },
        &export_path,
    )
    .expect("export writes");

    let v3 = root.join("v3");
    let config_dir = v3.join("config");
    std::fs::create_dir_all(&config_dir).expect("v3 config dir");
    std::fs::create_dir_all(v3.join("cache")).expect("v3 cache dir");
    let db_path = v3.join("cache").join("library.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .await
        .expect("pool opens");
    droppedneedle_kernel::schema::apply_migrations(&pool)
        .await
        .expect("schema migrates");
    let report =
        droppedneedle_tool::r#import::run_import(droppedneedle_tool::r#import::ImportRequest {
            export_bytes: std::fs::read(&export_path).expect("export reads"),
            passphrase: PASSPHRASE.to_owned(),
            pool: pool.clone(),
            legacy: droppedneedle_kernel::runtime_config::LegacyConfig::at(&config_dir, false),
            crypto: Crypto::load_or_generate(&config_dir).expect("v3 key").0,
            v2_config_path: None,
            attachments_dir: Some(root.clone()),
            cache_dir: Some(v3.join("cache")),
            dry_run: false,
            fault_before_commit: false,
            fault_after_commit: false,
            fault_after_sections: None,
        })
        .await;
    assert!(
        matches!(
            report.exit.code,
            droppedneedle_tool::r#import::ExitCode::Ok
                | droppedneedle_tool::r#import::ExitCode::OkWithDrops
        ),
        "{:?}",
        report.exit
    );
    let count = |entity: &str| report.entities[entity].imported;
    assert_eq!(count("library_track"), 2);
    // The curator's match and the pinned album's automatic one.
    assert_eq!(count("album_identity"), 2);
    assert_eq!(count("management_baseline"), 1);
    assert_eq!(count("original_baseline"), 1, "{:?}", report.items);
    // The open MusicBrainz contributions move, one with its return link;
    // the linked one stays behind.
    assert_eq!(count("contribution_draft"), 3);
    assert_eq!(count("contribution_callback_token"), 1);
    assert_eq!(count("youtube_link"), 1);
    assert_eq!(count("youtube_track_link"), 1);
    assert_eq!(count("ignored_release"), 1);
    let ignored_at: i64 = sqlx::query_scalar(
        "SELECT ignored_at FROM catalog_ignored_releases WHERE release_group_mbid = 'RG-HIDDEN'",
    )
    .fetch_one(&pool)
    .await
    .expect("hidden release carried");
    assert_eq!(
        ignored_at, 1_700_000_800_000,
        "v2 seconds arrive as milliseconds"
    );
    pool.close().await;

    // v3 starts on the imported state and scans the carried root.
    let handle = droppedneedle_kernel::db::Db::open_for_tests(&db_path).expect("database opens");
    let config = Arc::new(
        ConfigStore::open_blocking(
            handle.clone(),
            Crypto::load(&config_dir).expect("v3 key loads"),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
            &droppedneedle_kernel::runtime_config::LegacyConfig::None,
        )
        .expect("settings open"),
    );
    let platform = crate::common::TestPlatform::on(&handle, &config_dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    drain_scans(&library).await;

    // The contribution v2 was verifying is verified again: v3's first
    // upkeep pass queues its job. The one waiting for review keeps the
    // reason v2's last verification gave.
    let verifying_jobs = || {
        rusqlite::Connection::open(&db_path)
            .expect("v3 db")
            .query_row(
                "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'library.contribution_verify' \
                 AND idempotency_key = 'contrib-verifying' AND state = 'queued'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("jobs counted")
    };
    assert_eq!(verifying_jobs(), 0);
    assert!(library.contrib_worker.queue_missing().await);
    assert_eq!(verifying_jobs(), 1);
    let review = library
        .contrib
        .store()
        .get("contrib-review")
        .await
        .expect("contribution reads")
        .expect("contribution carried");
    assert_eq!(
        review.last_verification_failure.as_deref(),
        Some(REVIEW_REASON)
    );

    // Ids stay: the unchanged file and the re-read one keep their track,
    // and both stay on v2's album.
    let store = &library.scan_store;
    assert_eq!(
        store
            .track_at(MUSIC_ROOT_ID, V2_TRACK_REL)
            .unwrap()
            .as_deref(),
        Some(V2_TRACK_ID)
    );
    assert_eq!(
        store
            .track_at(MUSIC_ROOT_ID, SECOND_TRACK_REL)
            .unwrap()
            .as_deref(),
        Some(SECOND_TRACK_ID)
    );
    for track in [V2_TRACK_ID, SECOND_TRACK_ID] {
        assert_eq!(
            store.album_for_track(track).unwrap().as_deref(),
            Some(V2_ALBUM_ID)
        );
    }
    // The curator's match is still the curator's.
    let identity = library
        .identify_store
        .album_identity(V2_ALBUM_ID)
        .unwrap()
        .expect("identity kept");
    assert_eq!(identity.decision_source, DecisionSource::Manual);
    assert_eq!(identity.release_mbid.as_deref(), Some("rel-blue"));
    // The pinned edition is now the person's own choice of edition.
    let pinned = library
        .identify_store
        .album_identity(PINNED_ALBUM_ID)
        .unwrap()
        .expect("pinned identity");
    assert_eq!(pinned.decision_source, DecisionSource::Manual);
    assert_eq!(pinned.release_mbid.as_deref(), Some("rel-giant-jp"));

    // "Restore original" brings back the file as v2 first found it: its
    // place and exactly its tags, with the fields management added gone.
    library
        .publishing
        .baseline_restore(&[V2_TRACK_ID.to_owned()])
        .expect("restore publishes");
    let original = music.join(ORIGINAL_REL);
    assert!(original.is_file(), "the file is back where it started");
    assert!(!first.exists(), "and gone from where v2 put it");
    let expected: BTreeMap<TagField, Vec<String>> = [
        (TagField::Album, "Blue Train"),
        (TagField::Artist, "Coltrane"),
        (TagField::Title, "Blue Train (original rip)"),
        (TagField::TrackNumber, "1"),
    ]
    .into_iter()
    .map(|(field, value)| (field, vec![value.to_owned()]))
    .collect();
    assert_eq!(read_fields(&original).expect("fields read"), expected);
}
