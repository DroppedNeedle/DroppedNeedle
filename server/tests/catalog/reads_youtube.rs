//! YouTube links over a scratch database and the production YouTube search,
//! talking to a stand-in YouTube Data API: generate an album and its
//! tracks, reuse saved links without searching, edit and paste links by
//! hand, and delete them again. Journeys run the whole app: where a link's
//! recording id comes from, and how a play of it is filed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Body,
    extract::Query,
    http::{Request, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_catalog::collections::db::CollectionsDb;
use droppedneedle_catalog::metadata::Catalog;
use droppedneedle_catalog::metadata::library::LocalCatalog;
use droppedneedle_catalog::metadata::upstream::{LiveCatalogSettings, Upstream};
use droppedneedle_catalog::youtube::{
    self, YouTubeLinks,
    store::{AlbumRef, LinkStore, NewTrackLink},
};
use droppedneedle_kernel::clock::{Clock as _, SystemClock};
use droppedneedle_kernel::http_client::HttpClientFactory;
use droppedneedle_kernel::runtime_config::Secret;
use droppedneedle_kernel::runtime_config::secret_sections::YouTubeConnection;
use droppedneedle_providers::{ProviderEndpoints, Providers, ReqwestWire};
use droppedneedle_testkit::musicbrainz::MockMusicBrainz;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use crate::common::{FixedIdGenerator, Login, ScratchDir, TestApp, closed_outbound};
use crate::discover_queue::discover_deps;

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

const SEARCH_FOUND: &str = include_str!("../fixtures/youtube/search_found.json");

/// A stand-in YouTube Data API search: the recorded answer for every query,
/// except that a query naming "no-video-here" finds nothing. Records each
/// query it answers.
async fn youtube_stub() -> (String, Arc<Mutex<Vec<String>>>) {
    let queries: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = queries.clone();
    let stub = Router::new().route(
        "/youtube/v3/search",
        get(move |Query(query): Query<HashMap<String, String>>| {
            let seen = seen.clone();
            async move {
                let q = query.get("q").cloned().unwrap_or_default();
                seen.lock().unwrap().push(q.clone());
                if q.contains("no-video-here") {
                    Json(json!({"items": []})).into_response()
                } else {
                    ([(header::CONTENT_TYPE, "application/json")], SEARCH_FOUND).into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    (origin, queries)
}

#[tokio::test]
async fn youtube_links_journey() {
    let dir = ScratchDir::new("youtube");
    let rig = AuthRig::new("youtube").await;
    let (origin, queries) = youtube_stub().await;
    rig.config
        .save_secret_blocking(YouTubeConnection {
            api_key: Secret::new("youtube-key"),
            enabled: true,
            api_enabled: true,
            daily_quota_limit: 10_000,
        })
        .expect("youtube settings save");
    let http = HttpClientFactory::new().unwrap();
    let providers = Providers::unpaced_at(
        Arc::new(ReqwestWire::new(&http)),
        ProviderEndpoints {
            youtube: origin,
            ..ProviderEndpoints::closed()
        },
    );
    let catalog = Catalog::new(
        Upstream::new(
            Arc::new(Providers::unpaced_at(
                Arc::new(ReqwestWire::new(&http)),
                ProviderEndpoints::closed(),
            )),
            Arc::new(LiveCatalogSettings::new(&rig.config)),
            rig.deps.clone(),
        ),
        LocalCatalog::new(rig.platform.db.clone()),
    );
    let search = discover_deps(&rig, &dir, providers, |_| {}).youtube;
    let links = YouTubeLinks::new(
        LinkStore::new(CollectionsDb::new(
            rig.platform.db.clone(),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        )),
        search,
        catalog,
        Arc::new(FixedIdGenerator::new(
            "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
        )),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let app = with_test_principal(
        youtube::routes().with_state(links).into_router(),
        test_principal("u-ada", Role::User, "sess-test", SessionKind::Standard),
    );

    // Nothing saved yet: 204, not an error.
    let (status, _) = call(&app, "GET", "/api/v3/youtube/link/rg-1", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Album link: searched, saved, and answered with today's budget.
    let generate = json!({
        "artist_name": "Portishead", "album_name": "Dummy",
        "album_id": "rg-1", "cover_url": "https://covers/rg-1.jpg"
    });
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate",
        Some(generate.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let video = body["link"]["video_id"].as_str().unwrap().to_owned();
    assert_eq!(video, "hd9OYCqBK88");
    assert_eq!(
        body["link"]["embed_url"],
        format!("https://www.youtube.com/embed/{video}")
    );
    assert_eq!(body["link"]["is_manual"], false);
    assert_eq!(
        body["quota"],
        json!({"used": 1, "limit": 10000, "remaining": 9999, "date": body["quota"]["date"]})
    );
    // Asking again answers the saved row unchanged, without a search.
    let created = body["link"]["created_at"].clone();
    let (_, again) = call(&app, "POST", "/api/v3/youtube/generate", Some(generate)).await;
    assert_eq!(again["link"]["created_at"], created);
    assert_eq!(*queries.lock().unwrap(), ["Portishead Dummy full album"]);

    // No video found is a 404 with a readable message.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate",
        Some(json!({"artist_name": "X", "album_name": "no-video-here", "album_id": "rg-9"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no-video-here")
    );

    // Batch: one found, one with no video.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate-tracks",
        Some(json!({
            "album_id": "rg-1", "album_name": "Dummy", "artist_name": "Portishead",
            "tracks": [
                {"track_name": "Roads", "track_number": 5},
                {"track_name": "no-video-here", "track_number": 6, "disc_number": 1}
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["track_links"].as_array().unwrap().len(), 1);
    assert_eq!(body["track_links"][0]["disc_number"], 1);
    assert_eq!(
        body["failed"],
        json!([{"disc_number": 1, "track_number": 6, "track_name": "no-video-here", "reason": "No video found"}])
    );
    let roads = body["track_links"][0].clone();
    // A saved track answers without a new row.
    let (_, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate-track",
        Some(json!({
            "album_id": "rg-1", "album_name": "Dummy", "artist_name": "Portishead",
            "track_name": "Roads", "track_number": 5
        })),
    )
    .await;
    assert_eq!(body["track_link"], roads);
    let (_, list) = call(&app, "GET", "/api/v3/youtube/links", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["track_count"], 1);
    assert_eq!(list[0]["video_id"], video);

    // A track-only album gets an entry with no album video; removing its
    // last track removes the empty entry too.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/generate-track",
        Some(json!({
            "album_id": "rg-2", "album_name": "Third", "artist_name": "Portishead",
            "track_name": "Machine Gun", "track_number": 3, "disc_number": 2
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, entry) = call(&app, "GET", "/api/v3/youtube/link/rg-2", None).await;
    assert_eq!(entry["video_id"], Value::Null);
    assert_eq!(entry["track_count"], 1);

    // Edit: new video from a URL, cover cleared, creation time kept.
    let (status, body) = call(
        &app,
        "PUT",
        "/api/v3/youtube/link/rg-1",
        Some(json!({"youtube_url": "https://youtu.be/dQw4w9WgXcQ?si=x", "cover_url": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["video_id"], "dQw4w9WgXcQ");
    assert_eq!(body["cover_url"], Value::Null);
    assert_eq!(body["album_name"], "Dummy");
    assert_eq!(body["created_at"], created);
    let (status, body) = call(
        &app,
        "PUT",
        "/api/v3/youtube/link/rg-1",
        Some(json!({"youtube_url": "https://example.com/nope"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_INPUT");
    let (status, _) = call(&app, "PUT", "/api/v3/youtube/link/rg-x", Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Manual link with no album behind it gets a made-up id.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/youtube/manual",
        Some(json!({
            "album_name": "Live", "artist_name": "Portishead",
            "youtube_url": "https://www.youtube.com/watch?v=abcdefghijk"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["album_id"], "manual-0f1e2d3c4b5a");
    assert_eq!(body["is_manual"], true);

    // Deleting an album link takes its track links with it.
    let (status, _) = call(&app, "DELETE", "/api/v3/youtube/link/rg-1", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, tracks) = call(&app, "GET", "/api/v3/youtube/track-links/rg-1", None).await;
    assert_eq!(tracks, json!([]));
    let (_, list) = call(&app, "GET", "/api/v3/youtube/links", None).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
}

const GROUP: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_GROUP: &str = "22222222-2222-4222-8222-222222222222";
const RECORDING: &str = "98255a8c-017a-4bc7-8dd6-1fa36124572b";
const OTHER_RECORDING: &str = "0a8e8d55-4b83-4f8a-9732-fbb5ded9f344";
/// OK Computer and its first track's recording, in the mock MusicBrainz's
/// recorded answers.
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
const AIRBAG: &str = "8d3e9f0c-7b2a-4ef5-8a0b-2e3f5c9d1b87";

/// An album the library holds under `group`, with one track at disc 1,
/// number 1, of the title and tag recording given. The track's id is the
/// album's with `-t`.
async fn hold_album(app: &TestApp, album: &str, group: &str, title: &str, tag: Option<&str>) {
    let tag = tag.map_or("NULL".to_owned(), |id| format!("'{id}'"));
    let folded = title.to_lowercase();
    app.seed(format!(
        "INSERT OR IGNORE INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('a1', 'Portishead', 'portishead', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES ('{album}', 'r1', '{album}', 'Dummy', 'dummy', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_album_external_identities (local_album_id, release_group_mbid, \
         decision_source, selected_at) VALUES ('{album}', '{group}', 'manual', 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, disc_number, track_number, \
         title, title_folded, artist_name, artist_name_folded, album_title, album_title_folded, \
         duration_seconds, embedded_recording_mbid, availability, file_format, ingest_source, \
         imported_at, membership_source) VALUES ('{album}-t', '{album}', 'r1', '/m/{album}', \
         '{album}', '{album}', 1, 1, 's', 1, 1, '{title}', '{folded}', 'Portishead', \
         'portishead', 'Dummy', 'dummy', 240.0, {tag}, 'indexed', 'flac', 'scan', 1, \
         'automatic');"
    ))
    .await;
}

/// Mark `album` an unconfirmed best guess, as identification leaves one.
async fn flag_unconfirmed(app: &TestApp, album: &str) {
    app.seed_with(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, updated_at) \
         VALUES (?1, 'unconfirmed', 'CLOSE_CALL', 1)",
        [album],
    )
    .await;
}

/// A link already saved at disc 1, track 1 of `group`, as an older version
/// left it when `recording` is `None`.
async fn saved_link(app: &TestApp, group: &str, title: &str, recording: Option<&str>) {
    app.seed_with(
        "INSERT INTO catalog_youtube_track_links (album_id, track_number, disc_number, \
         album_name, track_name, video_id, artist_name, embed_url, created_at, recording_mbid) \
         VALUES (?1, 1, 1, 'Dummy', ?2, 'hd9OYCqBK88', 'Portishead', 'e', 1, ?3)",
        [Some(group), Some(title), recording],
    )
    .await;
}

/// The recording the first track link of `GROUP` answers.
async fn listed_recording(app: &TestApp, user: &Login) -> Value {
    recording_listed_for(app, user, GROUP).await
}

/// The recording the first track link of `album` answers.
async fn recording_listed_for(app: &TestApp, user: &Login, album: &str) -> Value {
    let (status, links) = app
        .call(
            "GET",
            &format!("/api/v3/youtube/track-links/{album}"),
            &[("authorization", user.authorization.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{links}");
    links[0]["recording_mbid"].clone()
}

fn stored_recording(app: &TestApp) -> Option<String> {
    app.db()
        .query_row(
            "SELECT recording_mbid FROM catalog_youtube_track_links",
            [],
            |row| row.get(0),
        )
        .expect("the link is saved")
}

/// The recording stored on the track link of `album`.
fn stored_recording_of(app: &TestApp, album: &str) -> Option<String> {
    app.db()
        .query_row(
            "SELECT recording_mbid FROM catalog_youtube_track_links WHERE album_id = ?1",
            [album],
            |row| row.get(0),
        )
        .expect("the link is saved")
}

/// Play Roads through `source`, sending what the web player sends.
async fn play(app: &TestApp, user: &Login, source: &str, mbid: &Value) {
    let (status, answer) = app
        .call(
            "POST",
            "/api/v3/scrobble/submit",
            &[("authorization", user.authorization.as_str())],
            Some(json!({
                "track_name": "Roads",
                "artist_name": "Portishead",
                "album_name": "Dummy",
                "timestamp": SystemClock.now_unix() - 60,
                "duration_ms": 240_000,
                "source": source,
                "mbid": mbid,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
}

/// The track the user's play is filed under, and the recording it keeps.
fn filed(app: &TestApp, user: &Login) -> (Option<String>, Option<String>) {
    app.db()
        .query_row(
            "SELECT local_track_id, recording_mbid FROM library_play_history WHERE user_id = ?1",
            [&user.user_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the play is kept")
}

/// The whole app with YouTube search on, talking to the stand-in, and a
/// plain user. MusicBrainz answers at `musicbrainz` when given, and is down
/// otherwise.
async fn searching_app(tag: &str, musicbrainz: Option<String>) -> (TestApp, Login) {
    let (origin, _) = youtube_stub().await;
    let closed = closed_outbound();
    let app = TestApp::start_with(
        tag,
        droppedneedle::bootstrap::Outbound {
            youtube: origin,
            brainzmash: musicbrainz.unwrap_or_else(|| closed.brainzmash.clone()),
            ..closed
        },
    )
    .await;
    let admin = app.login_admin().await;
    let (status, saved) = app
        .call(
            "PUT",
            "/api/v3/settings/youtube",
            &[("authorization", admin.as_str())],
            Some(json!({
                "api_key": "youtube-key", "enabled": true, "api_enabled": true,
                "daily_quota_limit": 10_000
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let user = app.login_user("listener").await;
    (app, user)
}

/// Ask for a video for disc 1, track 1 of `album`.
async fn generate(app: &TestApp, user: &Login, album: &str, title: &str) -> Value {
    let (status, made) = app
        .call(
            "POST",
            "/api/v3/youtube/generate-track",
            &[("authorization", user.authorization.as_str())],
            Some(json!({
                "album_id": album, "album_name": "Dummy", "artist_name": "Portishead",
                "track_name": title, "track_number": 1, "disc_number": 1
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    made
}

/// One whole run of the job that files older plays.
async fn run_filing_job(app: &TestApp) {
    use droppedneedle_library::repairs::plays;
    app.seed_tx(|tx| {
        plays::queue(tx, "filing", None, 1.0).expect("the job queues");
        while !plays::step(tx, "filing", plays::BATCH, 1.0).expect("the batch runs") {}
    })
    .await;
}

/// A link made from a release track carries that track's recording, read by
/// the server from MusicBrainz, and a play of it is filed under the library's
/// track.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_play_of_a_generated_link_is_filed_under_its_recording() {
    let musicbrainz = MockMusicBrainz::start().await.expect("the mock serves");
    let (app, user) = searching_app("youtube-filed", Some(musicbrainz.base_url())).await;
    hold_album(&app, "al1", OK_COMPUTER, "Airbag", Some(AIRBAG)).await;

    let made = generate(&app, &user, OK_COMPUTER, "Airbag").await;
    assert_eq!(made["track_link"]["recording_mbid"], AIRBAG);
    assert_eq!(stored_recording(&app).as_deref(), Some(AIRBAG));

    let listed = recording_listed_for(&app, &user, OK_COMPUTER).await;
    assert_eq!(listed, AIRBAG);
    play(&app, &user, "youtube", &listed).await;
    assert_eq!(
        filed(&app, &user),
        (Some("al1-t".to_owned()), Some(AIRBAG.to_owned()))
    );
    app.stop().await;
}

/// While MusicBrainz is down a new link stores no recording it cannot
/// prove. From the library's copy it reads the id only under the rule for
/// what the library holds, so a guess gives none; and a tag that is no
/// MusicBrainz id reads as none. Each case has its own album and group.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_stores_only_a_recording_it_can_prove() {
    let (app, user) = searching_app("youtube-mb-down", None).await;

    hold_album(&app, "al1", GROUP, "Roads", Some(RECORDING)).await;
    let made = generate(&app, &user, GROUP, "Roads").await;
    assert_eq!(
        made["track_link"]["recording_mbid"], RECORDING,
        "musicbrainz down"
    );
    assert_eq!(stored_recording_of(&app, GROUP), None, "musicbrainz down");
    flag_unconfirmed(&app, "al1").await;
    assert_eq!(
        recording_listed_for(&app, &user, GROUP).await,
        Value::Null,
        "a guess"
    );

    hold_album(&app, "al2", OTHER_GROUP, "Roads", Some("not-a-recording")).await;
    generate(&app, &user, OTHER_GROUP, "Roads").await;
    assert_eq!(
        stored_recording_of(&app, OTHER_GROUP),
        None,
        "a malformed tag"
    );
    assert_eq!(
        recording_listed_for(&app, &user, OTHER_GROUP).await,
        Value::Null,
        "a malformed tag"
    );
    app.stop().await;
}

/// A link from before the column takes a recording only from the one
/// accepted track it fits, when read, and writes nothing; its play is filed.
/// A guessed album, the same slot under another title, two editions that
/// disagree and a slot with no recording give it nothing; one recording on
/// two tracks leaves its play by name. Each case has its own release group
/// and user, so their plays are told apart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_link_takes_a_recording_only_from_one_accepted_track() {
    let app = TestApp::start("youtube-older").await;
    let group = |n: u8| format!("11111111-1111-4111-8111-1111111111{n:02}");

    // It takes the id of the one track it fits, and its play is filed. First,
    // while it is the only track holding the recording.
    let user = app.login_user("listener-1").await;
    let fits = group(1);
    hold_album(&app, "al1", &fits, "Roads", Some(RECORDING)).await;
    saved_link(&app, &fits, " ROADS ", None).await;
    let listed = recording_listed_for(&app, &user, &fits).await;
    assert_eq!(listed, RECORDING, "one track fits");
    assert_eq!(
        stored_recording_of(&app, &fits),
        None,
        "reading writes nothing"
    );
    play(&app, &user, "youtube", &listed).await;
    assert_eq!(
        filed(&app, &user).0.as_deref(),
        Some("al1-t"),
        "one track fits"
    );

    // An unconfirmed best guess is not an accepted release group, whatever
    // its files' tags say.
    let guessed = group(2);
    hold_album(&app, "al2", &guessed, "Roads", Some(RECORDING)).await;
    flag_unconfirmed(&app, "al2").await;
    saved_link(&app, &guessed, "Roads", None).await;
    assert_eq!(
        recording_listed_for(&app, &user, &guessed).await,
        Value::Null,
        "a guessed album"
    );

    // The same disc and number under another title is another track.
    let retitled = group(3);
    hold_album(&app, "al3", &retitled, "Roads", Some(RECORDING)).await;
    saved_link(&app, &retitled, "Sour Times", None).await;
    assert_eq!(
        recording_listed_for(&app, &user, &retitled).await,
        Value::Null,
        "another title"
    );

    // Two editions with different recordings at the slot: which one the
    // link stands for cannot be told.
    let editions = group(4);
    hold_album(&app, "al4", &editions, "Roads", Some(RECORDING)).await;
    hold_album(&app, "al5", &editions, "Roads", Some(OTHER_RECORDING)).await;
    saved_link(&app, &editions, "Roads", None).await;
    assert_eq!(
        recording_listed_for(&app, &user, &editions).await,
        Value::Null,
        "editions that disagree"
    );

    // One recording held by two tracks has no one track to file under: the
    // play stays by name, with its id.
    let user = app.login_user("listener-5").await;
    let (shared, elsewhere) = (group(5), group(6));
    hold_album(&app, "al6", &shared, "Roads", Some(RECORDING)).await;
    hold_album(&app, "al7", &elsewhere, "Roads", Some(RECORDING)).await;
    saved_link(&app, &shared, "Roads", None).await;
    let listed = recording_listed_for(&app, &user, &shared).await;
    assert_eq!(listed, RECORDING, "a recording on two tracks");
    play(&app, &user, "youtube", &listed).await;
    assert_eq!(
        filed(&app, &user),
        (None, Some(RECORDING.to_owned())),
        "a recording on two tracks"
    );

    // A play with no recording stays by name, and the filing job leaves it,
    // though its names fit a track.
    let user = app.login_user("listener-6").await;
    let untagged = group(7);
    hold_album(&app, "al8", &untagged, "Roads", None).await;
    saved_link(&app, &untagged, "Roads", None).await;
    let listed = recording_listed_for(&app, &user, &untagged).await;
    assert_eq!(listed, Value::Null, "no recording");
    play(&app, &user, "youtube", &listed).await;
    run_filing_job(&app).await;
    assert_eq!(filed(&app, &user), (None, None), "no recording");
    app.stop().await;
}

/// A recording the library gets later: the filing job files the play kept
/// with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_held_later_is_filed_by_the_job() {
    let app = TestApp::start("youtube-later").await;
    let user = app.login_user("listener").await;
    saved_link(&app, GROUP, "Roads", Some(RECORDING)).await;

    let listed = listed_recording(&app, &user).await;
    assert_eq!(listed, RECORDING);
    play(&app, &user, "youtube", &listed).await;
    assert_eq!(filed(&app, &user).0, None);

    hold_album(&app, "al1", GROUP, "Roads", Some(RECORDING)).await;
    run_filing_job(&app).await;
    assert_eq!(filed(&app, &user).0.as_deref(), Some("al1-t"));
    app.stop().await;
}

/// A link saved again over an old one keeps no old recording.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacing_a_link_clears_its_recording() {
    let app = TestApp::start("youtube-replace").await;
    let store = LinkStore::new(CollectionsDb::new(
        app.state().library.db.clone(),
        Arc::new(SystemClock),
    ));
    let album = AlbumRef {
        album_id: GROUP.to_owned(),
        album_name: "Dummy".to_owned(),
        artist_name: "Portishead".to_owned(),
        cover_url: None,
    };
    let track = |video: &str, recording: Option<&str>| NewTrackLink {
        disc_number: 1,
        track_number: 1,
        track_name: "Roads".to_owned(),
        video_id: video.to_owned(),
        recording_mbid: recording.map(str::to_owned),
    };
    store
        .save_track_links(album.clone(), vec![track("old-video-1", Some(RECORDING))])
        .await
        .expect("the link saves");
    assert_eq!(stored_recording(&app).as_deref(), Some(RECORDING));
    store
        .save_track_links(album, vec![track("new-video-1", None)])
        .await
        .expect("the link saves again");
    assert_eq!(stored_recording(&app), None);
    app.stop().await;
}

/// An imported entry keeps the id its server gave, cleaned, and a play of it
/// is filed. Moved to another source it no longer speaks for that source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_imported_entry_carries_its_recording_to_a_filed_play() {
    use droppedneedle_catalog::collections::remote_import::PlaylistImportSink;
    use droppedneedle_media::remotes::models::{SourceName, TrackView};
    use droppedneedle_media::remotes::source::ImportSink as _;

    let app = TestApp::start("youtube-imported").await;
    let user = app.login_user("listener").await;
    hold_album(&app, "al1", GROUP, "Roads", Some(RECORDING)).await;
    let entry = |id: &str, recording: &str| TrackView {
        source: SourceName::Jellyfin,
        id: id.to_owned(),
        title: "Roads".to_owned(),
        album_name: "Dummy".to_owned(),
        album_id: None,
        artist_name: "Portishead".to_owned(),
        artist_id: None,
        track_number: Some(1),
        disc_number: Some(1),
        duration_secs: Some(240),
        year: None,
        recording_mbid: Some(recording.to_owned()),
        image_url: None,
        part_key: None,
    };
    let imported = PlaylistImportSink::new(app.state().catalog.collections.clone())
        .import(
            &user.user_id,
            SourceName::Jellyfin,
            "pl1",
            "Mix",
            vec![
                entry("jf-1", &RECORDING.to_uppercase()),
                entry("jf-2", "not-a-recording"),
            ],
        )
        .await
        .expect("the playlist imports");

    let headers = [("authorization", user.authorization.as_str())];
    let playlist = format!("/api/v3/playlists/{}", imported.local_playlist_id);
    let (status, detail) = app.call("GET", &playlist, &headers, None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["tracks"][0]["recording_mbid"], RECORDING);
    assert_eq!(detail["tracks"][1]["recording_mbid"], Value::Null);

    play(
        &app,
        &user,
        "jellyfin",
        &detail["tracks"][0]["recording_mbid"],
    )
    .await;
    assert_eq!(filed(&app, &user).0.as_deref(), Some("al1-t"));

    let entry_id = detail["tracks"][0]["id"].as_str().expect("entry id");
    let (status, moved) = app
        .call(
            "PATCH",
            &format!("{playlist}/tracks/{entry_id}"),
            &headers,
            Some(json!({"source_type": "plex"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["recording_mbid"], Value::Null);
    app.stop().await;
}
