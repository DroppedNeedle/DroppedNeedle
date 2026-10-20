//! v2 artists through export, import and v3's first scan: each artist
//! keeps one row, under the id the scan finds it by, with v2's id as an
//! alias and its plays and favorites on it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, Secret};
use droppedneedle_library::scan::naming::{UNKNOWN_ARTIST_ID, artist_key};
use droppedneedle_library::tags::TagField;
use droppedneedle_library::tags::save::{TagEdit, save_tags};
use droppedneedle_tool::r#import::{ExitCode, ImportReport};
use droppedneedle_tool::tooling::fixture::{
    MUSIC_ROOT_ID, V2_ALBUM_ID, V2_ARTIST_ID, V2_TRACK_ID, V2_TRACK_REL, build_v2_fixture,
    delete_orphan_rows,
};
use rusqlite::{Connection, params};

use crate::common::ScratchDir;
use crate::common::library::library_at;
use crate::migration_library::drain_scans;

const PASSPHRASE: &str = "operator-passphrase";
const CLAIRO_ID: &str = "v2-clairo";
const COTTRILL_ID: &str = "v2-claire-cottrill";
const UNKNOWN_ID: &str = "v2-unknown-artist";

/// v2's artist tables the base fixture leaves out, in the columns the
/// export reads.
const V2_ARTIST_TABLES: &str = "
    CREATE TABLE local_artist_aliases (alias TEXT PRIMARY KEY, local_artist_id TEXT NOT NULL,
        kind TEXT NOT NULL, created_at REAL NOT NULL);
    CREATE TABLE local_artist_external_identities (local_artist_id TEXT NOT NULL,
        provider TEXT NOT NULL, provider_artist_id TEXT NOT NULL, decision_source TEXT NOT NULL,
        selected_by_user_id TEXT, selected_at REAL NOT NULL,
        PRIMARY KEY (local_artist_id, provider));
    CREATE TABLE local_artist_merge_candidates (id TEXT PRIMARY KEY,
        left_artist_id TEXT NOT NULL, right_artist_id TEXT NOT NULL, reason_code TEXT NOT NULL,
        state TEXT NOT NULL, created_at REAL NOT NULL, updated_at REAL NOT NULL,
        row_revision INTEGER NOT NULL DEFAULT 1);
    CREATE TABLE library_artist_reconciliation_dismissals (left_artist_id TEXT NOT NULL,
        right_artist_id TEXT NOT NULL, left_artist_revision INTEGER NOT NULL,
        right_artist_revision INTEGER NOT NULL, dismissed_by_user_id TEXT,
        reason_code TEXT NOT NULL, created_at REAL NOT NULL, updated_at REAL NOT NULL,
        row_revision INTEGER NOT NULL DEFAULT 1, PRIMARY KEY (left_artist_id, right_artist_id));
    CREATE TABLE local_entity_source_links (id TEXT PRIMARY KEY, local_artist_id TEXT,
        local_album_id TEXT, local_track_id TEXT, provider TEXT NOT NULL,
        external_entity_type TEXT NOT NULL, external_id TEXT NOT NULL,
        canonical_url TEXT NOT NULL, decision_source TEXT NOT NULL, selected_by_user_id TEXT,
        verified_at REAL NOT NULL, created_at REAL NOT NULL, updated_at REAL NOT NULL);
";

/// A file in the v2 music root with these tags over the fixture's. The
/// fixture's artist id is not aligned to a listed artist, so no credit
/// carries one; the release and recording ids are the file's own.
fn plant(music: &Path, relative: &str, fields: &[(TagField, &str)]) {
    let path = music.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/library/flac_full_01.flac"),
        &path,
    )
    .expect("fixture copies");
    let fields = [
        fields,
        &[
            (
                TagField::MusicBrainzReleaseId,
                relative.split('/').nth(1).unwrap_or(relative),
            ),
            (TagField::MusicBrainzRecordingId, relative),
        ],
    ]
    .concat();
    let edits: Vec<TagEdit> = fields
        .iter()
        .map(|(field, value)| TagEdit::new(*field, vec![(*value).to_owned()]))
        .collect();
    save_tags(&path, &edits).expect("tags planted");
}

/// The fixture v2 instance with the extra artist tables, its database open.
fn v2_instance(root: &Path) -> (PathBuf, String, Connection) {
    let v2_root = root.join("v2");
    let fixture = build_v2_fixture(&v2_root).expect("fixture builds");
    let db_path = v2_root.join("cache").join("library.db");
    delete_orphan_rows(&db_path).expect("repair runs");
    let db = Connection::open(&db_path).expect("v2 db");
    db.execute_batch(V2_ARTIST_TABLES).expect("artist tables");
    (v2_root, fixture.alice_id, db)
}

fn v2_artist(db: &Connection, id: &str, name: &str, sort: Option<&str>, into: Option<&str>) {
    db.execute(
        "INSERT INTO local_artists (id, display_name, sort_name, folded_name, normalized_name,
             kind, retired_into_artist_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, lower(?2), lower(?2), 'group', ?4, 1700000000.0, 1700000000.0)",
        params![id, name, sort, into],
    )
    .expect("artist row");
}

fn v2_album(db: &Connection, id: &str, title: &str, artist: (&str, &str)) {
    db.execute(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
             album_artist_name, album_artist_name_folded, album_artist_id, grouping_source,
             created_at, updated_at)
         VALUES (?1, ?2, ?1, ?3, lower(?3), ?4, lower(?4), ?5, 'automatic',
             1700000000.0, 1700000000.0)",
        params![id, MUSIC_ROOT_ID, title, artist.1, artist.0],
    )
    .expect("album row");
    db.execute(
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role,
             credited_name) VALUES (?1, 0, ?2, 'main', ?3)",
        params![id, artist.0, artist.1],
    )
    .expect("album credit");
}

/// A v2 track whose file v3 reads again (v2's stat differs), credited to
/// `artist` under `credited`.
fn v2_track(db: &Connection, id: &str, album: &str, relative: &str, artist: (&str, &str)) {
    db.execute(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
             artist_name, album_title, album_title_folded, track_number, file_format,
             availability, ingest_source, imported_at, membership_source, title_provenance,
             album_title_provenance, album_artist_provenance)
         VALUES (?1, ?2, ?3, ?4, ?4, ?1, 0, 0, '0:0', ?1, ?1, ?5, 'album', 'album', 1, 'flac',
             'indexed', 'scan', 1700000000.0, 'automatic', 'tag', 'tag', 'tag')",
        params![id, album, MUSIC_ROOT_ID, relative, artist.1],
    )
    .expect("track row");
    db.execute(
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role,
             credited_name) VALUES (?1, 0, ?2, 'main', ?3)",
        params![id, artist.0, artist.1],
    )
    .expect("track credit");
}

fn export(root: &Path, v2_root: &Path) -> PathBuf {
    let export_path = root.join("export.json");
    droppedneedle_tool::export::export_v2_to_file(
        &droppedneedle_tool::export::ExportRequest {
            v2: droppedneedle_tool::export::v2dir::V2Dirs::under_root(v2_root),
            db_path: None,
            passphrase: Secret::new(PASSPHRASE.to_owned()),
            exported_at: None,
            v2_commit: None,
        },
        &export_path,
    )
    .expect("export writes");
    export_path
}

/// Import the export into a fresh v3 data folder. Returns the report and
/// the v3 database.
async fn import(root: &Path, export_path: &Path) -> (ImportReport, PathBuf) {
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
            export_bytes: std::fs::read(export_path).expect("export reads"),
            passphrase: PASSPHRASE.to_owned(),
            pool: pool.clone(),
            legacy: droppedneedle_kernel::runtime_config::LegacyConfig::at(&config_dir, false),
            crypto: Crypto::load_or_generate(&config_dir).expect("v3 key").0,
            v2_config_path: None,
            attachments_dir: Some(root.to_owned()),
            cache_dir: Some(v3.join("cache")),
            dry_run: false,
            fault_before_commit: false,
            fault_after_commit: false,
            fault_after_sections: None,
        })
        .await;
    pool.close().await;
    assert!(
        matches!(report.exit.code, ExitCode::Ok | ExitCode::OkWithDrops),
        "{:?}",
        report.exit
    );
    (report, db_path)
}

/// v3 starts on the imported database and scans the carried root once.
async fn first_scan(db_path: &Path) {
    let config_dir = db_path
        .parent()
        .and_then(Path::parent)
        .expect("v3 folder")
        .join("config");
    let handle = droppedneedle_kernel::db::Db::open_for_tests(db_path).expect("database opens");
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
}

/// The live artist rows other than the two sentinels, as `(id, name)`.
fn live_artists(db: &Connection) -> Vec<(String, String)> {
    let mut stmt = db
        .prepare(
            "SELECT id, display_name FROM local_artists WHERE retired_into_artist_id IS NULL \
             AND id NOT LIKE '00000000-0000-4000-8000-00000000000_' ORDER BY display_name",
        )
        .expect("prepare");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// The live row an artist id opens: itself, an alias's row, or the row
/// it was merged into (the order artist reads use).
fn opens(db: &Connection, id: &str) -> Option<String> {
    db.query_row(
        "SELECT id FROM ( \
         SELECT id, 0 AS rank FROM local_artists WHERE id = ?1 AND retired_into_artist_id IS NULL \
         UNION ALL SELECT r.id, 1 FROM local_artist_aliases x \
          JOIN local_artists r ON r.id = x.local_artist_id \
          WHERE x.alias = lower(?1) AND r.retired_into_artist_id IS NULL \
         UNION ALL SELECT r.id, 2 FROM local_artists a \
          JOIN local_artists r ON r.id = a.retired_into_artist_id \
          WHERE a.id = ?1 AND r.retired_into_artist_id IS NULL) \
         ORDER BY rank LIMIT 1",
        params![id],
        |row| row.get(0),
    )
    .ok()
}

fn credit_row(db: &Connection, track: &str) -> String {
    db.query_row(
        "SELECT local_artist_id FROM local_track_artists WHERE local_track_id = ?1 \
         AND position = 0",
        params![track],
        |row| row.get(0),
    )
    .expect("track credit")
}

/// After a v2 import and the first scan each artist has one row, keyed as
/// the scan keys it, holding its plays and its favorite, and v2's id still
/// opens it. A track's credit shares its album's credit of the same name,
/// so John Coltrane takes the album credit's key although two of his three
/// credits name a sort the album lacks. A credit under the name of an
/// artist v2 merged away lands on the survivor, with no row made for it.
#[tokio::test]
async fn v2_artists_keep_one_row_with_their_plays_after_the_first_scan() {
    let scratch = ScratchDir::new("migration-artists");
    let root = scratch.to_path_buf();
    let (v2_root, alice, db) = v2_instance(&root);
    let music = v2_root.join("music");
    let coltrane = [
        (TagField::Artist, "John Coltrane"),
        (TagField::ArtistSort, "Coltrane, John"),
        (TagField::Album, "Blue Train"),
        (TagField::AlbumArtist, "John Coltrane"),
    ];
    plant(
        &music,
        V2_TRACK_REL,
        &[&coltrane[..], &[(TagField::Title, "Blue Train")]].concat(),
    );
    plant(
        &music,
        "John Coltrane/Blue Train/02.flac",
        &[&coltrane[..], &[(TagField::Title, "Moment's Notice")]].concat(),
    );
    v2_track(
        &db,
        "v2-jc-2",
        V2_ALBUM_ID,
        "John Coltrane/Blue Train/02.flac",
        (V2_ARTIST_ID, "John Coltrane"),
    );
    db.execute(
        "UPDATE local_tracks SET artist_sort = 'Coltrane, John' WHERE local_album_id = ?1",
        params![V2_ALBUM_ID],
    )
    .expect("sorts");
    let clairo = [
        (TagField::Album, "Immunity"),
        (TagField::AlbumArtist, "Clairo"),
    ];
    plant(
        &music,
        "Clairo/Immunity/01.flac",
        &[
            &clairo[..],
            &[(TagField::Title, "Alewife"), (TagField::Artist, "Clairo")],
        ]
        .concat(),
    );
    plant(
        &music,
        "Clairo/Immunity/02.flac",
        &[
            &clairo[..],
            &[
                (TagField::Title, "Bags"),
                (TagField::Artist, "Claire Cottrill"),
                (TagField::ArtistSort, "Clairo"),
            ],
        ]
        .concat(),
    );
    v2_artist(&db, CLAIRO_ID, "Clairo", None, None);
    v2_artist(
        &db,
        COTTRILL_ID,
        "Claire Cottrill",
        Some("Clairo"),
        Some(CLAIRO_ID),
    );
    v2_album(&db, "v2-immunity", "Immunity", (CLAIRO_ID, "Clairo"));
    v2_track(
        &db,
        "v2-alewife",
        "v2-immunity",
        "Clairo/Immunity/01.flac",
        (CLAIRO_ID, "Clairo"),
    );
    v2_track(
        &db,
        "v2-bags",
        "v2-immunity",
        "Clairo/Immunity/02.flac",
        (CLAIRO_ID, "Claire Cottrill"),
    );
    db.execute_batch(&format!(
        "UPDATE local_tracks SET artist_sort = 'Clairo' WHERE id = 'v2-bags';
         INSERT INTO local_artist_aliases (alias, local_artist_id, kind, created_at)
         VALUES ('{COTTRILL_ID}', '{CLAIRO_ID}', 'merged_artist', 1700000000.0);
         INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id,
             local_artist_id, track_name, artist_name, album_name, played_at)
         VALUES ('listen-jc-2', '{alice}', 'v2-jc-2', '{V2_ALBUM_ID}', '{V2_ARTIST_ID}',
                 'Moment''s Notice', 'John Coltrane', 'Blue Train', '2025-06-01T00:00:00Z'),
                ('listen-bags', '{alice}', 'v2-bags', 'v2-immunity', '{CLAIRO_ID}',
                 'Bags', 'Clairo', 'Immunity', '2025-06-01T00:00:00Z');
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at)
         VALUES ('{alice}', 'artist', '{V2_ARTIST_ID}', 1700000002.0);"
    ))
    .expect("plays and favorite");
    drop(db);

    let export_path = export(&root, &v2_root);
    let (_, db_path) = import(&root, &export_path).await;
    first_scan(&db_path).await;

    let v3 = Connection::open(&db_path).expect("v3 db");
    let coltrane_id = artist_key("John Coltrane", None);
    let clairo_id = artist_key("Clairo", None);
    assert_eq!(
        live_artists(&v3),
        [
            (clairo_id.clone(), "Clairo".to_owned()),
            (coltrane_id.clone(), "John Coltrane".to_owned()),
        ]
    );
    for track in [V2_TRACK_ID, "v2-jc-2"] {
        assert_eq!(credit_row(&v3, track), coltrane_id);
    }
    assert_eq!(
        opens(&v3, V2_ARTIST_ID).as_deref(),
        Some(coltrane_id.as_str())
    );
    let legacy: String = v3
        .query_row(
            "SELECT kind FROM local_artist_aliases WHERE alias = ?1",
            params![V2_ARTIST_ID],
            |row| row.get(0),
        )
        .expect("v2 id kept as an alias");
    assert_eq!(legacy, "legacy_artist");
    let plays: Vec<(String, Option<String>)> = v3
        .prepare("SELECT id, local_artist_id FROM library_play_history ORDER BY id")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("plays");
    assert_eq!(
        plays,
        [
            ("listen-1".to_owned(), Some(coltrane_id.clone())),
            ("listen-2".to_owned(), None),
            ("listen-bags".to_owned(), Some(clairo_id.clone())),
            ("listen-jc-2".to_owned(), Some(coltrane_id.clone())),
        ]
    );
    let favorite: String = v3
        .query_row(
            "SELECT item_id FROM library_user_favorites WHERE item_kind = 'artist'",
            [],
            |row| row.get(0),
        )
        .expect("artist favorite");
    assert_eq!(favorite, coltrane_id);

    // The merged-away name lands on the survivor; both old ids open it.
    assert_eq!(credit_row(&v3, "v2-bags"), clairo_id);
    for old in [CLAIRO_ID, COTTRILL_ID] {
        assert_eq!(opens(&v3, old).as_deref(), Some(clairo_id.as_str()));
    }
}

/// A v2 row named "Unknown Artist" would key to v3's own Unknown Artist
/// row. It keeps its v2 id, the export says so, and its merge candidate
/// against that row still imports.
#[tokio::test]
async fn a_v2_unknown_artist_row_keeps_its_id() {
    let scratch = ScratchDir::new("migration-unknown-artist");
    let root = scratch.to_path_buf();
    let (v2_root, _, db) = v2_instance(&root);
    v2_artist(&db, UNKNOWN_ID, "Unknown Artist", None, None);
    v2_album(
        &db,
        "v2-larks",
        "Larks' Tongues",
        (UNKNOWN_ID, "Unknown Artist"),
    );
    v2_track(
        &db,
        "v2-larks-1",
        "v2-larks",
        "King Crimson/Larks/01.flac",
        (UNKNOWN_ID, "Unknown Artist"),
    );
    db.execute(
        "INSERT INTO local_artist_merge_candidates (id, left_artist_id, right_artist_id,
             reason_code, state, created_at, updated_at)
         VALUES ('candidate-unknown', ?1, ?2, 'FOLDED_NAME_COLLISION', 'open',
             1700000000.0, 1700000000.0)",
        params![UNKNOWN_ARTIST_ID, UNKNOWN_ID],
    )
    .expect("candidate");
    drop(db);

    let export_path = export(&root, &v2_root);
    let (report, db_path) = import(&root, &export_path).await;
    assert_eq!(
        report.notes,
        [
            "Artists kept under their v2 id because v3 keeps their name for its own artist \
          row: Unknown Artist"
        ]
    );
    assert_eq!(report.entities["artist_merge_candidate"].imported, 1);
    let v3 = Connection::open(&db_path).expect("v3 db");
    let names: Vec<(String, String)> = v3
        .prepare("SELECT id, display_name FROM local_artists WHERE display_name = 'Unknown Artist' ORDER BY id")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(
        names,
        [
            (UNKNOWN_ARTIST_ID.to_owned(), "Unknown Artist".to_owned()),
            (UNKNOWN_ID.to_owned(), "Unknown Artist".to_owned()),
        ]
    );
}

/// Bundle columns that may still hold an old artist id, and why.
const OLD_ID_KEPT: &[(&str, &str)] = &[
    // The alias is the old id itself.
    ("library_artist_alias", "alias"),
    // An opaque key; v3 never reads an artist from it.
    ("artist_merge_candidate", "id"),
    // JSON snapshots of v2's own state; v3 never looks an artist up in them.
    ("contribution_draft", "local_snapshot_json"),
    ("management_baseline", "catalog_document_json"),
];

/// After export, no bundle cell holds a re-keyed v2 artist id except the
/// columns listed above: every reference to an artist was rewritten.
#[test]
fn no_bundle_cell_keeps_a_rekeyed_artist_id() {
    let scratch = ScratchDir::new("migration-artist-cells");
    let root = scratch.to_path_buf();
    let (v2_root, alice, db) = v2_instance(&root);
    v2_artist(&db, CLAIRO_ID, "Clairo", None, None);
    v2_artist(
        &db,
        COTTRILL_ID,
        "Claire Cottrill",
        Some("Clairo"),
        Some(CLAIRO_ID),
    );
    v2_album(&db, "v2-immunity", "Immunity", (CLAIRO_ID, "Clairo"));
    v2_track(
        &db,
        "v2-alewife",
        "v2-immunity",
        "Clairo/Immunity/01.flac",
        (CLAIRO_ID, "Clairo"),
    );
    db.execute_batch(&format!(
        "INSERT INTO local_artist_external_identities (local_artist_id, provider,
             provider_artist_id, decision_source, selected_at)
         VALUES ('{CLAIRO_ID}', 'musicbrainz', 'c2a8b4e9-0000-4000-8000-000000000001',
                 'manual', 1700000000.0);
         INSERT INTO local_artist_merge_candidates (id, left_artist_id, right_artist_id,
             reason_code, state, created_at, updated_at)
         VALUES ('provider:{CLAIRO_ID}:{V2_ARTIST_ID}', '{CLAIRO_ID}', '{V2_ARTIST_ID}',
                 'SHARED_PROVIDER_IDENTITY', 'open', 1700000000.0, 1700000000.0);
         INSERT INTO library_artist_reconciliation_dismissals (left_artist_id, right_artist_id,
             left_artist_revision, right_artist_revision, dismissed_by_user_id, reason_code,
             created_at, updated_at)
         VALUES ('{V2_ARTIST_ID}', '{CLAIRO_ID}', 1, 2, '{alice}', 'DIFFERENT_ARTISTS',
                 1700000000.0, 1700000000.0);
         INSERT INTO local_artist_aliases (alias, local_artist_id, kind, created_at)
         VALUES ('{COTTRILL_ID}', '{CLAIRO_ID}', 'merged_artist', 1700000000.0);
         INSERT INTO local_entity_source_links (id, local_artist_id, provider,
             external_entity_type, external_id, canonical_url, decision_source, verified_at,
             created_at, updated_at)
         VALUES ('link-clairo', '{CLAIRO_ID}', 'wikidata', 'artist', 'Q1', 'https://w/Q1',
                 'manual', 1700000000.0, 1700000000.0, 1700000000.0);
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at)
         VALUES ('{alice}', 'artist', '{V2_ARTIST_ID}', 1700000002.0),
                ('{alice}', 'artist', '{CLAIRO_ID}', 1700000003.0);"
    ))
    .expect("references");
    drop(db);

    let export_path = export(&root, &v2_root);
    let bundle = Connection::open(root.join("export.bundle.sqlite")).expect("bundle opens");
    let old: Vec<String> = [V2_ARTIST_ID, CLAIRO_ID, COTTRILL_ID]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let tables: Vec<String> = bundle
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("tables");
    let mut found = Vec::new();
    for table in &tables {
        let columns: Vec<String> = bundle
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("columns");
        for column in columns {
            if OLD_ID_KEPT.contains(&(table.as_str(), column.as_str())) {
                continue;
            }
            for id in &old {
                let hits: i64 = bundle
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM \"{table}\" \
                             WHERE instr(CAST(\"{column}\" AS TEXT), ?1) > 0"
                        ),
                        params![id],
                        |row| row.get(0),
                    )
                    .expect("cells searched");
                if hits > 0 {
                    found.push(format!("{table}.{column} holds {id}"));
                }
            }
        }
    }
    assert!(found.is_empty(), "{found:?}");
    let ordered: bool = bundle
        .query_row(
            "SELECT left_artist_id < right_artist_id FROM artist_dismissal",
            [],
            |row| row.get(0),
        )
        .expect("dismissal");
    assert!(ordered, "a dismissal names the smaller id first");
    let aliases: i64 = bundle
        .query_row(
            "SELECT COUNT(*) FROM library_artist_alias WHERE alias = ?1",
            params![COTTRILL_ID],
            |row| row.get(0),
        )
        .expect("aliases");
    assert_eq!(aliases, 1, "v2's own alias is not added twice");
    let parsed = droppedneedle_tool::export::parse_export(
        &std::fs::read_to_string(&export_path).expect("export reads"),
    )
    .expect("export parses");
    assert_eq!(
        parsed.doc.attachments[0].sections["library_artist_alias"],
        u64::try_from(
            bundle
                .query_row("SELECT COUNT(*) FROM library_artist_alias", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .expect("count")
        )
        .expect("count fits"),
        "the declared count includes the added aliases"
    );
}
