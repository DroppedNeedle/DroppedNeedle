//! Home and the genre page over a real database: the first visit answers
//! with the library shelves at once and builds the rest in the background,
//! the built page carries the listening shelves with ownership marked and
//! genre tiles drawn from the collection, a restart serves the built
//! shelves at once, the genre page joins owned and popular rows, and a
//! collage whose art file is gone falls back to a gradient. A release
//! group the sitewide chart lists twice is one popular album. Your Top
//! Albums comes from plays in the app while there are enough of them. The
//! Discover teaser is the week's next pick, and Milestone Anniversaries is a
//! Home shelf.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use droppedneedle_catalog::discover::facts::KeptFacts;
use droppedneedle_catalog::discover::genre::GenrePages;
use droppedneedle_catalog::discover::genre::artwork::GenreArt;
use droppedneedle_catalog::discover::home::library::HomeLibrary;
use droppedneedle_catalog::discover::home::{HomeInputs, LiveHome};
use droppedneedle_catalog::discover::listening::{
    ArtistRow, ListenedAlbum, ListeningData, LovedTrack, TagAlbum, UserMusic,
};
use droppedneedle_catalog::discover::models::{
    ArtworkKind, PromptService, SectionItem, SectionKey,
};
use droppedneedle_catalog::discover::page::shelves::shelf_of;
use droppedneedle_catalog::discover::page::{DiscoverPages, PageInputs};
use droppedneedle_catalog::discover::queue::store::QueueDb;
use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::test_support::ManualClock;

use crate::discover_queue::{Script, after_commits};

const OWNED_RG: &str = "0a1b2c3d-0000-4000-8000-000000000001";
const OTHER_RG: &str = "0a1b2c3d-0000-4000-8000-000000000002";
const OWNED_ARTIST: &str = "0a1b2c3d-0000-4000-8000-0000000000a1";
const OTHER_ARTIST: &str = "0a1b2c3d-0000-4000-8000-0000000000a2";

fn artist(name: &str, mbid: &str) -> ArtistRow {
    ArtistRow {
        name: name.to_owned(),
        mbid: Some(mbid.to_owned()),
        listen_count: Some(10),
    }
}

/// A ListenBrainz user who listens to shoegaze. The sitewide chart lists
/// one release group twice, as ListenBrainz's can.
fn home_script() -> Script {
    Script {
        user: UserMusic {
            listenbrainz: Some("ada".to_owned()),
            ..UserMusic::default()
        },
        sitewide_artists: vec![artist("Everyone", OTHER_ARTIST)],
        sitewide_albums: vec![
            ListenedAlbum {
                release_group_mbid: Some(OWNED_RG.to_owned()),
                title: "First Light".to_owned(),
                artist_name: "Aurora".to_owned(),
                artist_mbid: Some(OWNED_ARTIST.to_owned()),
                listen_count: 400,
            },
            ListenedAlbum {
                release_group_mbid: Some(OWNED_RG.to_owned()),
                title: "First Light".to_owned(),
                artist_name: "Aurora".to_owned(),
                artist_mbid: Some(OWNED_ARTIST.to_owned()),
                listen_count: 900,
            },
        ],
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
         VALUES ('user-1', 'Ada', 'user', 0); \
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
         1790848800000);",
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
    let db = runtime.db().clone();
    let listening = ListeningData::new(
        Arc::new(home_script()),
        droppedneedle_providers::Providers::closed().cache,
        KeptFacts::new(db.clone(), Arc::new(SystemClock)),
    );
    let art = Arc::new(GenreArt::new(HomeLibrary::new(db.clone())));
    let settings = crate::common::config_store(&dir);
    let integrations = || {
        droppedneedle_catalog::discover::content::IntegrationSettings::new(
            &settings,
            Arc::new(crate::common::bundles::NoDownloadSource),
        )
    };
    let warm =
        crate::common::bundles::cover_warm_at(&db, Arc::new(ManualClock::at(1_790_000_000_000)));
    let home_over = || {
        let clock = Arc::new(ManualClock::at(1_790_000_000_000));
        LiveHome::new(HomeInputs {
            listening: listening.clone(),
            db: runtime.db().clone(),
            clock: clock.clone(),
            integrations: integrations(),
            art: art.clone(),
            warm: warm.clone(),
            discover: DiscoverPages::new(PageInputs {
                listening: listening.clone(),
                queue_db: QueueDb::new(db.clone()),
                db: db.clone(),
                clock,
                advanced: settings.live(),
                integrations: integrations(),
                warm: warm.clone(),
                forgets: Default::default(),
            }),
            forgets: Default::default(),
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

    // The build ends by saving its shelves, a commit.
    let page = after_commits(&db, "the home build kept its shelves", || async {
        let page = home.page("user-1").await;
        shelf_of(&page.sections, SectionKey::TrendingArtists)
            .is_some()
            .then_some(page)
    })
    .await;
    assert!(shelf_of(&page.sections, SectionKey::FavoriteArtists).is_some());
    let popular_shelf = shelf_of(&page.sections, SectionKey::PopularAlbums);
    assert_eq!(
        popular_shelf.map(|shelf| shelf.items.len()),
        Some(1),
        "one release group is one popular album"
    );
    let Some(SectionItem::Album(popular)) = popular_shelf.and_then(|shelf| shelf.items.first())
    else {
        panic!("popular albums built");
    };
    assert_eq!(popular.listen_count, Some(900), "with its higher count");
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
    let genre = GenrePages::new(db.clone(), listening, art.clone(), warm)
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

/// Home over a ListenBrainz user whose weekly playlist is dated `date`,
/// on a clock at 2026-09-21 15:33 UTC: whether the built shelves hold it.
async fn home_shows_weekly(date: &str) -> bool {
    let dir = crate::common::ScratchDir::new("home-weekly");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let db = runtime.db().clone();
    db.write(Lane::Foreground, "home seed", |tx| {
        tx.execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('user-1', 'Ada', 'user', 0)",
            [],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .expect("seed");
    let listening = ListeningData::new(
        Arc::new(Script {
            weekly: Some(crate::discover_page::weekly_playlist(date)),
            ..home_script()
        }),
        droppedneedle_providers::Providers::closed().cache,
        KeptFacts::new(db.clone(), Arc::new(SystemClock)),
    );
    let settings = crate::common::config_store(&dir);
    let integrations = || {
        droppedneedle_catalog::discover::content::IntegrationSettings::new(
            &settings,
            Arc::new(crate::common::bundles::NoDownloadSource),
        )
    };
    let clock = Arc::new(ManualClock::at(1_790_000_000_000));
    let warm = crate::common::bundles::cover_warm_at(&db, clock.clone());
    let home = LiveHome::new(HomeInputs {
        listening: listening.clone(),
        db: db.clone(),
        clock: clock.clone(),
        integrations: integrations(),
        art: Arc::new(GenreArt::new(HomeLibrary::new(db.clone()))),
        warm: warm.clone(),
        discover: DiscoverPages::new(PageInputs {
            listening,
            queue_db: QueueDb::new(db.clone()),
            db: db.clone(),
            clock,
            advanced: settings.live(),
            integrations: integrations(),
            warm,
            forgets: Default::default(),
        }),
        forgets: Default::default(),
    });
    home.page("user-1").await;
    let page = after_commits(&db, "the home build kept its shelves", || async {
        let page = home.page("user-1").await;
        shelf_of(&page.sections, SectionKey::TrendingArtists)
            .is_some()
            .then_some(page)
    })
    .await;
    page.sections
        .iter()
        .any(|section| section.key == SectionKey::WeeklyExploration)
}

/// Home over a ListenBrainz user who played the first `albums` of four
/// library albums in the app during the last 30 days (the first most), and
/// the fourth 40 days ago: its built Your Top Albums shelf, as (source,
/// album ids), and how often ListenBrainz was asked for their top albums.
async fn top_albums_after_plays(albums: usize) -> ((String, Vec<String>), usize) {
    let dir = crate::common::ScratchDir::new("home-top-albums");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let db = runtime.db().clone();
    let now = 1_790_000_000_000_i64;
    let day = 86_400_000;
    let mut seed = String::from(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('a1', 'Aurora', 'aurora', 'group', 1, 1);",
    );
    for n in 1..=4 {
        seed.push_str(&format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) \
             VALUES ('al{n}', 'r1', 'g{n}', 'Album {n}', 'album {n}', 'a1', 'automatic', 1, 1); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source) VALUES ('t{n}', 'al{n}', 'r1', '/music/t{n}.flac', \
             't{n}.flac', 'h{n}', 1, 1, 's{n}', 'Song', 'song', 'Album {n}', 'album {n}', \
             'flac', 'indexed', 'scan', 1, 'automatic');"
        ));
        let (count, at) = if n == 4 {
            (5, now - 40 * day)
        } else if n <= albums {
            (5 - n, now - day)
        } else {
            (0, now)
        };
        for play in 0..count {
            seed.push_str(&format!(
                "INSERT INTO library_play_history (id, user_id, local_track_id, \
                 local_album_id, local_artist_id, track_name, artist_name, played_at) \
                 VALUES ('p{n}-{play}', 'user-1', 't{n}', 'al{n}', 'a1', 'Song', 'Aurora', \
                 {at});"
            ));
        }
    }
    db.write(Lane::Foreground, "home seed", move |tx| {
        tx.execute_batch(&seed).map_err(DbError::from)
    })
    .await
    .expect("seed");
    let script = Arc::new(Script {
        user_albums: vec![ListenedAlbum {
            release_group_mbid: Some(OTHER_RG.to_owned()),
            title: "Elsewhere".to_owned(),
            artist_name: "Everyone".to_owned(),
            artist_mbid: Some(OTHER_ARTIST.to_owned()),
            listen_count: 30,
        }],
        ..home_script()
    });
    let network: Arc<Script> = Arc::clone(&script);
    let listening = ListeningData::new(
        network,
        droppedneedle_providers::Providers::closed().cache,
        KeptFacts::new(db.clone(), Arc::new(SystemClock)),
    );
    let settings = crate::common::config_store(&dir);
    let integrations = || {
        droppedneedle_catalog::discover::content::IntegrationSettings::new(
            &settings,
            Arc::new(crate::common::bundles::NoDownloadSource),
        )
    };
    let clock = Arc::new(ManualClock::at(now));
    let warm = crate::common::bundles::cover_warm_at(&db, clock.clone());
    let home = LiveHome::new(HomeInputs {
        listening: listening.clone(),
        db: db.clone(),
        clock: clock.clone(),
        integrations: integrations(),
        art: Arc::new(GenreArt::new(HomeLibrary::new(db.clone()))),
        warm: warm.clone(),
        discover: DiscoverPages::new(PageInputs {
            listening,
            queue_db: QueueDb::new(db.clone()),
            db: db.clone(),
            clock,
            advanced: settings.live(),
            integrations: integrations(),
            warm,
            forgets: Default::default(),
        }),
        forgets: Default::default(),
    });
    home.page("user-1").await;
    let page = after_commits(&db, "the home build kept its shelves", || async {
        let page = home.page("user-1").await;
        shelf_of(&page.sections, SectionKey::TrendingArtists)
            .is_some()
            .then_some(page)
    })
    .await;
    let shelf = shelf_of(&page.sections, SectionKey::YourTopAlbums).expect("your top albums");
    let ids = shelf
        .items
        .iter()
        .filter_map(|item| match item {
            SectionItem::Album(album) => album.local_id.clone().or(album.musicbrainz_id.clone()),
            _ => None,
        })
        .collect();
    let source = serde_json::to_value(shelf.source).expect("source encodes");
    let reads = script.user_album_reads.load(Ordering::SeqCst);
    ((source.as_str().unwrap_or_default().to_owned(), ids), reads)
}

/// Your Top Albums comes from the person's own plays in the app over the
/// last 30 days, most played first, with no ListenBrainz read; with fewer
/// than three albums played, ListenBrainz's top albums stand in.
#[tokio::test]
async fn your_top_albums_come_from_plays_in_the_app() {
    let local = vec!["al1".to_owned(), "al2".to_owned(), "al3".to_owned()];
    assert_eq!(
        top_albums_after_plays(3).await,
        (("local".to_owned(), local), 0)
    );
    assert_eq!(
        top_albums_after_plays(2).await,
        (("listenbrainz".to_owned(), vec![OTHER_RG.to_owned()]), 1)
    );
}

/// Home's teaser is the first pick of the caller's set for this week they
/// have not answered, with its reason and its place in the set, and stays
/// on it while the next set is due but not built; once every pick is
/// answered it says so with the week's albums; with no set it is absent. Every read behind it is local: its whole answer
/// comes before the background build has read anything. Milestone
/// Anniversaries is a Home shelf of library albums with a round birthday
/// this year, oldest first.
#[tokio::test]
async fn home_teaser_is_the_next_pick_and_anniversaries_are_home() {
    use droppedneedle_catalog::discover::engine::weekly::{self, SetDay, day_on_or_before};

    let dir = crate::common::ScratchDir::new("home-teaser");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let db = runtime.db().clone();
    let now = 1_790_000_000_000_i64;
    let this_year = 2026;
    let week = day_on_or_before(now, SetDay::DEFAULT);
    let group = |n: u8| format!("0a1b2c3d-0000-4000-8000-0000000000b{n}");
    let mut seed = String::from(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bo', 'user', 0); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('a1', 'Aurora', 'aurora', 'group', 1, 1);",
    );
    for (n, year) in [(1, this_year - 10), (2, this_year - 30), (3, this_year - 7)] {
        seed += &format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at, year) \
             VALUES ('al{n}', 'r1', 'g{n}', 'Album {n}', 'album {n}', 'a1', 'automatic', 1, 1, \
             {year}); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source) VALUES ('t{n}', 'al{n}', 'r1', '/music/t{n}.flac', \
             't{n}.flac', 'h{n}', 1, 1, 's{n}', 'Song', 'song', 'Album {n}', 'album {n}', \
             'flac', 'indexed', 'scan', 1, 'automatic');"
        );
    }
    seed += &format!(
        "INSERT INTO catalog_weekly_sets (user_id, week_start_at, built_at, plays_counted, \
         seeds, shelf_artists, shelf_albums, linked, tastes) \
         VALUES ('user-1', {week}, {week}, 40, 3, 1, 3, 0, '[]');"
    );
    for n in 1..=3 {
        seed += &format!(
            "INSERT INTO catalog_weekly_set_picks (user_id, week_start_at, pick_no, \
             release_group_mbid_lower, release_group_mbid, title, artist_mbid, artist_name, \
             primary_type, first_release_date, reason_line, evidence) VALUES ('user-1', \
             {week}, {n}, '{g}', '{g}', 'Pick {n}', '{OTHER_ARTIST}', 'Someone', 'Album', \
             '2016-03-01', 'Close to Aurora, number {n}.', '{{}}');",
            g = group(n)
        );
    }
    seed += &format!(
        "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         release_group_mbid, created_at) VALUES ('user-1', 'album', '{g}', 'no', '{g}', {now});",
        g = group(1)
    );
    db.write(Lane::Foreground, "home seed", move |tx| {
        tx.execute_batch(&seed).map_err(DbError::from)
    })
    .await
    .expect("seed");
    let listening = ListeningData::new(
        Arc::new(home_script()),
        droppedneedle_providers::Providers::closed().cache,
        KeptFacts::new(db.clone(), Arc::new(SystemClock)),
    );
    let settings = crate::common::config_store(&dir);
    let integrations = || {
        droppedneedle_catalog::discover::content::IntegrationSettings::new(
            &settings,
            Arc::new(crate::common::bundles::NoDownloadSource),
        )
    };
    let clock = Arc::new(ManualClock::at(now));
    let warm = crate::common::bundles::cover_warm_at(&db, clock.clone());
    let home = LiveHome::new(HomeInputs {
        listening: listening.clone(),
        db: db.clone(),
        clock: clock.clone(),
        integrations: integrations(),
        art: Arc::new(GenreArt::new(HomeLibrary::new(db.clone()))),
        warm: warm.clone(),
        discover: DiscoverPages::new(PageInputs {
            listening,
            queue_db: QueueDb::new(db.clone()),
            db: db.clone(),
            clock,
            advanced: settings.live(),
            integrations: integrations(),
            warm,
            forgets: Default::default(),
        }),
        forgets: Default::default(),
    });

    // The first visit answers before any provider shelf is built.
    let first = home.page("user-1").await;
    assert!(first.refreshing);
    assert!(shelf_of(&first.sections, SectionKey::TrendingArtists).is_none());
    let teaser = first.discover_preview.expect("a set is out");
    let next = teaser.next.expect("an open pick");
    assert_eq!(
        (
            next.pick_no,
            next.album.title.as_str(),
            next.reason.as_str()
        ),
        (2, "Pick 2", "Close to Aurora, number 2."),
        "the first pick not answered, with its reason"
    );
    assert_eq!(next.album.year, Some(2016));
    assert_eq!(teaser.albums.len(), 3, "2 of 3 this week");
    assert_eq!(teaser.next_set_at, week + 7 * 86_400_000);
    let a_week_on = week + 8 * 86_400_000;
    let kept = weekly::teaser(&db, "user-1", a_week_on)
        .await
        .expect("the teaser reads")
        .and_then(|teaser| teaser.next);
    assert_eq!(
        kept.map(|pick| pick.pick_no),
        Some(2),
        "while the next set is due, last week's open pick stays"
    );

    let anniversaries: Vec<(String, Option<String>)> =
        shelf_of(&first.sections, SectionKey::Anniversaries)
            .expect("an anniversaries shelf")
            .items
            .iter()
            .filter_map(|item| match item {
                SectionItem::Album(album) => {
                    Some((album.title.clone(), album.release_date.clone()))
                }
                _ => None,
            })
            .collect();
    assert_eq!(
        anniversaries,
        [
            ("Album 2".to_owned(), Some((this_year - 30).to_string())),
            ("Album 1".to_owned(), Some((this_year - 10).to_string())),
        ],
        "round birthdays only, oldest first"
    );

    let answered = format!(
        "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         release_group_mbid, created_at) VALUES ('user-1', 'album', '{a}', 'know_it', '{a}', \
         {now}), ('user-1', 'album', '{b}', 'not_now', '{b}', {now});",
        a = group(2),
        b = group(3)
    );
    db.write(Lane::Foreground, "answers", move |tx| {
        tx.execute_batch(&answered).map_err(DbError::from)
    })
    .await
    .expect("answers");
    let all = home
        .page("user-1")
        .await
        .discover_preview
        .expect("the set is still out");
    assert_eq!(all.next, None, "every pick answered");
    assert_eq!(all.albums.len(), 3, "the week's albums, for the covers");
    assert_eq!(
        weekly::teaser(&db, "user-1", a_week_on)
            .await
            .expect("the teaser reads"),
        None,
        "a due week with every pick answered has nothing to show"
    );

    assert_eq!(
        home.page("user-2").await.discover_preview,
        None,
        "no set, no teaser"
    );
}

/// Home shows the weekly exploration playlist only while it is this
/// week's: the one dated 14 September is gone a week later.
#[tokio::test]
async fn home_shows_only_this_weeks_playlist() {
    assert!(!home_shows_weekly("2026-09-14T00:21:44.054723+00:00").await);
    assert!(home_shows_weekly("2026-09-21T00:00:00+00:00").await);
}
