//! Subsonic wire contract through the whole app, over a library its own
//! scanner filled: envelopes, error codes, response shapes per endpoint,
//! cover art, binary endpoints, the transcode decision, transcoded streams
//! and mutations. The library is the one `reference/compat/library.json`
//! lays out. Only the transcoding app is given an ffmpeg: the committed
//! stand-in.

use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

use crate::common::compat::{CompatApp, PLAYER, named};

/// One answer as it came.
struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or("")
    }
}

/// `/subsonic/rest/{endpoint}` with `pairs` form-encoded as its query.
fn uri(endpoint: &str, pairs: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://test.local/").expect("base parses");
    url.query_pairs_mut().extend_pairs(pairs);
    format!("/subsonic/rest/{endpoint}?{}", url.query().unwrap_or(""))
}

/// One request with `pairs` as its query and `body` under `content_type`,
/// asked again while the app answers busy.
async fn request(
    rig: &CompatApp,
    method: &str,
    endpoint: &str,
    pairs: &[(&str, &str)],
    content_type: Option<&str>,
    body: &[u8],
) -> Answer {
    let headers: Vec<(&str, &str)> = content_type
        .map(|kind| ("content-type", kind))
        .into_iter()
        .collect();
    let target = uri(endpoint, pairs);
    let (status, headers, body) =
        crate::common::through_busy(|| rig.app.raw_bytes(method, &target, &headers, body.to_vec()))
            .await;
    Answer {
        status,
        headers,
        body,
    }
}

/// A request carrying only `pairs`, no credentials of its own.
async fn unsigned(rig: &CompatApp, method: &str, endpoint: &str, pairs: &[(&str, &str)]) -> Answer {
    request(rig, method, endpoint, pairs, None, &[]).await
}

/// A Subsonic app signed in as one account, as the client `golden`.
struct Client<'a> {
    rig: &'a CompatApp,
    user: String,
    secret: String,
}

impl<'a> Client<'a> {
    fn player(rig: &'a CompatApp) -> Self {
        Self {
            rig,
            user: PLAYER.to_owned(),
            secret: rig.secret.clone(),
        }
    }

    async fn admin(rig: &'a CompatApp) -> Self {
        Self {
            rig,
            user: "admin".to_owned(),
            secret: rig.admin_secret().await,
        }
    }

    /// The credentials, then `pairs`.
    fn signed<'s>(&'s self, pairs: &[(&'s str, &'s str)]) -> Vec<(&'s str, &'s str)> {
        let mut all = vec![
            ("u", self.user.as_str()),
            ("p", self.secret.as_str()),
            ("v", "1.16.1"),
            ("c", "golden"),
        ];
        all.extend_from_slice(pairs);
        all
    }

    async fn send(&self, method: &str, endpoint: &str, pairs: &[(&str, &str)]) -> Answer {
        unsigned(self.rig, method, endpoint, &self.signed(pairs)).await
    }

    async fn get(&self, endpoint: &str, pairs: &[(&str, &str)]) -> Answer {
        self.send("GET", endpoint, pairs).await
    }

    /// A POST with `body` under `content_type`; `pairs` go in the query.
    async fn post(
        &self,
        endpoint: &str,
        pairs: &[(&str, &str)],
        content_type: &str,
        body: &[u8],
    ) -> Answer {
        let signed = self.signed(pairs);
        request(
            self.rig,
            "POST",
            endpoint,
            &signed,
            Some(content_type),
            body,
        )
        .await
    }

    /// The JSON answer to `endpoint`, which must succeed.
    async fn ok(&self, endpoint: &str, pairs: &[(&str, &str)]) -> Value {
        let mut all = vec![("f", "json")];
        all.extend_from_slice(pairs);
        let answer = self.get(endpoint, &all).await;
        let body: Value = serde_json::from_slice(&answer.body).expect("json answer");
        assert_eq!(
            body["subsonic-response"]["status"], "ok",
            "{endpoint}: {body}"
        );
        body["subsonic-response"].clone()
    }
}

/// Subsonic ids of the scanned library, by name.
struct Ids {
    songs: Value,
    albums: Value,
    artists: Value,
}

impl Ids {
    async fn read(client: &Client<'_>) -> Self {
        let all = client
            .ok(
                "search3",
                &[("query", ""), ("songCount", "500"), ("albumCount", "500")],
            )
            .await;
        let artists = client.ok("getArtists", &[]).await;
        let artists = artists["artists"]["index"]
            .as_array()
            .expect("index")
            .iter()
            .flat_map(|index| index["artist"].as_array().cloned().unwrap_or_default())
            .collect();
        Self {
            songs: all["searchResult3"]["song"].clone(),
            albums: all["searchResult3"]["album"].clone(),
            artists: Value::Array(artists),
        }
    }

    fn song(&self, title: &str) -> String {
        id_of(named(&self.songs, "title", title))
    }

    fn album(&self, name: &str) -> String {
        id_of(named(&self.albums, "name", name))
    }

    fn artist(&self, name: &str) -> String {
        id_of(named(&self.artists, "name", name))
    }
}

fn id_of(item: &Value) -> String {
    item["id"].as_str().expect("id").to_owned()
}

fn assert_ok(answer: &Answer, case: &str) {
    let text = answer.text();
    assert_eq!(answer.status, StatusCode::OK, "{case}");
    assert!(
        text.contains("status=\"ok\"") || text.contains("\"status\":\"ok\""),
        "{case}: {text}"
    );
}

fn assert_code(answer: &Answer, code: u32, case: &str) {
    let text = answer.text();
    assert_eq!(answer.status, StatusCode::OK, "{case}");
    assert_eq!(answer.content_type(), "application/xml", "{case}");
    assert!(text.contains("status=\"failed\""), "{case}: {text}");
    assert!(text.contains(&format!("code=\"{code}\"")), "{case}: {text}");
}

/// Turn the Subsonic API on or off in Settings, as the admin does.
async fn set_subsonic(rig: &CompatApp, enabled: bool) {
    let admin = rig.app.login_admin().await;
    let admin = [("authorization", admin.as_str())];
    let (status, mut apps) = rig
        .app
        .call("GET", "/api/v3/settings/connect-apps", &admin, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{apps}");
    apps["subsonic_enabled"] = json!(enabled);
    let (status, saved) = rig
        .app
        .call("PUT", "/api/v3/settings/connect-apps", &admin, Some(apps))
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

const XML_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"http://subsonic.org/restapi\"";
/// `serverVersion` is DroppedNeedle's own version.
const ATTRS: &str = concat!(
    "version=\"1.16.1\" type=\"DroppedNeedle\" serverVersion=\"",
    env!("CARGO_PKG_VERSION"),
    "\" openSubsonic=\"true\""
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn envelopes_are_byte_exact() {
    let rig = CompatApp::start("compat-sub-envelopes").await;
    let client = Client::player(&rig);
    let ok = format!("{XML_HEAD} status=\"ok\" {ATTRS}");
    let cases = [
        (
            client.get("ping", &[]).await,
            "application/xml",
            format!("{ok}/>"),
        ),
        (
            client.get("ping", &[("f", "json")]).await,
            "application/json",
            concat!(
                "{\"subsonic-response\":{\"status\":\"ok\",\"version\":\"1.16.1\",\"type\":\"DroppedNeedle\",\"serverVersion\":\"",
                env!("CARGO_PKG_VERSION"),
                "\",\"openSubsonic\":true}}"
            )
            .to_owned(),
        ),
        // Endpoint names casefold and drop `.view`.
        (
            client.get("GETLICENSE", &[]).await,
            "application/xml",
            format!("{ok}><license valid=\"true\"/></subsonic-response>"),
        ),
        // Extensions are public and list every served extension.
        (
            unsigned(&rig, "GET", "getOpenSubsonicExtensions", &[]).await,
            "application/xml",
            format!("{ok}><openSubsonicExtensions name=\"apiKeyAuthentication\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"formPost\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"transcodeOffset\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"songLyrics\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"playbackReport\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"indexBasedQueue\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"transcoding\"><versions>1</versions></openSubsonicExtensions></subsonic-response>"),
        ),
        (
            unsigned(&rig, "GET", "ping", &[]).await,
            "application/xml",
            format!(
                "{XML_HEAD} status=\"failed\" {ATTRS}><error code=\"10\" message=\"Required parameter is missing.\"/></subsonic-response>"
            ),
        ),
    ];
    for (answer, content_type, expected) in cases {
        assert_eq!(answer.status, StatusCode::OK, "{expected}");
        assert_eq!(answer.content_type(), content_type, "{expected}");
        assert_eq!(answer.text(), expected);
    }
    assert_ok(&client.get("Ping.VIEW", &[]).await, "ping.view");

    // Credentials that are neither one password, one token pair nor one
    // API key. They are here and not with the other sign-in codes because
    // five refusals from one address lock it out.
    let malformed: [&[(&str, &str)]; 3] = [
        &[("u", PLAYER), ("t", "abc")],
        &[("u", PLAYER), ("p", "secret"), ("t", "a"), ("s", "b")],
        &[("u", PLAYER), ("u", PLAYER), ("p", "secret")],
    ];
    for pairs in malformed {
        let answer = unsigned(&rig, "GET", "ping", pairs).await;
        assert_code(&answer, 10, &format!("{pairs:?}"));
    }

    // An unknown method is a plain 404 in every format, for GET and POST.
    for format in ["json", "xml", "jsonp"] {
        for method in ["GET", "POST"] {
            let answer = client.send(method, "nope", &[("f", format)]).await;
            assert_eq!(
                (answer.status, answer.content_type()),
                (StatusCode::NOT_FOUND, "text/plain"),
                "{method} {format}"
            );
            assert_eq!(answer.text(), "404 page not found\n");
        }
    }

    // JSONP wraps only a safe callback name, otherwise falls back to JSON.
    let answer = client
        .get("ping", &[("f", "jsonp"), ("callback", "app.cb$1")])
        .await;
    assert_eq!(answer.content_type(), "application/javascript");
    let text = answer.text();
    assert!(
        text.starts_with("app.cb$1({\"subsonic-response\":") && text.ends_with("});"),
        "{text}"
    );
    let answer = client
        .get("ping", &[("f", "jsonp"), ("callback", "alert(1)")])
        .await;
    assert_eq!(answer.content_type(), "application/json");
    assert!(answer.text().starts_with("{\"subsonic-response\":"));
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_codes() {
    /// Endpoint, query pairs and the expected Subsonic error code.
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, u32);
    let rig = CompatApp::start("compat-sub-errors").await;
    let client = Client::player(&rig);
    let ids = Ids::read(&client).await;
    let (one, airbag) = (ids.song("One"), ids.song("Airbag"));
    let (one, airbag) = (one.as_str(), airbag.as_str());

    let authed: Vec<Case> = vec![
        ("ping", vec![("f", "yaml")], 10),
        ("getArtists", vec![("musicFolderId", "2")], 70),
        ("getArtist", vec![("id", one)], 70),
        ("getArtist", vec![("id", "xx-1")], 70),
        ("getSong", vec![("id", "tr-nope")], 70),
        ("getAlbumList2", vec![], 10),
        (
            "getAlbumList2",
            vec![("type", "byYear"), ("fromYear", "2020")],
            10,
        ),
        (
            "getAlbumList2",
            vec![("type", "newest"), ("fromYear", "2020")],
            10,
        ),
        ("getAlbumList2", vec![("type", "byGenre")], 10),
        ("getMusicDirectory", vec![("id", one)], 70),
        ("getPlaylist", vec![("id", "pl-nope")], 70),
        ("createPlaylist", vec![], 10),
        ("star", vec![], 10),
        ("star", vec![("id", "tr-nope")], 70),
        ("setRating", vec![("id", one), ("rating", "6")], 10),
        ("setRating", vec![("id", "tr-nope"), ("rating", "3")], 70),
        ("setRating", vec![("id", one)], 10),
        ("scrobble", vec![], 10),
        (
            "scrobble",
            vec![("id", one), ("id", airbag), ("time", "1")],
            0,
        ),
        (
            "reportPlayback",
            vec![("mediaId", one), ("mediaType", "song"), ("positionMs", "1")],
            10,
        ),
        (
            "createBookmark",
            vec![("id", "tr-nope"), ("position", "10")],
            70,
        ),
        ("getAlbumInfo2", vec![("id", "al-nope")], 70),
        ("getLyrics", vec![], 10),
        ("getSongsByGenre", vec![], 10),
        ("getUser", vec![], 10),
        ("startScan", vec![], 50),
        ("getTopSongs", vec![("artist", "Nobody")], 70),
        ("getSimilarSongs", vec![("id", "ar-nope")], 70),
        ("getSimilarSongs", vec![("id", one)], 70),
        (
            "getTranscodeStream",
            vec![("mediaId", one), ("mediaType", "song")],
            10,
        ),
        (
            "getTranscodeDecision",
            vec![("mediaId", airbag), ("mediaType", "song")],
            10,
        ),
        ("savePlayQueue", vec![("id", one), ("id", airbag)], 10),
        ("savePlayQueue", vec![("current", one)], 10),
        (
            "savePlayQueueByIndex",
            vec![("id", one), ("currentIndex", "4")],
            10,
        ),
    ];
    for (endpoint, pairs, code) in authed {
        assert_code(
            &client.get(endpoint, &pairs).await,
            code,
            &format!("{endpoint} {pairs:?}"),
        );
    }

    // The app password is also an API key, on its own.
    let key = [("apiKey", rig.secret.as_str()), ("c", "golden")];
    assert_ok(&unsigned(&rig, "GET", "ping", &key).await, "apiKey alone");
    let refused: [Case; 3] = [
        (
            "ping",
            vec![("u", PLAYER), ("p", "wrong"), ("c", "golden")],
            40,
        ),
        ("ping", vec![("apiKey", "nope"), ("c", "golden")], 44),
        (
            "ping",
            vec![("apiKey", rig.secret.as_str()), ("u", PLAYER)],
            43,
        ),
    ];
    for (endpoint, pairs, code) in refused {
        let answer = unsigned(&rig, "GET", endpoint, &pairs).await;
        assert_code(&answer, code, &format!("{endpoint} {pairs:?}"));
    }

    // A disabled API answers code 0 before any lookup or auth.
    set_subsonic(&rig, false).await;
    for endpoint in ["ping", "nope"] {
        let answer = client.get(endpoint, &[]).await;
        assert_code(&answer, 0, endpoint);
        assert!(answer.text().contains("disabled"));
    }
    rig.stop().await;
}

/// LRCLIB as the lyrics lookup sees it: synced and plain lyrics for "One"
/// by U2, nothing for anything else.
async fn lyrics_server() -> (String, tokio::task::JoinHandle<()>) {
    use axum::extract::Query;
    use axum::response::IntoResponse;
    use std::collections::HashMap;

    let get = |Query(query): Query<HashMap<String, String>>| async move {
        if query.get("track_name").map(String::as_str) != Some("One") {
            return StatusCode::NOT_FOUND.into_response();
        }
        axum::Json(json!({
            "id": 1,
            "trackName": "One",
            "artistName": "U2",
            "albumName": "Achtung Baby",
            "duration": 200,
            "instrumental": false,
            "plainLyrics": "First line\nSecond line",
            "syncedLyrics": "[00:01.00]First line\n[00:02.00]Second line",
        }))
        .into_response()
    };
    let router = axum::Router::new().route("/lrclib/api/get", axum::routing::get(get));
    droppedneedle_testkit::usenet::serve_loopback(router)
        .await
        .expect("lyrics server starts")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_shapes() {
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, Vec<&'a str>, Vec<&'a str>);
    let (lrclib, server) = lyrics_server().await;
    let rig = CompatApp::start_with("compat-sub-shapes", crate::common::outbound_at(&lrclib)).await;
    // The lyrics lookup skips a track shorter than a second, as every
    // fixture track is.
    rig.app
        .db()
        .execute(
            "UPDATE local_tracks SET duration_seconds = 200 \
             WHERE relative_path = 'U2/Achtung Baby/03 One.mp3'",
            [],
        )
        .expect("duration seeds");
    let client = Client::player(&rig);
    let ids = Ids::read(&client).await;
    let (one, airbag) = (ids.song("One"), ids.song("Airbag"));
    let ok_computer = ids.album("OK Computer");
    let (u2, radiohead) = (ids.artist("U2"), ids.artist("Radiohead"));

    // Articles are ignored when bucketing.
    use droppedneedle_compat::subsonic::browse::index_letter;
    assert_eq!(index_letter("The Beatles"), 'B');
    assert_eq!(index_letter("Los Lobos"), 'L');
    assert_eq!(index_letter("ユキ"), 'ユ');

    // lastModified is a change counter: only its echo means unchanged.
    let indexes = client.ok("getIndexes", &[]).await;
    let revision = indexes["indexes"]["lastModified"].to_string();
    let earlier = (revision.parse::<i64>().expect("revision") - 1).to_string();
    let unchanged = client
        .get("getIndexes", &[("ifModifiedSince", &revision)])
        .await
        .text();
    assert!(
        unchanged.contains(&format!("lastModified=\"{revision}\""))
            && !unchanged.contains("<index "),
        "{unchanged}"
    );
    let changed = client
        .get("getIndexes", &[("ifModifiedSince", &earlier)])
        .await
        .text();
    assert!(changed.contains("<index "), "{changed}");

    // What the calls below read: a playlist with two songs and an entry
    // with no library file, a star, a song playing, a queue and a bookmark.
    let made = client
        .ok(
            "createPlaylist",
            &[("name", "Mix"), ("songId", &one), ("songId", &airbag)],
        )
        .await;
    let playlist = id_of(&made["playlist"]);
    let bearer = player_session(&rig).await;
    let (status, added) = rig
        .app
        .call(
            "POST",
            &format!(
                "/api/v3/playlists/{}/tracks",
                playlist.trim_start_matches("pl-")
            ),
            &[("authorization", bearer.as_str())],
            Some(json!({"tracks": [{
                "track_name": "Elsewhere",
                "artist_name": "Nobody",
                "album_name": "Nowhere",
                "source_type": "youtube",
            }]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    client.ok("star", &[("id", &one)]).await;
    client
        .ok("scrobble", &[("id", &one), ("submission", "false")])
        .await;
    client
        .ok(
            "savePlayQueue",
            &[("id", &one), ("current", &one), ("position", "1000")],
        )
        .await;
    client
        .ok(
            "createBookmark",
            &[("id", &one), ("position", "5000"), ("comment", "chorus")],
        )
        .await;

    let parent = format!("parent=\"{radiohead}\"");
    let cover = format!("coverArt=\"{playlist}\"");
    let cases: Vec<Case> = vec![
        (
            "getMusicFolders",
            vec![],
            vec!["musicFolder id=\"1\""],
            vec![],
        ),
        (
            "getArtists",
            vec![("musicFolderId", "1"), ("musicFolderId", "1")],
            vec![],
            vec![],
        ),
        // Album artists only: the compilation's track artist is no entry.
        (
            "getArtists",
            vec![],
            vec![
                "ignoredArticles=\"The El La Los Las Le Les\"",
                "<index name=\"M\">",
                "<index name=\"R\">",
                "<index name=\"U\">",
                "<index name=\"V\">",
                "<index name=\"ユ\">",
            ],
            vec!["<index name=\"A\">"],
        ),
        ("getArtist", vec![("id", &u2)], vec!["Achtung Baby"], vec![]),
        (
            "getAlbum",
            vec![("id", &ok_computer)],
            vec![
                "Airbag",
                "Paranoid Android",
                "mediaType=\"song\"",
                "channelCount=\"2\"",
            ],
            vec![],
        ),
        (
            "getSong",
            vec![("id", &airbag)],
            vec![
                "contentType=\"audio/flac\"",
                "suffix=\"flac\"",
                "<genres name=\"Alternative Rock\"/>",
            ],
            vec!["transcodedContentType"],
        ),
        (
            "getAlbumList2",
            vec![("type", "byYear"), ("fromYear", "0"), ("toYear", "2000")],
            vec!["OK Computer", "Achtung Baby", "Mezzanine"],
            vec!["Now Thats Music"],
        ),
        (
            "getAlbumList2",
            vec![("type", "byGenre"), ("genre", "Rock")],
            vec!["Achtung Baby"],
            vec!["OK Computer"],
        ),
        (
            "getAlbumList",
            vec![("type", "newest")],
            vec!["<albumList", "isDir=\"true\""],
            vec![],
        ),
        (
            "getRandomSongs",
            vec![("size", "5")],
            vec!["<randomSongs"],
            vec![],
        ),
        (
            "getMusicDirectory",
            vec![("id", "1")],
            vec!["Radiohead", "isDir=\"true\""],
            vec![],
        ),
        (
            "getMusicDirectory",
            vec![("id", &u2)],
            vec!["Achtung Baby"],
            vec![],
        ),
        (
            "getMusicDirectory",
            vec![("id", &ok_computer)],
            vec!["Airbag", &parent],
            vec![],
        ),
        // Missing, empty and quoted-empty queries match everything.
        (
            "search3",
            vec![("songCount", "10")],
            vec!["title=\"One\"", "title=\"Airbag\""],
            vec![],
        ),
        (
            "search3",
            vec![("query", ""), ("songCount", "10")],
            vec!["title=\"One\""],
            vec![],
        ),
        (
            "search3",
            vec![("query", "\"\""), ("songCount", "10")],
            vec!["title=\"One\""],
            vec![],
        ),
        (
            "search3",
            vec![("query", "''"), ("songCount", "10")],
            vec!["title=\"One\""],
            vec![],
        ),
        (
            "search3",
            vec![("query", "  "), ("songCount", "10")],
            vec!["title=\"One\""],
            vec![],
        ),
        (
            "search3",
            vec![("query", "\"Airbag\""), ("songCount", "10")],
            vec!["title=\"Airbag\""],
            vec!["title=\"One\""],
        ),
        (
            "search2",
            vec![("query", "Radiohead")],
            vec!["<searchResult2", "name=\"Radiohead\""],
            vec![],
        ),
        // Playlist counts and entries cover streamable tracks only.
        (
            "getPlaylists",
            vec![],
            vec!["songCount=\"2\"", &cover],
            vec![],
        ),
        (
            "getPlaylist",
            vec![("id", &playlist)],
            vec![
                "songCount=\"2\"",
                "title=\"One\"",
                "title=\"Airbag\"",
                "owner=\"alice\"",
            ],
            vec!["Elsewhere"],
        ),
        (
            "getStarred",
            vec![],
            vec!["<starred", "title=\"One\""],
            vec![],
        ),
        (
            "getNowPlaying",
            vec![],
            vec![
                "username=\"alice\"",
                "minutesAgo=\"0\"",
                "playerId=\"0\"",
                "playerName=\"golden\"",
                "title=\"One\"",
            ],
            vec![],
        ),
        (
            "getPlayQueue",
            vec![],
            vec!["position=\"1000\"", "changedBy=\"golden\"", "title=\"One\""],
            vec![],
        ),
        (
            "getPlayQueueByIndex",
            vec![],
            vec!["currentIndex=\"0\""],
            vec![],
        ),
        (
            "getBookmarks",
            vec![],
            vec![
                "position=\"5000\"",
                "comment=\"chorus\"",
                "username=\"alice\"",
            ],
            vec![],
        ),
        // Picture links are absolute at the address the app used, signed,
        // and carry no credential.
        (
            "getArtistInfo2",
            vec![("id", &u2)],
            vec![
                "<artistInfo2",
                "smallImageUrl=\"http://test.local/subsonic/img?id=",
            ],
            vec!["apiKey", "p="],
        ),
        (
            "getArtistInfo",
            vec![("id", &u2)],
            vec!["<artistInfo "],
            vec!["apiKey", "p="],
        ),
        (
            "getAlbumInfo2",
            vec![("id", &ok_computer)],
            vec!["<albumInfo "],
            vec![],
        ),
        (
            "getAlbumInfo",
            vec![("id", &ok_computer)],
            vec!["<albumInfo "],
            vec![],
        ),
        (
            "getLyricsBySongId",
            vec![("id", &one)],
            vec![
                "lang=\"und\"",
                "synced=\"true\"",
                "First line",
                "displayTitle=\"One\"",
            ],
            vec![],
        ),
        (
            "getLyricsBySongId",
            vec![("id", &airbag)],
            vec!["<lyricsList"],
            vec!["<structuredLyrics"],
        ),
        (
            "getLyrics",
            vec![("artist", "U2"), ("title", "One")],
            vec!["First line", "Second line"],
            vec![],
        ),
        (
            "getLyrics",
            vec![("title", "No Such Song")],
            vec!["<lyrics"],
            vec!["First line"],
        ),
        (
            "getGenres",
            vec![],
            vec!["<genre songCount=\"1\" albumCount=\"1\">Rock</genre>"],
            vec![],
        ),
        (
            "getSongsByGenre",
            vec![("genre", "Rock"), ("count", "2")],
            vec!["title=\"One\""],
            vec!["title=\"Airbag\""],
        ),
        // getUser always describes the caller.
        (
            "getUser",
            vec![("username", "ignored-anyone")],
            vec![
                "username=\"alice\"",
                "adminRole=\"false\"",
                "streamRole=\"true\"",
                "maxBitRate=\"320\"",
            ],
            vec![],
        ),
        (
            "getScanStatus",
            vec![],
            vec![
                "scanning=\"false\"",
                "count=\"6\"",
                "folderCount=\"5\"",
                "scanType=\"incremental\"",
            ],
            vec![],
        ),
        (
            "getTopSongs",
            vec![("artist", "U2")],
            vec!["title=\"One\""],
            vec![],
        ),
        (
            "getSimilarSongs2",
            vec![("id", &u2)],
            vec!["title=\"One\""],
            vec![],
        ),
        (
            "getSimilarSongs",
            vec![("id", &u2)],
            vec!["title=\"One\""],
            vec![],
        ),
        (
            "setRating",
            vec![("id", &one), ("rating", "4")],
            vec![],
            vec![],
        ),
        (
            "scrobble",
            vec![("id", &one), ("time", "1700000000000")],
            vec![],
            vec![],
        ),
        (
            "scrobble",
            vec![("id", &one), ("submission", "false")],
            vec![],
            vec![],
        ),
        (
            "reportPlayback",
            vec![
                ("mediaId", &one),
                ("mediaType", "song"),
                ("positionMs", "1000"),
                ("state", "playing"),
            ],
            vec![],
            vec![],
        ),
    ];
    for (endpoint, pairs, present, absent) in cases {
        let answer = client.get(endpoint, &pairs).await;
        let case = format!("{endpoint} {pairs:?}");
        assert_ok(&answer, &case);
        let text = answer.text();
        for needle in present {
            assert!(text.contains(needle), "{case}: missing {needle}\n{text}");
        }
        for needle in absent {
            assert!(
                !text.contains(needle),
                "{case}: unexpected {needle}\n{text}"
            );
        }
    }

    // A scan's time and length are a time and a whole number.
    let status = client.ok("getScanStatus", &[]).await;
    let scan = &status["scanStatus"];
    assert!(
        scan["lastScan"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z'))
            && scan["elapsedTime"].is_u64(),
        "{scan}"
    );

    // An offset page of the unfiltered song list is the next song in it.
    let all = client
        .ok("search3", &[("query", ""), ("songCount", "10")])
        .await;
    let second = client
        .ok(
            "search3",
            &[
                ("songCount", "1"),
                ("songOffset", "1"),
                ("artistCount", "0"),
                ("albumCount", "0"),
            ],
        )
        .await;
    assert_eq!(
        second["searchResult3"]["song"],
        json!([all["searchResult3"]["song"][1]]),
        "{second}"
    );

    // Album lists: newest first; byYear direction follows the bounds' order.
    let newest = client.ok("getAlbumList2", &[("type", "newest")]).await;
    let created: Vec<&str> = newest["albumList2"]["album"]
        .as_array()
        .expect("albums")
        .iter()
        .map(|album| album["created"].as_str().expect("created"))
        .collect();
    assert!(
        created.windows(2).all(|pair| pair[0] >= pair[1]),
        "{newest}"
    );
    let names = |list: Value| -> Vec<String> {
        list["albumList2"]["album"]
            .as_array()
            .expect("albums")
            .iter()
            .map(|album| album["name"].as_str().expect("name").to_owned())
            .collect()
    };
    let up = [("type", "byYear"), ("fromYear", "1990"), ("toYear", "2000")];
    assert_eq!(
        names(client.ok("getAlbumList2", &up).await),
        ["Achtung Baby", "OK Computer", "Mezzanine"]
    );
    let down = [("type", "byYear"), ("fromYear", "2000"), ("toYear", "1990")];
    assert_eq!(
        names(client.ok("getAlbumList2", &down).await),
        ["Mezzanine", "OK Computer", "Achtung Baby"]
    );
    rig.stop().await;
    server.abort();
}

/// The `authorization` header value of the player's own session.
async fn player_session(rig: &CompatApp) -> String {
    let (status, body) = rig
        .app
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(json!({
                "username": PLAYER,
                "password": format!("{PLAYER}-password-1234"),
                "transport": "bearer",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    format!("Bearer {}", body["token"].as_str().expect("session token"))
}

const SLEEVE: [u8; 3] = [200, 10, 10];
const PORTRAIT: [u8; 3] = [10, 200, 10];

/// A square PNG of one colour, `side` pixels wide.
fn png(colour: [u8; 3], side: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbImage::from_pixel(side, side, image::Rgb(colour))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("picture encodes");
    bytes
}

/// Add a folder holding one album with a `cover.png`, and give its artist
/// a MusicBrainz id and a cached picture. Returns the Subsonic ids of the
/// album, its artist and its song.
async fn covered(rig: &CompatApp, client: &Client<'_>) -> [String; 3] {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let second = rig.app.config.root_app_dir.join("second");
    let folder = second.join("Covered/Sleeve");
    std::fs::create_dir_all(&folder).expect("mkdir");
    std::fs::copy(
        manifest.join("tests/fixtures/library/management_full.flac"),
        folder.join("01 Sleeve.flac"),
    )
    .expect("fixture copies");
    std::fs::write(folder.join("cover.png"), png(SLEEVE, 600)).expect("cover writes");
    rig.app.add_library_folder("second", &second).await;

    let all = client
        .ok("search3", &[("query", ""), ("songCount", "500")])
        .await;
    let song = all["searchResult3"]["song"]
        .as_array()
        .expect("songs")
        .iter()
        .find(|song| song["path"] == "Covered/Sleeve/01 Sleeve.flac")
        .expect("the covered song")
        .clone();
    let album = song["albumId"].as_str().expect("album id").to_owned();
    let artist = client.ok("getAlbum", &[("id", &album)]).await["album"]["artistId"]
        .as_str()
        .expect("album artist id")
        .to_owned();

    let mbid = "5b11f4ce-a62d-471e-81fc-a69a8278c7da";
    rig.app
        .db()
        .execute(
            "INSERT OR REPLACE INTO local_artist_external_identities \
             (local_artist_id, provider, provider_artist_id, decision_source, selected_at) \
             VALUES (?1, 'musicbrainz', ?2, 'manual', 1000)",
            [artist.trim_start_matches("ar-"), mbid],
        )
        .expect("artist identity seeds");
    droppedneedle_catalog::reads::platform::artwork::cache::ArtworkCache::new(
        rig.app.config.cache_dir.join("covers"),
        64 * 1024 * 1024,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    )
    .put(
        Some(&format!("artist:{mbid}")),
        png(PORTRAIT, 600),
        "image/png",
    )
    .await;
    [album, artist, id_of(&song)]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cover_art_buckets_caching_and_misses() {
    let rig = CompatApp::start("compat-sub-covers").await;
    let client = Client::player(&rig);
    let [album, artist, song] = covered(&rig, &client).await;
    let immutable = Some("public, max-age=31536000, immutable");
    for (id, size, colour, side) in [
        (&album, None, SLEEVE, 500),
        (&album, Some("100"), SLEEVE, 250),
        // The largest bucket never enlarges a smaller picture.
        (&album, Some("2000"), SLEEVE, 600),
        (&song, None, SLEEVE, 500),
        (&artist, None, PORTRAIT, 500),
    ] {
        let mut pairs = vec![("id", id.as_str())];
        pairs.extend(size.map(|size| ("size", size)));
        let answer = client.get("getCoverArt", &pairs).await;
        assert_eq!(
            (answer.status, answer.content_type()),
            (StatusCode::OK, "image/png"),
            "{id} {size:?}"
        );
        assert_eq!(answer.header("cache-control"), immutable, "{id} {size:?}");
        let picture = image::load_from_memory(&answer.body)
            .expect("a picture")
            .to_rgb8();
        assert_eq!(
            (picture.width(), picture.get_pixel(0, 0).0),
            (side, colour),
            "{id} {size:?}"
        );
    }
    let made = client
        .ok("createPlaylist", &[("name", "Sleeves"), ("songId", &song)])
        .await;
    let playlist = client
        .get("getCoverArt", &[("id", &id_of(&made["playlist"]))])
        .await;
    assert_eq!(
        playlist.header("cache-control"),
        Some("private, max-age=3600")
    );
    assert_eq!(playlist.content_type(), "image/png");
    // A known album without art gets a WebP placeholder drawn from its id:
    // the same id always gets the same bytes, another album other colours.
    // Unknown ids 404 text.
    let ids = Ids::read(&client).await;
    let mut drawn = Vec::new();
    for id in [
        ids.album("Achtung Baby"),
        ids.album("Achtung Baby"),
        ids.album("OK Computer"),
    ] {
        let placeholder = client.get("getCoverArt", &[("id", &id)]).await;
        assert_eq!(
            (placeholder.status, placeholder.content_type()),
            (StatusCode::OK, "image/webp"),
            "{id}"
        );
        assert_eq!(
            placeholder.header("cache-control"),
            Some("public, max-age=86400"),
            "{id}"
        );
        drawn.push(placeholder.body);
    }
    assert_eq!(drawn[0], drawn[1], "one album, one placeholder");
    assert_ne!(drawn[0], drawn[2], "two albums, two placeholders");
    for id in ["al-nope", "xx-1"] {
        let answer = client.get("getCoverArt", &[("id", id)]).await;
        assert_eq!(
            (answer.status, answer.content_type()),
            (StatusCode::NOT_FOUND, "text/plain"),
            "{id}"
        );
    }
    rig.stop().await;
}

pub(crate) const RED: [u8; 3] = [220, 20, 20];
pub(crate) const GREEN: [u8; 3] = [20, 200, 20];

/// Gives Radiohead, an artist with no MusicBrainz link, folder art on two
/// albums: red on OK Computer, and green on Achtung Baby, which is made to
/// credit them too. Their picture is then those two covers crosswise.
/// Returns Radiohead's local id.
pub(crate) fn two_covers_for_radiohead(rig: &CompatApp) -> String {
    let music = rig.app.config.root_app_dir.join("music");
    let db = rig.app.db();
    let album_of = |path: &str| -> (String, String) {
        db.query_row(
            "SELECT a.id, a.album_artist_id FROM local_tracks t \
             JOIN local_albums a ON a.id = t.local_album_id WHERE t.relative_path = ?1",
            [path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the album")
    };
    let (ok_computer, radiohead) = album_of("Radiohead/OK Computer/01 Airbag.flac");
    let (achtung_baby, _) = album_of("U2/Achtung Baby/03 One.mp3");
    for (album, folder, colour) in [
        (&ok_computer, "Radiohead/OK Computer", RED),
        (&achtung_baby, "U2/Achtung Baby", GREEN),
    ] {
        let cover = music.join(folder).join("cover.png");
        std::fs::write(&cover, png(colour, 300)).expect("cover writes");
        db.execute(
            "INSERT INTO local_album_artwork \
             (local_album_id, source, source_locator, version, updated_at) \
             VALUES (?1, 'folder', ?2, 1, 1)",
            [album.as_str(), cover.to_str().expect("utf-8 path")],
        )
        .expect("art row");
    }
    db.execute(
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
         SELECT ?1, MAX(position) + 1, ?2, 'main' FROM local_album_artists \
         WHERE local_album_id = ?1",
        [achtung_baby.as_str(), radiohead.as_str()],
    )
    .expect("credit row");
    radiohead
}

/// Checks `body` is a `side` pixel JPEG whose quadrants (left to right,
/// top to bottom) show `quadrants`.
pub(crate) fn assert_quadrants(body: &[u8], side: u32, quadrants: [[u8; 3]; 4]) {
    let picture = image::load_from_memory_with_format(body, image::ImageFormat::Jpeg)
        .expect("a JPEG")
        .to_rgb8();
    assert_eq!(picture.dimensions(), (side, side));
    let (near, far) = (side / 4, side * 3 / 4);
    for ((x, y), want) in [(near, near), (far, near), (near, far), (far, far)]
        .into_iter()
        .zip(quadrants)
    {
        let got = picture.get_pixel(x, y).0;
        assert!(
            got.iter()
                .zip(want)
                .all(|(got, want)| got.abs_diff(want) <= 30),
            "pixel at ({x}, {y}) is {got:?}, wanted {want:?}"
        );
    }
}

/// An artist with no MusicBrainz link gets the picture the web UI shows,
/// the mosaic of their albums' art, at the bucket the size asks for. One
/// with no art keeps its placeholder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unlinked_artist_shows_their_album_mosaic() {
    let rig = CompatApp::start("compat-sub-artist-mosaic").await;
    let client = Client::player(&rig);
    let radiohead = format!("ar-{}", two_covers_for_radiohead(&rig));
    let artist = client.ok("getArtist", &[("id", &radiohead)]).await;
    assert_eq!(artist["artist"]["coverArt"], radiohead.as_str(), "{artist}");
    for (size, side) in [(Some("100"), 250), (None, 500)] {
        let mut pairs = vec![("id", radiohead.as_str())];
        pairs.extend(size.map(|size| ("size", size)));
        let answer = client.get("getCoverArt", &pairs).await;
        assert_eq!(
            (answer.status, answer.content_type()),
            (StatusCode::OK, "image/jpeg"),
            "{size:?}"
        );
        assert_quadrants(&answer.body, side, [RED, GREEN, GREEN, RED]);
    }
    let ids = Ids::read(&client).await;
    let bare = client
        .get("getCoverArt", &[("id", &ids.artist("Massive Attack"))])
        .await;
    assert_eq!(
        (bare.status, bare.content_type()),
        (StatusCode::OK, "image/webp")
    );
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_endpoints_render_errors_as_text() {
    use base64::Engine as _;

    let rig = CompatApp::start("compat-sub-binary").await;
    let client = Client::player(&rig);
    let one = Ids::read(&client).await.song("One");

    let answer = client.get("stream", &[("id", &one)]).await;
    assert_eq!(answer.header("content-encoding"), Some("identity"));
    assert_eq!(answer.body, rig.file("one"));

    let answer = client.get("download", &[("id", &one)]).await;
    assert_eq!(
        answer.header("content-disposition"),
        Some("attachment; filename=\"One.mp3\"")
    );
    use droppedneedle_compat::subsonic::stream::download_filename;
    assert_eq!(download_filename("a/b:c*d", "MP3"), "a_b_c_d.mp3");
    assert_eq!(download_filename("...", "flac"), "track.flac");

    let avatar = png([10, 10, 200], 8);
    let session = player_session(&rig).await;
    let (status, body) = rig
        .app
        .call(
            "POST",
            "/api/v3/me/avatar",
            &[("authorization", session.as_str())],
            Some(json!({
                "content_type": "image/png",
                "image_base64": base64::engine::general_purpose::STANDARD.encode(&avatar),
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answer = client.get("getAvatar", &[("username", PLAYER)]).await;
    assert_eq!(
        (answer.content_type(), answer.body.as_slice()),
        ("image/png", avatar.as_slice())
    );
    let answer = client
        .get("getAvatar", &[("username", "someone-else")])
        .await;
    assert_eq!(
        (answer.status, answer.content_type()),
        (StatusCode::FORBIDDEN, "text/plain")
    );
    assert_eq!(
        answer.text(),
        "Avatar access is limited to the authenticated user"
    );

    // Auth failures stay enveloped on binary endpoints. A stream miss is
    // enveloped too; other not-found answers stay text.
    use droppedneedle_compat::subsonic::dispatch_uses_envelope;
    assert!(dispatch_uses_envelope(50, "stream"));
    assert!(dispatch_uses_envelope(40, "getavatar"));
    assert!(dispatch_uses_envelope(70, "stream"));
    assert!(!dispatch_uses_envelope(70, "download"));
    assert!(dispatch_uses_envelope(70, "getsong"));
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcode_decision_needs_a_json_post_for_an_existing_song() {
    let rig = CompatApp::start("compat-sub-decision").await;
    let client = Client::player(&rig);
    let airbag = Ids::read(&client).await.song("Airbag");
    let song = [("mediaId", airbag.as_str()), ("mediaType", "song")];
    // A client that names no profile it can play gets no playback path.
    let bare = br#"{"name":"Feishin","platform":"web","maxAudioBitrate":0}"#;
    let text = client
        .post("getTranscodeDecision", &song, "application/json", bare)
        .await
        .text();
    for needle in [
        "<transcodeDecision",
        "canDirectPlay=\"false\"",
        "canTranscode=\"false\"",
    ] {
        assert!(text.contains(needle), "{text}");
    }
    let flac = br#"{"name":"Feishin","platform":"web","maxAudioBitrate":0,
        "directPlayProfiles":[{"containers":["flac"],"audioCodecs":["flac"],"protocols":["http"]}]}"#;
    let text = client
        .post("getTranscodeDecision", &song, "application/json", flac)
        .await
        .text();
    assert!(text.contains("canDirectPlay=\"true\""), "{text}");
    // The signed parameters of a direct play stream the original file.
    let params = regex::Regex::new("transcodeParams=\"([^\"]+)\"")
        .expect("pattern")
        .captures(&text)
        .unwrap_or_else(|| panic!("no transcodeParams: {text}"))[1]
        .to_owned();
    let stream = client
        .get(
            "getTranscodeStream",
            &[
                ("mediaId", &airbag),
                ("mediaType", "song"),
                ("transcodeParams", &params),
            ],
        )
        .await;
    assert_eq!(stream.body, rig.file("airbag"));

    /// Query pairs, content type, body and a needle the answer carries.
    type Case<'a> = (&'a [(&'a str, &'a str)], &'a str, &'a [u8], &'a str);
    let minimal = br#"{"name":"x","platform":"y"}"#;
    let cases: [Case; 4] = [
        (
            &song,
            "application/x-www-form-urlencoded",
            b"{}",
            "requires JSON",
        ),
        (
            &song,
            "application/json",
            br#"{"name":"x","platform":"y","bogus":1}"#,
            "code=\"10\"",
        ),
        (
            &[("mediaId", airbag.as_str()), ("mediaType", "podcast")],
            "application/json",
            minimal,
            "Only song",
        ),
        (
            &[("mediaId", "tr-nope"), ("mediaType", "song")],
            "application/json",
            minimal,
            "code=\"70\"",
        ),
    ];
    for (pairs, content_type, body, needle) in cases {
        let text = client
            .post("getTranscodeDecision", pairs, content_type, body)
            .await
            .text();
        assert!(text.contains(needle), "{needle}: {text}");
    }
    assert!(
        client
            .get("getTranscodeDecision", &song)
            .await
            .text()
            .contains("requires POST")
    );
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutations_persist() {
    let rig = CompatApp::start("compat-sub-mutations").await;
    let client = Client::player(&rig);
    let ids = Ids::read(&client).await;
    let (one, airbag, teardrop) = (ids.song("One"), ids.song("Airbag"), ids.song("Teardrop"));
    let ok_computer = ids.album("OK Computer");

    // Playlists: create, replace contents by id, rename and remove by index,
    // delete.
    let text = client
        .get("createPlaylist", &[("name", "New"), ("songId", &one)])
        .await
        .text();
    assert!(
        text.contains("name=\"New\"") && text.contains("songCount=\"1\""),
        "{text}"
    );
    let made = client
        .ok("createPlaylist", &[("name", "Other"), ("songId", &one)])
        .await;
    let playlist = id_of(&made["playlist"]);
    let text = client
        .get(
            "createPlaylist",
            &[("playlistId", &playlist), ("songId", &teardrop)],
        )
        .await
        .text();
    assert!(
        text.contains("songCount=\"1\"")
            && text.contains("title=\"Teardrop\"")
            && !text.contains("title=\"One\""),
        "{text}"
    );
    client
        .ok(
            "createPlaylist",
            &[
                ("playlistId", &playlist),
                ("songId", &one),
                ("songId", &airbag),
            ],
        )
        .await;
    let updated = client
        .get(
            "updatePlaylist",
            &[
                ("playlistId", &playlist),
                ("name", "Renamed"),
                ("songIndexToRemove", "0"),
            ],
        )
        .await;
    assert_ok(&updated, "updatePlaylist");
    let text = client.get("getPlaylist", &[("id", &playlist)]).await.text();
    assert!(
        text.contains("name=\"Renamed\"") && text.contains("songCount=\"1\""),
        "{text}"
    );
    assert_ok(
        &client.get("deletePlaylist", &[("id", &playlist)]).await,
        "deletePlaylist",
    );
    assert_code(
        &client.get("getPlaylist", &[("id", &playlist)]).await,
        70,
        "deleted",
    );

    // Stars.
    assert_ok(
        &client
            .get("star", &[("id", &airbag), ("albumId", &ok_computer)])
            .await,
        "star",
    );
    let text = client.get("getStarred2", &[]).await.text();
    assert!(
        text.contains("title=\"Airbag\"") && text.contains("name=\"OK Computer\""),
        "{text}"
    );
    assert_ok(&client.get("unstar", &[("id", &airbag)]).await, "unstar");

    // Play queues: `current` must reference a queued song.
    let text = client
        .get("savePlayQueue", &[("id", &one), ("current", &teardrop)])
        .await
        .text();
    assert!(text.contains("must reference a queued song"), "{text}");
    let saved = client
        .get(
            "savePlayQueue",
            &[("id", &airbag), ("current", &airbag), ("position", "5")],
        )
        .await;
    assert_ok(&saved, "savePlayQueue");
    assert!(
        client
            .get("getPlayQueue", &[])
            .await
            .text()
            .contains(&format!("current=\"{airbag}\""))
    );
    let saved = client
        .get(
            "savePlayQueueByIndex",
            &[("id", &one), ("currentIndex", "0")],
        )
        .await;
    assert_ok(&saved, "savePlayQueueByIndex");

    // Bookmarks.
    assert_ok(
        &client
            .get("createBookmark", &[("id", &airbag), ("position", "10")])
            .await,
        "create",
    );
    assert_ok(
        &client.get("deleteBookmark", &[("id", &airbag)]).await,
        "delete",
    );

    // Admin-only calls succeed for an admin.
    let admin = Client::admin(&rig).await;
    let user = admin.get("getUser", &[("username", "x")]).await;
    assert!(
        user.text().contains("adminRole=\"true\""),
        "{}",
        user.text()
    );
    let scan = admin.get("startScan", &[]).await;
    assert!(scan.text().contains("scanning=\"true\""), "{}", scan.text());
    rig.stop().await;
}

/// With an ffmpeg (the stand-in) songs name what they transcode to, and a
/// transcode streams without ranges or caching, answers HEAD without
/// running ffmpeg and carries a length only when asked to estimate one.
/// Signed transcode parameters stop working once transcoding is off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcoding_streams() {
    let rig = CompatApp::transcoding("compat-sub-transcoding").await;
    let client = Client::player(&rig);
    let ids = Ids::read(&client).await;
    let (one, airbag) = (ids.song("One"), ids.song("Airbag"));

    let text = client.get("getSong", &[("id", &one)]).await.text();
    assert!(
        text.contains("transcodedContentType=\"audio/mpeg\"")
            && text.contains("transcodedSuffix=\"mp3\""),
        "{text}"
    );

    let opus = [("id", one.as_str()), ("format", "opus")];
    // HEAD answers the transcode headers without running ffmpeg.
    let head = client.send("HEAD", "stream", &opus).await;
    assert_eq!(
        (head.status, head.content_type()),
        (StatusCode::OK, "audio/ogg")
    );
    assert!(head.body.is_empty());
    assert!(rig.ffmpeg_runs("one").is_empty(), "HEAD ran ffmpeg");

    let stream = client.get("stream", &opus).await;
    assert_eq!(
        (stream.status, stream.content_type()),
        (StatusCode::OK, "audio/ogg")
    );
    assert_eq!(stream.header("accept-ranges"), Some("none"));
    assert_eq!(stream.header("cache-control"), Some("no-store"));
    assert!(
        stream.text().starts_with("ffmpeg stand-in: ogg "),
        "{}",
        stream.text()
    );
    assert_eq!(rig.ffmpeg_runs("one").len(), 1);

    // Only transcodes carry the estimated length: the bitrate over the
    // track's duration.
    let estimate = [
        ("id", airbag.as_str()),
        ("format", "mp3"),
        ("maxBitRate", "128"),
        ("estimateContentLength", "true"),
    ];
    let stream = client.get("stream", &estimate).await;
    assert_eq!(stream.text(), "ffmpeg stand-in: mp3 128k\n");
    let length: u64 = stream
        .header("content-length")
        .expect("an estimated length")
        .parse()
        .expect("a number");
    let seconds = named(&ids.songs, "title", "Airbag")["duration"]
        .as_u64()
        .expect("duration");
    let per_second = 128 * 1000 / 8;
    assert!(
        (per_second * seconds.saturating_sub(1)..=per_second * (seconds + 1)).contains(&length),
        "{length} for {seconds}s"
    );

    // Signed parameters for a transcode stream it, until transcoding is
    // turned off.
    let song = [("mediaId", airbag.as_str()), ("mediaType", "song")];
    let mp3_only = br#"{"name":"Feishin","platform":"web","maxAudioBitrate":0,
        "transcodingProfiles":[{"container":"mp3","audioCodec":"mp3","protocol":"http"}]}"#;
    let text = client
        .post("getTranscodeDecision", &song, "application/json", mp3_only)
        .await
        .text();
    assert!(text.contains("canTranscode=\"true\""), "{text}");
    let params = regex::Regex::new("transcodeParams=\"([^\"]+)\"")
        .expect("pattern")
        .captures(&text)
        .unwrap_or_else(|| panic!("no transcodeParams: {text}"))[1]
        .to_owned();
    let signed = [
        ("mediaId", airbag.as_str()),
        ("mediaType", "song"),
        ("transcodeParams", params.as_str()),
    ];
    let stream = client.get("getTranscodeStream", &signed).await;
    assert_eq!(stream.text(), "ffmpeg stand-in: mp3 320k\n");

    rig.set_connect_apps(json!({"transcoding_enabled": false}))
        .await;
    let refused = client.get("getTranscodeStream", &signed).await;
    assert_eq!(
        (refused.status, refused.content_type()),
        (StatusCode::NOT_FOUND, "text/plain")
    );
    assert!(
        refused.text().contains("Transcoding is disabled"),
        "{}",
        refused.text()
    );
    rig.stop().await;
}

/// When `stream` transcodes: only on a real trigger, never when forced
/// original, disabled or without ffmpeg. The stream gateway makes this one
/// decision for every protocol.
#[test]
fn stream_transcode_decision() {
    use droppedneedle_media::stream::transcode::{
        OutFormat, StreamPlan, TrackInfo, TranscodeSettings, decide,
    };

    let source = |format: &str, bitrate: i64| TrackInfo {
        file_format: format.to_owned(),
        bitrate_kbps: Some(bitrate),
        duration_seconds: 0.0,
    };
    let plan = |source: TrackInfo, requested: Option<&str>, ceiling: Option<i64>| {
        decide(
            &source,
            requested,
            ceiling,
            false,
            0.0,
            &TranscodeSettings::default(),
            true,
        )
    };
    // A server ceiling alone, or a zero client bitrate (Feishin), is no trigger.
    assert!(!plan(source("flac", 900), None, None).is_transcode());
    assert!(!plan(source("flac", 900), None, Some(0)).is_transcode());
    assert!(!plan(source("mp3", 320), Some("mp3"), None).is_transcode());
    assert!(matches!(
        plan(source("flac", 900), None, Some(128)),
        StreamPlan::Transcode {
            out_format: OutFormat::Mp3,
            out_bitrate_kbps: 128,
            ..
        }
    ));
    assert_eq!(
        plan(source("mp3", 320), Some("opus"), None).out_format(),
        Some(OutFormat::Opus)
    );
    // The floor is 64 kbps on every protocol.
    assert!(matches!(
        plan(source("flac", 900), None, Some(48)),
        StreamPlan::Transcode {
            out_bitrate_kbps: 64,
            ..
        }
    ));
    for (force, enabled, ffmpeg) in [
        (true, true, true),
        (false, false, true),
        (false, true, false),
    ] {
        let settings = TranscodeSettings {
            transcoding_enabled: enabled,
            ..TranscodeSettings::default()
        };
        let plan = decide(
            &source("flac", 900),
            Some("opus"),
            Some(64),
            force,
            5.0,
            &settings,
            ffmpeg,
        );
        assert!(!plan.is_transcode());
    }
}

/// Request parsing bounds and XML escaping: both guard against hostile input.
#[test]
fn request_limits_and_xml_escaping() {
    use droppedneedle_compat::subsonic::params::{
        MAX_REQUEST_PARAMETER_BYTES, check_limits, decode_pairs,
    };
    use droppedneedle_compat::subsonic::value::{SubsonicFormat, render_error};

    assert_eq!(MAX_REQUEST_PARAMETER_BYTES, 64 * 1024);
    assert_eq!(
        decode_pairs(b"a=1&b=x+y&q=%22%22").expect("decodes"),
        vec![
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "x y".to_owned()),
            ("q".to_owned(), "\"\"".to_owned()),
        ]
    );
    for bad in [&b"a=%"[..], b"a=%zz", b"a=%2", &[0xff]] {
        assert_eq!(decode_pairs(bad).expect_err("rejects").code, 10, "{bad:?}");
    }
    let pair = |key: &str, value: String| vec![(key.to_owned(), value)];
    assert!(check_limits(&pair("a", "b".to_owned())).is_ok());
    assert_eq!(
        check_limits(&pair("", "b".to_owned()))
            .expect_err("empty key")
            .code,
        10
    );
    assert_eq!(
        check_limits(&pair("a", "x".repeat(9000)))
            .expect_err("long value")
            .code,
        10
    );
    let many = vec![("a".to_owned(), "b".to_owned()); 2000];
    assert_eq!(check_limits(&many).expect_err("too many").code, 10);

    let xml = render_error(0, "x\x07\x1b&<>\"", SubsonicFormat::Xml, None, "s", "v").body_text();
    assert!(
        xml.contains("message=\"x&amp;&lt;&gt;&quot;\"") && !xml.contains('\x07'),
        "{xml}"
    );
}

/// A read that finds the database busy answers 503 with `Retry-After` in
/// both protocols, so an app asks again instead of showing an empty
/// library: Subsonic in its failed envelope with code 0, Jellyfin with an
/// empty body. The compat routers run over a database with one pooled
/// reader for the collections and a short wait for it; the test holds that
/// reader, so the playlist read gives up as busy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_read_answers_503_with_retry_after_in_both_protocols() {
    use std::net::SocketAddr;
    use std::time::Duration;

    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use droppedneedle_auth::users::roles::Role;
    use droppedneedle_catalog::reads::collections::CollectionsState;
    use droppedneedle_catalog::reads::collections::db::CollectionsDb;
    use droppedneedle_catalog::reads::library::store::LocalCatalog;
    use droppedneedle_compat::CompatSetup;
    use droppedneedle_compat::setup::CompatDeps;
    use droppedneedle_kernel::db::{DbConfig, LaneLimits, open_runtime};
    use droppedneedle_kernel::runtime_config::sections::ConnectApps;
    use tower::ServiceExt as _;

    const SECRET: &str = "alice-app-secret";
    let scope = crate::common::TestApp::start("busy-read-scope").await;
    let dir = crate::common::ScratchDir::new("busy-read");
    let path = dir.join("app.db");
    crate::common::migrated_db(&path);
    let mut config = DbConfig::new(&path)
        .with_lane_limits(LaneLimits::patient())
        .with_read_wait(Duration::from_millis(100));
    config.readers = 1;
    let runtime = open_runtime(&config).await.expect("runtime opens");
    let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
        .build()
        .await;
    let auth = crate::common::auth::AuthRig::on(&platform);
    let user = auth.seed_user("alice", Role::User).await;
    auth.seed_app_password(&user.id, SECRET).await;
    let (library, _) = crate::common::library::library_at(&platform);
    let reads = crate::common::bundles::reads_at(
        &platform,
        auth.deps.clone(),
        crate::common::reads_inputs(&dir),
        &library,
        CollectionsState::new(
            CollectionsDb::new(runtime.db().clone(), platform.clock.clone()),
            LocalCatalog::new(runtime.db().clone()),
        ),
        None,
    );
    let collections = crate::common::bundles::collections_at(&platform);
    let acquire = crate::common::bundles::acquire_at(
        &platform,
        auth.deps.clone(),
        &library,
        &collections.stores,
    );
    let media =
        crate::common::bundles::media_at(&platform, auth.deps.clone(), &library, &reads, &acquire);
    platform
        .settings
        .save_blocking(ConnectApps {
            subsonic_enabled: true,
            jellyfin_enabled: true,
            ..ConnectApps::default()
        })
        .expect("connect apps save");
    let deps = CompatDeps::over_reads(
        &platform,
        auth.deps.clone(),
        media.playback.clone(),
        media.stream.engine.clone(),
        library.scans.clone(),
        &reads,
    );
    let compat = CompatSetup::build(deps);
    compat
        .jellyfin_ids()
        .fill()
        .pass()
        .await
        .expect("jellyfin ids load");
    let router = scope.request_scope(compat.router());
    let ask = |uri: String, authorization: Option<String>| {
        let router = router.clone();
        async move {
            let mut request = axum::http::Request::get(uri);
            if let Some(authorization) = authorization {
                request = request.header("authorization", authorization);
            }
            let mut request = request.body(Body::empty()).expect("request builds");
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
            let response = router.oneshot(request).await.expect("router answers");
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("body reads");
            (status, retry_after, body.to_vec())
        }
    };
    let subsonic = uri(
        "getPlaylists",
        &[
            ("u", "alice"),
            ("p", SECRET),
            ("v", "1.16.1"),
            ("c", "test"),
            ("f", "json"),
        ],
    );
    let jellyfin = "/jellyfin/Items?IncludeItemTypes=Playlist".to_owned();
    let token = Some(format!("MediaBrowser Client=\"Test\", Token=\"{SECRET}\""));

    let held = runtime.db().sqlx().acquire().await.expect("reader held");
    let (status, retry_after, body) = ask(subsonic.clone(), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry_after.as_deref(), Some("1"));
    let envelope: Value = serde_json::from_slice(&body).expect("envelope is json");
    assert_eq!(envelope["subsonic-response"]["status"], "failed");
    assert_eq!(envelope["subsonic-response"]["error"]["code"], 0);
    let (status, retry_after, body) = ask(jellyfin.clone(), token.clone()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry_after.as_deref(), Some("1"));
    assert!(body.is_empty());

    drop(held);
    assert_eq!(ask(subsonic, None).await.0, StatusCode::OK);
    assert_eq!(ask(jellyfin, token).await.0, StatusCode::OK);
    drop(router);
    scope.stop().await;
}

/// A busy answer to a write means nothing was written, so asking again is
/// safe. `createPlaylist` reads everything it needs before its one write:
/// with the playlist and favorites reads busy it answers 503 with
/// `Retry-After` and no playlist exists, and asking again once they are
/// free makes exactly one playlist holding the song. The compat routers
/// run over one database file whose collections go through a runtime with
/// one pooled reader and a short wait for it; the test holds that reader.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_playlist_create_writes_nothing_and_asking_again_makes_one() {
    use std::net::SocketAddr;
    use std::time::Duration;

    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use droppedneedle_auth::users::roles::Role;
    use droppedneedle_catalog::reads::collections::CollectionsState;
    use droppedneedle_catalog::reads::collections::db::CollectionsDb;
    use droppedneedle_catalog::reads::library::store::LocalCatalog;
    use droppedneedle_compat::CompatSetup;
    use droppedneedle_compat::setup::CompatDeps;
    use droppedneedle_kernel::db::{DbConfig, LaneLimits, open_runtime};
    use droppedneedle_kernel::runtime_config::sections::ConnectApps;
    use tower::ServiceExt as _;

    const SECRET: &str = "alice-app-secret";
    let scope = crate::common::TestApp::start("busy-write-scope").await;
    let dir = crate::common::ScratchDir::new("busy-write");
    let path = dir.join("app.db");
    crate::common::migrated_db(&path);
    let main = open_runtime(&DbConfig::new(&path))
        .await
        .expect("runtime opens");
    let mut starved = DbConfig::new(&path)
        .with_lane_limits(LaneLimits::patient())
        .with_read_wait(Duration::from_millis(100));
    starved.readers = 1;
    let starved = open_runtime(&starved).await.expect("starved runtime opens");
    let platform = crate::common::TestPlatform::on(main.db(), &dir)
        .build()
        .await;
    let auth = crate::common::auth::AuthRig::on(&platform);
    let user = auth.seed_user("alice", Role::User).await;
    auth.seed_app_password(&user.id, SECRET).await;
    rusqlite::Connection::open(&path)
        .expect("database opens")
        .execute_batch(crate::compat_catalog::LIBRARY)
        .expect("catalog seeds");
    let (library, _) = crate::common::library::library_at(&platform);
    let reads = crate::common::bundles::reads_at(
        &platform,
        auth.deps.clone(),
        crate::common::reads_inputs(&dir),
        &library,
        CollectionsState::new(
            CollectionsDb::new(starved.db().clone(), platform.clock.clone()),
            LocalCatalog::new(main.db().clone()),
        ),
        None,
    );
    let collections = crate::common::bundles::collections_at(&platform);
    let acquire = crate::common::bundles::acquire_at(
        &platform,
        auth.deps.clone(),
        &library,
        &collections.stores,
    );
    let media =
        crate::common::bundles::media_at(&platform, auth.deps.clone(), &library, &reads, &acquire);
    platform
        .settings
        .save_blocking(ConnectApps {
            subsonic_enabled: true,
            ..ConnectApps::default()
        })
        .expect("connect apps save");
    let deps = CompatDeps::over_reads(
        &platform,
        auth.deps.clone(),
        media.playback.clone(),
        media.stream.engine.clone(),
        library.scans.clone(),
        &reads,
    );
    let router = scope.request_scope(CompatSetup::build(deps).router());
    let create = uri(
        "createPlaylist",
        &[
            ("u", "alice"),
            ("p", SECRET),
            ("v", "1.16.1"),
            ("c", "test"),
            ("f", "json"),
            ("name", "Mix"),
            ("songId", "tr-trk-1"),
        ],
    );
    let ask = || {
        let router = router.clone();
        let create = create.clone();
        async move {
            let mut request = axum::http::Request::get(create)
                .body(Body::empty())
                .expect("request builds");
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
            let response = router.oneshot(request).await.expect("router answers");
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("body reads");
            let body: Value = serde_json::from_slice(&body).expect("envelope is json");
            (status, retry_after, body)
        }
    };
    let rows = || {
        let db = rusqlite::Connection::open(&path).expect("database opens");
        let count = |table: &str| {
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("rows count")
        };
        (count("playlists"), count("playlist_tracks"))
    };

    let held = starved.db().sqlx().acquire().await.expect("reader held");
    let (status, retry_after, body) = ask().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(retry_after.as_deref(), Some("1"));
    assert_eq!(body["subsonic-response"]["error"]["code"], 0);
    assert_eq!(rows(), (0, 0), "a busy answer wrote nothing");

    drop(held);
    let (status, _, body) = ask().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["subsonic-response"]["playlist"]["songCount"], 1);
    assert_eq!(rows(), (1, 1), "asking again made one playlist");
    drop(router);
    scope.stop().await;
}
