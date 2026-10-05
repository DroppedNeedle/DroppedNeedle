//! Compat over the real catalog: Subsonic and Jellyfin read the scanned
//! library from SQLite, and playlists and favorites are one set of rows
//! shared with the native routes and the Navidrome m3u export.

use std::collections::HashMap;
use std::sync::Arc;

use droppedneedle::compat::CompatSetup;
use droppedneedle::compat::jellyfin::params::SortKey;
use droppedneedle::compat::jellyfin::seams::{
    AlbumFilter, ItemSort, LibraryRead as _, TrackFilter,
};
use droppedneedle::compat::subsonic::fake::{FakeAudio, FakeVerifier, NOW_UNIX, USER_ID};
use droppedneedle::compat::subsonic::params::SubsonicParameters;
use droppedneedle::compat::subsonic::{Request, Settings, dispatch};
use droppedneedle::db::{DbConfig, DbRuntime, Lane, open_runtime};
use droppedneedle::ids::UuidGenerator;
use droppedneedle::reads::ReadsSetup;
use droppedneedle::reads::collections::db::CollectionsDb;
use droppedneedle::reads::collections::models::{AddTracksBody, CreatePlaylistBody, TrackInput};
use droppedneedle::reads::collections::service::CollectionsService;

use crate::common::ScratchDir;

const SEED: &str = "INSERT INTO auth_users (id, display_name, role, created_at) \
    VALUES ('u1', 'User', 'user', '2024-01-01T00:00:00Z'), \
    ('u2', 'Other', 'user', '2024-01-01T00:00:00Z'); \
    INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
    VALUES ('art-1', 'Portishead', 'portishead', 'group', 1, 1); \
    INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_name, \
    album_artist_name_folded, album_artist_id, year, grouping_source, created_at, updated_at) \
    VALUES ('alb-1', 'root', 'k1', 'Dummy', 'dummy', 'Portishead', 'portishead', 'art-1', 1994, \
    'automatic', 1, 1); \
    INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
    file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, artist_name, \
    artist_name_folded, album_title, album_title_folded, album_artist_name, \
    album_artist_name_folded, track_number, genre, genre_folded, duration_seconds, \
    file_format, ingest_source, imported_at, membership_source) \
    VALUES ('trk-1', 'alb-1', 'root', '/music/dummy/01.flac', 'dummy/01.flac', 'h1', 1000, 1, \
    'r1', 'Mysterons', 'mysterons', 'Portishead', 'portishead', 'Dummy', 'dummy', 'Portishead', \
    'portishead', 1, 'Trip Hop', 'trip hop', 305.0, 'flac', 'scan', 1, 'automatic'); \
    INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
    VALUES ('trk-1', 0, 'art-1', 'main');";

struct Rig {
    _dir: ScratchDir,
    runtime: DbRuntime,
    compat: CompatSetup,
    reads: ReadsSetup,
}

impl Rig {
    async fn open() -> Self {
        let dir = ScratchDir::new("compat-catalog");
        let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
            .await
            .unwrap();
        runtime
            .lane()
            .write(Lane::Foreground, "seed", |tx| {
                tx.execute_batch(SEED)?;
                Ok(())
            })
            .await
            .unwrap();
        let auth = droppedneedle::auth::wiring::AuthSetup::for_tests().unwrap();
        let ids: Arc<dyn droppedneedle::ids::IdGenerator> = Arc::new(UuidGenerator);
        let reads = ReadsSetup::build(
            runtime.pool(),
            auth.users.clone(),
            ids.clone(),
            String::new(),
            None,
        )
        .with_collections(CollectionsDb::new(
            runtime.pool().clone(),
            runtime.lane().clone(),
        ));
        let library = droppedneedle::library::wiring::LibrarySetup::for_tests(
            auth.users.clone(),
            ids.clone(),
        )
        .unwrap();
        let compat = CompatSetup::for_tests(auth.users.clone(), library, &reads, ids).unwrap();
        Self {
            _dir: dir,
            runtime,
            compat,
            reads,
        }
    }

    async fn subsonic(&self, endpoint: &str, pairs: &[(&str, &str)]) -> String {
        let mut params = vec![
            ("u".to_owned(), "user".to_owned()),
            ("p".to_owned(), "secret".to_owned()),
            ("f".to_owned(), "json".to_owned()),
        ];
        params.extend(
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        let request = Request {
            method: "GET".to_owned(),
            endpoint: endpoint.to_owned(),
            params: SubsonicParameters::new(params),
            headers: HashMap::new(),
            body: Vec::new(),
            content_type: None,
            now_unix: Some(NOW_UNIX),
        };
        let settings = Settings {
            enabled: true,
            ..Settings::default()
        };
        let rendered = dispatch(
            &FakeVerifier,
            &self.compat.subsonic_store(),
            &FakeAudio,
            &settings,
            &request,
        )
        .await;
        String::from_utf8(rendered.body).unwrap()
    }
}

#[tokio::test]
async fn subsonic_and_jellyfin_read_the_scanned_library() {
    let rig = Rig::open().await;

    let album = rig.subsonic("getAlbum", &[("id", "al-alb-1")]).await;
    assert!(album.contains("\"title\":\"Mysterons\""), "{album}");
    assert!(album.contains("\"id\":\"tr-trk-1\""), "{album}");
    let artists = rig.subsonic("getArtists", &[]).await;
    assert!(artists.contains("Portishead"), "{artists}");
    let found = rig.subsonic("search3", &[("query", "myster")]).await;
    assert!(found.contains("tr-trk-1"), "{found}");
    let genre = rig
        .subsonic("getSongsByGenre", &[("genre", "Trip Hop")])
        .await;
    assert!(genre.contains("tr-trk-1"), "{genre}");

    let jellyfin = rig.compat.jellyfin_state();
    let search = TrackFilter {
        search: Some("myster".to_owned()),
        ..TrackFilter::default()
    };
    let (tracks, total) = jellyfin
        .library
        .track_page(USER_ID, &search, ItemSort::Catalog, 0, 10)
        .await;
    assert_eq!((total, tracks[0].title.as_str()), (1, "Mysterons"));
    let (albums, total) = jellyfin
        .library
        .album_page(
            USER_ID,
            &AlbumFilter::default(),
            ItemSort::By(SortKey::Recent, true),
            0,
            1,
        )
        .await;
    assert_eq!((total, albums[0].title.as_str()), (1, "Dummy"));
    let (played, total) = jellyfin
        .library
        .track_page(
            USER_ID,
            &TrackFilter::default(),
            ItemSort::By(SortKey::PlayCount, true),
            0,
            10,
        )
        .await;
    assert_eq!((played.len(), total), (0, 0), "history sorts skip unplayed");
}

#[tokio::test]
async fn playlists_and_stars_are_shared_with_the_native_routes() {
    let rig = Rig::open().await;
    let native = CollectionsService::new(&rig.reads.collections);

    // A playlist made in a Subsonic client shows in the web UI.
    let created = rig
        .subsonic(
            "createPlaylist",
            &[("name", "From Symfonium"), ("songId", "tr-trk-1")],
        )
        .await;
    assert!(created.contains("tr-trk-1"), "{created}");
    let detail = native.list_playlists(USER_ID).await.unwrap();
    let json = serde_json::to_value(&detail).unwrap();
    assert_eq!(json["playlists"][0]["name"], "From Symfonium");
    assert_eq!(json["playlists"][0]["track_count"], 1);

    // A playlist made in the web UI with a local track streams in Subsonic.
    let made = native
        .create_playlist(
            USER_ID,
            &CreatePlaylistBody {
                name: "From the web".to_owned(),
                source_ref: None,
            },
        )
        .await
        .unwrap();
    native
        .add_tracks(
            USER_ID,
            &made.id,
            &AddTracksBody {
                tracks: vec![TrackInput {
                    track_name: "Mysterons".to_owned(),
                    artist_name: "Portishead".to_owned(),
                    album_name: "Dummy".to_owned(),
                    album_id: None,
                    artist_id: None,
                    track_source_id: Some("trk-1".to_owned()),
                    cover_url: None,
                    source_type: "local".to_owned(),
                    available_sources: None,
                    format: None,
                    track_number: None,
                    disc_number: None,
                    duration: None,
                    plex_rating_key: None,
                }],
                position: None,
            },
        )
        .await
        .unwrap();
    let listed = rig
        .subsonic("getPlaylist", &[("id", &format!("pl-{}", made.id))])
        .await;
    assert!(listed.contains("\"songCount\":1"), "{listed}");

    // A star from a client is a heart in the web UI.
    rig.subsonic("star", &[("id", "al-alb-1")]).await;
    let hearts = native.list_favorites(USER_ID, Some("album")).await.unwrap();
    assert_eq!(hearts.items[0].item_id, "alb-1");
    let starred = rig.subsonic("getStarred2", &[]).await;
    assert!(starred.contains("al-alb-1"), "{starred}");

    // Queues persist in the database.
    rig.subsonic(
        "savePlayQueue",
        &[("id", "tr-trk-1"), ("current", "tr-trk-1")],
    )
    .await;
    let saved: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM compat_play_queue_items WHERE user_id = 'u1'")
            .fetch_one(rig.runtime.pool())
            .await
            .unwrap();
    assert_eq!(saved, 1);
}

#[tokio::test]
async fn playlist_refusals_keep_their_codes_and_repeats_stay() {
    let rig = Rig::open().await;
    let native = CollectionsService::new(&rig.reads.collections);
    let theirs = native
        .create_playlist(
            "u2",
            &CreatePlaylistBody {
                name: "Theirs".to_owned(),
                source_ref: None,
            },
        )
        .await
        .unwrap();
    let id = format!("pl-{}", theirs.id);

    // Private to someone else: not found (70), never a server fault.
    let hidden = rig.subsonic("getPlaylist", &[("id", &id)]).await;
    assert!(hidden.contains("\"code\":70"), "{hidden}");
    // Public but not ours to change: not authorized (50).
    native.set_visibility("u2", &theirs.id, true).await.unwrap();
    let refused = rig
        .subsonic("updatePlaylist", &[("playlistId", &id), ("name", "Mine")])
        .await;
    assert!(refused.contains("\"code\":50"), "{refused}");

    // The same song asked twice is added twice (v2 appends each).
    let created = rig.subsonic("createPlaylist", &[("name", "Twice")]).await;
    let ours: serde_json::Value = serde_json::from_str(&created).unwrap();
    let ours = ours["subsonic-response"]["playlist"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.subsonic(
        "updatePlaylist",
        &[
            ("playlistId", &ours),
            ("songIdToAdd", "tr-trk-1"),
            ("songIdToAdd", "tr-trk-1"),
        ],
    )
    .await;
    let listed = rig.subsonic("getPlaylist", &[("id", &ours)]).await;
    assert!(listed.contains("\"songCount\":2"), "{listed}");
}

/// The Navidrome m3u export reads the playlists the web UI saves: a local
/// entry resolves to its file, and a fractional duration (the web player
/// sends float seconds) does not make the playlist read fail.
#[tokio::test]
async fn native_playlists_export_to_m3u_with_their_files() {
    use droppedneedle::jobs::playlist_export::M3uPlaylistExporter;
    use droppedneedle::jobs::playlist_sync::{PlaylistExporter as _, PlaylistSyncConfig};

    let rig = Rig::open().await;
    let native = CollectionsService::new(&rig.reads.collections);
    let made = native
        .create_playlist(
            USER_ID,
            &CreatePlaylistBody {
                name: "Export me".to_owned(),
                source_ref: None,
            },
        )
        .await
        .unwrap();
    native
        .add_tracks(
            USER_ID,
            &made.id,
            &AddTracksBody {
                tracks: vec![TrackInput {
                    track_name: "Mysterons".to_owned(),
                    artist_name: "Portishead".to_owned(),
                    album_name: "Dummy".to_owned(),
                    album_id: None,
                    artist_id: None,
                    track_source_id: Some("trk-1".to_owned()),
                    cover_url: None,
                    source_type: "local".to_owned(),
                    available_sources: None,
                    format: None,
                    track_number: None,
                    disc_number: None,
                    duration: Some(305.6),
                    plex_rating_key: None,
                }],
                position: None,
            },
        )
        .await
        .unwrap();

    let out = ScratchDir::new("m3u-export");
    let folder = out.join("playlists");
    let report = M3uPlaylistExporter::new(Some(rig.runtime.pool().clone()))
        .sync(PlaylistSyncConfig {
            target_dir: folder.display().to_string(),
            scope: "all".to_owned(),
            remove_deleted: true,
        })
        .await;
    assert!(report.success, "{}", report.message);
    assert_eq!(report.written, 1, "{}", report.message);
    let file = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "m3u8"))
        .unwrap();
    let text = std::fs::read_to_string(file).unwrap();
    assert!(
        text.contains("#EXTINF:305,Portishead - Mysterons"),
        "{text}"
    );
    assert!(text.contains("music/dummy/01.flac"), "{text}");
}
