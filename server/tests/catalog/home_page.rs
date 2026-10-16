//! Home and the genre page over a real database: the first visit answers
//! with the library shelves at once and builds the rest in the background,
//! the built page carries the listening shelves with ownership marked and
//! genre tiles drawn from the collection, the genre page joins owned and
//! popular rows, and a collage whose art file is gone falls back to a
//! gradient.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_catalog::reads::discover::adapters::home::LiveHome;
use droppedneedle_catalog::reads::discover::adapters::home::artwork::GenreArt;
use droppedneedle_catalog::reads::discover::adapters::home::genre::GenrePages;
use droppedneedle_catalog::reads::discover::adapters::home::library::HomeLibrary;
use droppedneedle_catalog::reads::discover::adapters::home::sources::{
    HomeSources, LovedTrack, TagAlbum,
};
use droppedneedle_catalog::reads::discover::adapters::page::sources::{
    LastFmChartAlbum, RankedAlbum,
};
use droppedneedle_catalog::reads::discover::adapters::queue::sources::{
    AlbumRow, ArtistRow, SourceResult, UserMusic,
};
use droppedneedle_catalog::reads::discover::models::{SectionItem, WeeklyExploration};
use droppedneedle_catalog::reads::discover::ports::BoxFuture;
use droppedneedle_kernel::db::{Lane, OpError};

const OWNED_RG: &str = "0a1b2c3d-0000-4000-8000-000000000001";
const OTHER_RG: &str = "0a1b2c3d-0000-4000-8000-000000000002";
const OWNED_ARTIST: &str = "0a1b2c3d-0000-4000-8000-0000000000a1";
const OTHER_ARTIST: &str = "0a1b2c3d-0000-4000-8000-0000000000a2";

fn ok<T: Send + 'static>(value: T) -> BoxFuture<'static, SourceResult<T>> {
    Box::pin(async move { Ok(value) })
}

fn artist(name: &str, mbid: &str) -> ArtistRow {
    ArtistRow {
        name: name.to_owned(),
        mbid: Some(mbid.to_owned()),
        listen_count: 10,
    }
}

/// A ListenBrainz user who listens to shoegaze.
struct Scripted;

impl HomeSources for Scripted {
    fn source_key(&self) -> String {
        "official".to_owned()
    }
    fn user_music<'a>(&'a self, _user: &'a str) -> BoxFuture<'a, UserMusic> {
        Box::pin(async {
            UserMusic {
                listenbrainz: Some("ada".to_owned()),
                ..UserMusic::default()
            }
        })
    }
    fn listenbrainz_trending_artists(
        &self,
        _c: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<ArtistRow>>> {
        ok(vec![artist("Everyone", OTHER_ARTIST)])
    }
    fn listenbrainz_popular_albums(
        &self,
        _c: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<RankedAlbum>>> {
        ok(vec![RankedAlbum {
            album: AlbumRow {
                release_group_mbid: OWNED_RG.to_owned(),
                title: "First Light".to_owned(),
                artist_name: "Aurora".to_owned(),
                artist_mbid: Some(OWNED_ARTIST.to_owned()),
            },
            listen_count: 900,
        }])
    }
    fn listenbrainz_user_albums<'a>(
        &'a self,
        _u: &'a str,
        _c: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>> {
        ok(Vec::new())
    }
    fn listenbrainz_loved<'a>(
        &'a self,
        _u: &'a str,
        _c: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LovedTrack>>> {
        ok(vec![LovedTrack {
            name: "Opener".to_owned(),
            artist_name: "Aurora".to_owned(),
            artist_mbid: None,
            album_name: Some("First Light".to_owned()),
            recording_mbid: None,
            image_url: None,
        }])
    }
    fn listenbrainz_genres<'a>(
        &'a self,
        _u: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<(String, i64)>>> {
        ok(vec![("Shoegaze".to_owned(), 40)])
    }
    fn listenbrainz_weekly<'a>(
        &'a self,
        _user: &'a str,
        _u: &'a str,
    ) -> BoxFuture<'a, Option<WeeklyExploration>> {
        Box::pin(async { None })
    }
    fn lastfm_trending_artists<'a>(
        &'a self,
        _user: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn lastfm_user_albums<'a>(
        &'a self,
        _user: &'a str,
        _u: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>> {
        ok(Vec::new())
    }
    fn lastfm_loved<'a>(
        &'a self,
        _user: &'a str,
        _u: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LovedTrack>>> {
        ok(Vec::new())
    }
    fn musicbrainz_tag_artists<'a>(
        &'a self,
        _t: &'a str,
        _l: u32,
        _o: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(vec![
            artist("Aurora", OWNED_ARTIST),
            artist("Everyone", OTHER_ARTIST),
        ])
    }
    fn musicbrainz_tag_albums<'a>(
        &'a self,
        _t: &'a str,
        _l: u32,
        _o: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<TagAlbum>>> {
        ok(vec![TagAlbum {
            release_group_mbid: OTHER_RG.to_owned(),
            title: "Elsewhere".to_owned(),
            artist_name: "Everyone".to_owned(),
            year: Some(1991),
        }])
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
         imported_at, membership_source) VALUES ('t1', 'al1', 'r1', '/music/t1.flac', 't1.flac', \
         'h1', 1, 1, 's1', 'Opener', 'opener', 'First Light', 'first light', 'flac', 'indexed', \
         'scan', 1, 'automatic'); \
         INSERT INTO local_track_genres (local_track_id, position, name, folded_name, source) \
         VALUES ('t1', 0, 'Shoegaze', 'shoegaze', 'local'); \
         INSERT INTO local_album_artwork (local_album_id, source, source_locator, content_hash, \
         version, updated_at) VALUES ('al1', 'folder', '{}', 'abc123', 3, 1); \
         INSERT INTO library_play_history (id, user_id, track_name, artist_name, album_name, \
         played_at) VALUES ('p1', 'user-1', 'Opener', 'Aurora', 'First Light', \
         '2026-10-01T10:00:00Z');",
        cover.display()
    );
    runtime
        .lane()
        .write(Lane::Foreground, "home seed", move |tx| {
            tx.execute_batch(&seed).map_err(OpError::from)?;
            Ok(())
        })
        .await
        .expect("seed");
    let pool = runtime.pool().clone();
    let sources: Arc<dyn HomeSources> = Arc::new(Scripted);
    let art = Arc::new(GenreArt::new(HomeLibrary::new(pool.clone())));
    let home = LiveHome::new(
        sources.clone(),
        pool.clone(),
        crate::common::config_store(&dir),
        art.clone(),
        None,
    );

    // First visit: library shelves at once, the rest building.
    let first = home.page("user-1").await;
    assert!(first.refreshing);
    let Some(SectionItem::Album(recent)) = first
        .recently_added
        .as_ref()
        .and_then(|shelf| shelf.items.first())
    else {
        panic!("recently added holds the album");
    };
    assert_eq!(recent.local_id.as_deref(), Some("al1"));
    assert_eq!(
        recent.image_url.as_deref(),
        Some("/api/v3/library/albums/al1/artwork?size=500&v=3")
    );
    assert!(first.recently_played.is_some());
    assert!(first.trending_artists.is_none());

    let mut page = first;
    for _ in 0..250 {
        if !page.refreshing {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        page = home.page("user-1").await;
    }
    assert!(!page.refreshing, "the build finished");
    assert!(page.trending_artists.is_some());
    assert!(page.favorite_artists.is_some());
    let Some(SectionItem::Album(popular)) = page
        .popular_albums
        .as_ref()
        .and_then(|shelf| shelf.items.first())
    else {
        panic!("popular albums built");
    };
    assert!(popular.in_library, "owned albums are marked");
    assert_eq!(popular.local_id.as_deref(), Some("al1"));
    let tile = page.genre_artwork.get("Shoegaze").expect("genre tile art");
    assert_eq!(tile.kind, "collage");
    assert_eq!(tile.albums[0].album_id, "al1");
    assert_eq!(tile.albums[0].cover_version, 3);
    assert!(
        page.service_prompts
            .iter()
            .any(|p| p.service == "download-client")
    );

    // The genre page: owned and popular rows side by side.
    let genre = GenrePages::new(pool.clone(), sources, art.clone(), None)
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
    assert_eq!(genre.genre_artwork.kind, "collage");

    // Art the scan finds gone is not shown again: dropping its row bumps
    // the genre's revision, so the remembered collage is replaced.
    std::fs::remove_file(&cover).expect("cover removed");
    runtime
        .lane()
        .write(Lane::Foreground, "art gone", |tx| {
            tx.execute(
                "DELETE FROM local_album_artwork WHERE local_album_id = 'al1'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("art row removed");
    assert_eq!(art.one("Shoegaze").await.kind, "gradient");
}
