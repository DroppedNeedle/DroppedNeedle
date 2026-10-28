//! Home and the genre page over a real database: the first visit answers
//! with the library shelves at once and builds the rest in the background,
//! the built page carries the listening shelves with ownership marked and
//! genre tiles drawn from the collection, a restart serves the built
//! shelves at once, the genre page joins owned and popular rows, and a
//! collage whose art file is gone falls back to a gradient.

use std::sync::Arc;
use std::time::{Duration, Instant};

use droppedneedle_catalog::reads::discover::genre::GenrePages;
use droppedneedle_catalog::reads::discover::genre::artwork::GenreArt;
use droppedneedle_catalog::reads::discover::home::library::HomeLibrary;
use droppedneedle_catalog::reads::discover::home::{HomeInputs, LiveHome};
use droppedneedle_catalog::reads::discover::listening::{
    ArtistRow, ListenedAlbum, ListeningData, LovedTrack, TagAlbum, UserMusic,
};
use droppedneedle_catalog::reads::discover::models::{
    ArtworkKind, PromptService, SectionItem, SectionKey,
};
use droppedneedle_catalog::reads::discover::page::shelves::shelf_of;
use droppedneedle_kernel::clock::ManualClock;
use droppedneedle_kernel::db::{DbError, Lane};

use crate::discover_queue::Script;

const OWNED_RG: &str = "0a1b2c3d-0000-4000-8000-000000000001";
const OTHER_RG: &str = "0a1b2c3d-0000-4000-8000-000000000002";
const OWNED_ARTIST: &str = "0a1b2c3d-0000-4000-8000-0000000000a1";
const OTHER_ARTIST: &str = "0a1b2c3d-0000-4000-8000-0000000000a2";

fn artist(name: &str, mbid: &str) -> ArtistRow {
    ArtistRow {
        name: name.to_owned(),
        mbid: Some(mbid.to_owned()),
        listen_count: 10,
    }
}

/// A ListenBrainz user who listens to shoegaze.
fn home_script() -> Script {
    Script {
        user: UserMusic {
            listenbrainz: Some("ada".to_owned()),
            ..UserMusic::default()
        },
        sitewide_artists: vec![artist("Everyone", OTHER_ARTIST)],
        sitewide_albums: vec![ListenedAlbum {
            release_group_mbid: Some(OWNED_RG.to_owned()),
            title: "First Light".to_owned(),
            artist_name: "Aurora".to_owned(),
            artist_mbid: Some(OWNED_ARTIST.to_owned()),
            listen_count: 900,
        }],
        loved: vec![LovedTrack {
            name: "Opener".to_owned(),
            artist_name: "Aurora".to_owned(),
            artist_mbid: None,
            album_name: Some("First Light".to_owned()),
            recording_mbid: None,
            image_url: None,
        }],
        genres: vec![("Shoegaze".to_owned(), 40)],
        tag_artists: vec![
            artist("Aurora", OWNED_ARTIST),
            artist("Everyone", OTHER_ARTIST),
        ],
        tag_albums: vec![TagAlbum {
            release_group_mbid: OTHER_RG.to_owned(),
            title: "Elsewhere".to_owned(),
            artist_name: "Everyone".to_owned(),
            artist_mbid: None,
            year: Some(1991),
        }],
        ..Script::default()
    }
}

#[tokio::test]
async fn home_and_genre_pages_build_from_library_and_listening() {
    let dir = crate::common::ScratchDir::new("home-page");
    let cover = dir.join("cover.png");
    std::fs::write(&cover, b"\x89PNG\r\n\x1a\nnot really").expect("cover written");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let seed = format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', '2026-01-01T00:00:00Z'); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'Aurora', 'aurora', 'group', 1, 1); \
         INSERT INTO local_artist_external_identities (local_artist_id, provider, \
         provider_artist_id, decision_source, selected_at) \
         VALUES ('a1', 'musicbrainz', '{OWNED_ARTIST}', 'manual', 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
         created_at, updated_at, year) VALUES ('al1', 'r1', 'g1', 'First Light', 'first light', \
         'Aurora', 'aurora', 'a1', 'automatic', 1, 1, 1990); \
         INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
         VALUES ('al1', 0, 'a1', 'main'); \
         INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at) \
         VALUES ('al1', 'musicbrainz', '{OWNED_RG}', 'manual', 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, availability, ingest_source, \
         imported_at, membership_source) VALUES ('t1', 'al1', 'r1', '{track}', 't1.flac', \
         'h1', 1, 1, 's1', 'Opener', 'opener', 'First Light', 'first light', 'flac', 'indexed', \
         'scan', 1, 'automatic'); \
         INSERT INTO local_track_genres (local_track_id, position, name, folded_name, source) \
         VALUES ('t1', 0, 'Shoegaze', 'shoegaze', 'local'); \
         INSERT INTO local_album_artwork (local_album_id, source, source_locator, content_hash, \
         version, updated_at) VALUES ('al1', 'folder', '{}', 'abc123', 3, 1); \
         INSERT INTO library_play_history (id, user_id, track_name, artist_name, album_name, \
         played_at) VALUES ('p1', 'user-1', 'Opener', 'Aurora', 'First Light', \
         '2026-10-01T10:00:00Z');",
        cover.display(),
        track = dir.join("t1.flac").display()
    );
    runtime
        .db()
        .write(Lane::Foreground, "home seed", move |tx| {
            tx.execute_batch(&seed).map_err(DbError::from)
        })
        .await
        .expect("seed");
    let pool = runtime.db().sqlx().clone();
    let listening = ListeningData::new(
        Arc::new(home_script()),
        droppedneedle_providers::Providers::closed().cache,
    );
    let art = Arc::new(GenreArt::new(HomeLibrary::new(pool.clone())));
    let home_over = || {
        LiveHome::new(HomeInputs {
            listening: listening.clone(),
            db: runtime.db().clone(),
            clock: Arc::new(ManualClock::at(1_790_000_000_000)),
            integrations: droppedneedle_catalog::reads::discover::content::IntegrationSettings::new(
                &crate::common::config_store(&dir),
                Arc::new(crate::common::bundles::NoDownloadSource),
            ),
            art: art.clone(),
            warm: None,
            discover: None,
        })
    };
    let home = home_over();

    // First visit: library shelves at once, the rest building.
    let first = home.page("user-1").await;
    assert!(first.refreshing);
    let Some(SectionItem::Album(recent)) =
        shelf_of(&first.sections, SectionKey::RecentlyAdded).and_then(|shelf| shelf.items.first())
    else {
        panic!("recently added holds the album");
    };
    assert_eq!(recent.local_id.as_deref(), Some("al1"));
    assert_eq!(
        recent.cover_url.as_deref(),
        Some("/api/v3/library/albums/al1/artwork?size=500&v=3")
    );
    assert!(shelf_of(&first.sections, SectionKey::RecentlyPlayed).is_some());
    assert!(shelf_of(&first.sections, SectionKey::TrendingArtists).is_none());

    // Each check hands the runtime to the build until it is kept.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut page = first;
    while page.refreshing {
        assert!(Instant::now() < deadline, "the build never finished");
        tokio::task::yield_now().await;
        page = home.page("user-1").await;
    }
    assert!(shelf_of(&page.sections, SectionKey::TrendingArtists).is_some());
    assert!(shelf_of(&page.sections, SectionKey::FavoriteArtists).is_some());
    let Some(SectionItem::Album(popular)) =
        shelf_of(&page.sections, SectionKey::PopularAlbums).and_then(|shelf| shelf.items.first())
    else {
        panic!("popular albums built");
    };
    assert!(popular.in_library, "owned albums are marked");
    assert_eq!(popular.local_id.as_deref(), Some("al1"));
    let tile = page.genre_artwork.get("Shoegaze").expect("genre tile art");
    assert_eq!(tile.kind, ArtworkKind::Collage);
    assert_eq!(tile.albums[0].album_id, "al1");
    assert_eq!(tile.albums[0].cover_version, 3);
    assert!(
        page.service_prompts
            .iter()
            .any(|p| p.service == PromptService::DownloadClient)
    );

    // A restart serves the built shelves at once, without a build.
    let restarted = home_over().page("user-1").await;
    assert!(!restarted.refreshing, "the saved shelves are fresh");
    assert!(shelf_of(&restarted.sections, SectionKey::TrendingArtists).is_some());
    assert!(shelf_of(&restarted.sections, SectionKey::RecentlyPlayed).is_some());

    // The genre page: owned and popular rows side by side.
    let genre = GenrePages::new(pool.clone(), listening, art.clone(), None)
        .detail("Shoegaze", 50, 0, 0)
        .await;
    let library = genre.library.expect("library lane");
    assert_eq!(library.albums[0].local_id.as_deref(), Some("al1"));
    assert_eq!(library.artists[0].listen_count, Some(1));
    let popular = genre.popular.expect("popular lane");
    assert!(popular.artists[0].in_library);
    assert!(!popular.artists[1].in_library);
    assert_eq!(popular.albums[0].release_date.as_deref(), Some("1991"));
    assert!(!popular.has_more_artists);
    assert_eq!(genre.genre_artwork.kind, ArtworkKind::Collage);

    // Art the scan finds gone is not shown again: dropping its row bumps
    // the genre's revision, so the remembered collage is replaced.
    std::fs::remove_file(&cover).expect("cover removed");
    runtime
        .db()
        .write(Lane::Foreground, "art gone", |tx| {
            tx.execute(
                "DELETE FROM local_album_artwork WHERE local_album_id = 'al1'",
                [],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("art row removed");
    assert_eq!(art.one("Shoegaze").await.kind, ArtworkKind::Gradient);
}
