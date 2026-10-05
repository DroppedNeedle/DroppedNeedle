//! Contract, quirk, and degradation briefs for the stage-5 audio
//! metadata providers (AudioDB, Last.fm, ListenBrainz, AcoustID).
//!
//! Every brief runs against a scripted TCP fake on loopback or a closed
//! port; no test touches the live network.

use droppedneedle::providers::{DegradationSink, NoopSink, Pacer};
use droppedneedle::providers::{acoustid, audiodb, lastfm, listenbrainz};

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------------------------------------------------------------------------
// Scripted HTTP fakes
// ---------------------------------------------------------------------------

/// One request the fake observed.
#[derive(Debug, Clone)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn text_body(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// One scripted reply.
#[derive(Debug, Clone)]
struct ScriptedResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl ScriptedResponse {
    fn json(status: u16, value: &serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }

    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

type Responder = Arc<dyn Fn(&CapturedRequest) -> ScriptedResponse + Send + Sync>;

fn always(scripted: ScriptedResponse) -> Responder {
    Arc::new(move |_| scripted.clone())
}

/// A scripted HTTP server on loopback. Aborted on drop.
struct Fake {
    base_url: String,
    hits: Arc<Mutex<Vec<CapturedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fake {
    async fn start(respond: Responder) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake binds loopback");
        let port = listener.local_addr().expect("fake has a port").port();
        let hits: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn(serve(listener, hits.clone(), respond));
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            hits,
            task,
        }
    }

    fn hit_count(&self) -> usize {
        self.hits.lock().expect("hits lock").len()
    }

    fn hits(&self) -> Vec<CapturedRequest> {
        self.hits.lock().expect("hits lock").clone()
    }
}

fn headers_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

async fn read_exactly(stream: &mut tokio::net::TcpStream, mut need: usize, body: &mut Vec<u8>) {
    while need > 0 {
        let mut chunk = vec![0u8; need.min(8192)];
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await;
        match read {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => {
                chunk.truncate(n);
                body.extend_from_slice(&chunk);
                need -= n;
            }
            Ok(Err(_)) => break,
        }
    }
}

async fn serve(
    listener: tokio::net::TcpListener,
    hits: Arc<Mutex<Vec<CapturedRequest>>>,
    respond: Responder,
) {
    loop {
        let accepted = listener.accept().await;
        let Ok((mut stream, _)) = accepted else {
            break;
        };
        let hits = hits.clone();
        let respond = respond.clone();
        tokio::spawn(async move {
            let mut buffer: Vec<u8> = Vec::new();
            loop {
                let mut tmp = [0u8; 4096];
                let read =
                    tokio::time::timeout(Duration::from_secs(5), stream.read(&mut tmp)).await;
                match read {
                    Ok(Ok(0)) | Err(_) => return,
                    Ok(Ok(n)) => {
                        buffer.extend_from_slice(&tmp[..n]);
                        if buffer.len() > 65536 || headers_end(&buffer).is_some() {
                            break;
                        }
                    }
                    Ok(Err(_)) => return,
                }
            }
            let head_end = headers_end(&buffer).unwrap_or(buffer.len());
            let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
            let mut lines = head.split("\r\n");
            let request_line = lines.next().unwrap_or("");
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or("").to_owned();
            let path = parts.next().unwrap_or("").to_owned();
            let mut headers: Vec<(String, String)> = Vec::new();
            let mut content_length = 0usize;
            for line in lines {
                if let Some((key, value)) = line.split_once(':') {
                    let key = key.trim().to_ascii_lowercase();
                    if key == "content-length" {
                        content_length = value.trim().parse::<usize>().unwrap_or(0).min(1 << 20);
                    }
                    headers.push((key, value.trim().to_owned()));
                }
            }
            let mut body = buffer.get(head_end..).unwrap_or(&[]).to_vec();
            if body.len() < content_length {
                read_exactly(&mut stream, content_length - body.len(), &mut body).await;
            }
            body.truncate(content_length);
            let captured = CapturedRequest {
                method,
                path,
                headers,
                body,
            };
            let scripted = respond(&captured);
            hits.lock().expect("hits lock").push(captured);
            let reason = match scripted.status {
                200 => "OK",
                204 => "No Content",
                400 => "Bad Request",
                401 => "Unauthorized",
                403 => "Forbidden",
                404 => "Not Found",
                405 => "Method Not Allowed",
                429 => "Too Many Requests",
                500 => "Internal Server Error",
                _ => "Reply",
            };
            let mut head = format!(
                "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n",
                scripted.status,
                reason,
                scripted.body.len()
            );
            for (key, value) in &scripted.headers {
                head.push_str(&format!("{key}: {value}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&scripted.body).await;
        });
    }
}

/// A loopback URL with nothing listening: connects fail fast.
async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("dead port binds");
    let port = listener.local_addr().expect("dead port has a port").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// Percent-decode one query or form component.
fn percent_decode(input: &str) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(input.len());
    let mut chars = input.as_bytes().iter().copied();
    while let Some(byte) = chars.next() {
        match byte {
            b'%' => {
                let hex = |next: Option<u8>| {
                    next.and_then(|digit| (digit as char).to_digit(16))
                        .unwrap_or(0) as u8
                };
                bytes.push(hex(chars.next()) * 16 + hex(chars.next()));
            }
            b'+' => bytes.push(b' '),
            _ => bytes.push(byte),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Split a query string or form body into decoded pairs.
fn pairs(encoded: &str) -> Vec<(String, String)> {
    encoded
        .split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.split_once('='))
        .map(|(key, value)| (percent_decode(key), percent_decode(value)))
        .collect()
}

/// Split a request path into its route and decoded query pairs.
fn route_and_query(path: &str) -> (String, Vec<(String, String)>) {
    match path.split_once('?') {
        Some((route, query)) => (route.to_owned(), pairs(query)),
        None => (path.to_owned(), Vec::new()),
    }
}

fn query_value(query: &[(String, String)], name: &str) -> Option<String> {
    query
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

// ---------------------------------------------------------------------------
// Port fakes
// ---------------------------------------------------------------------------

/// Recording degradation sink shared with the client under test.
#[derive(Debug, Clone)]
struct RecSink {
    records: Arc<Mutex<Vec<(String, String)>>>,
}

impl RecSink {
    fn new() -> Self {
        Self {
            records: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn count(&self) -> usize {
        self.records.lock().expect("sink lock").len()
    }

    fn messages(&self) -> Vec<String> {
        self.records
            .lock()
            .expect("sink lock")
            .iter()
            .map(|(_, message)| message.clone())
            .collect()
    }

    fn sources(&self) -> Vec<String> {
        self.records
            .lock()
            .expect("sink lock")
            .iter()
            .map(|(source, _)| source.clone())
            .collect()
    }
}

/// Counting pacer: every wire attempt must pace exactly once.
#[derive(Debug, Clone)]
struct CountingPacer {
    count: Arc<Mutex<usize>>,
}

impl CountingPacer {
    fn new() -> Self {
        Self {
            count: Arc::new(Mutex::new(0)),
        }
    }

    fn count(&self) -> usize {
        *self.count.lock().expect("pacer lock")
    }
}

impl Pacer for CountingPacer {
    async fn acquire(&self) {
        *self.count.lock().expect("pacer lock") += 1;
    }
}

impl DegradationSink for RecSink {
    fn record(&self, source: &'static str, message: String) {
        self.records
            .lock()
            .expect("sink lock")
            .push((source.to_owned(), message));
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

// ---------------------------------------------------------------------------
// AudioDB briefs
// ---------------------------------------------------------------------------

fn audiodb_client(
    fake: &Fake,
    pacer: CountingPacer,
    sink: RecSink,
) -> audiodb::AudioDbClient<CountingPacer, RecSink> {
    audiodb::AudioDbClient::new(http(), &fake.base_url, pacer, sink)
}

fn audiodb_artist_payload() -> serde_json::Value {
    serde_json::json!({
        "artists": [{
            "idArtist": "111239",
            "strArtist": "Radiohead",
            "strMusicBrainzID": "a74b1b7f-71a5-4011-9441-d0b5e4122711",
            "strArtistThumb": "https://thumb.example/a.jpg",
            "strArtistFanart": "https://fan.example/a.jpg",
            "strBrandNewField": "ignored, like v2's tolerant structs"
        }]
    })
}

#[tokio::test]
async fn audiodb_artist_by_mbid_found_tolerates_unknown_fields() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &audiodb_artist_payload(),
    )))
    .await;
    let sink = RecSink::new();
    let client = audiodb_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client
        .artist_by_mbid("a74b1b7f-71a5-4011-9441-d0b5e4122711")
        .await;

    let artist = outcome.into_option().expect("artist found");
    assert_eq!(artist.id_artist, "111239");
    assert_eq!(artist.name, "Radiohead");
    assert_eq!(
        artist.mbid.as_deref(),
        Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
    );
    assert_eq!(artist.thumb.as_deref(), Some("https://thumb.example/a.jpg"));
    assert_eq!(sink.count(), 0, "a hit records nothing");
    let hits = fake.hits();
    assert_eq!(hits.len(), 1);
    let (route, query) = route_and_query(&hits[0].path);
    assert_eq!(route, "/123/artist-mb.php");
    assert_eq!(
        query_value(&query, "i").as_deref(),
        Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
    );
}

#[tokio::test]
async fn audiodb_album_by_mbid_found() {
    let payload = serde_json::json!({
        "album": [{
            "idAlbum": "2115883",
            "strAlbum": "OK Computer",
            "strMusicBrainzID": "baf13839-4a19-3050-9d5e-189405ff86bd",
            "strAlbumThumb": "https://thumb.example/b.jpg",
            "strAlbum3DCase": "https://3d.example/b.jpg"
        }]
    });
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = audiodb_client(&fake, CountingPacer::new(), sink.clone());

    let album = client
        .album_by_mbid("baf13839-4a19-3050-9d5e-189405ff86bd")
        .await
        .into_option()
        .expect("album found");

    assert_eq!(album.id_album, "2115883");
    assert_eq!(album.title, "OK Computer");
    assert_eq!(album.case_3d.as_deref(), Some("https://3d.example/b.jpg"));
    assert_eq!(sink.count(), 0);
    let (route, _) = route_and_query(&fake.hits()[0].path);
    assert_eq!(route, "/123/album-mb.php");
}

#[tokio::test]
async fn audiodb_search_by_name_routes() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &audiodb_artist_payload(),
    )))
    .await;
    let client = audiodb_client(&fake, CountingPacer::new(), RecSink::new());

    let artist = client
        .search_artist("Radiohead")
        .await
        .into_option()
        .expect("artist found");
    assert_eq!(artist.name, "Radiohead");
    let (route, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(route, "/123/search.php");
    assert_eq!(query_value(&query, "s").as_deref(), Some("Radiohead"));
}

#[tokio::test]
async fn audiodb_search_album_routes() {
    let payload = serde_json::json!({"album": [{"idAlbum": "1", "strAlbum": "Kid A"}]});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = audiodb_client(&fake, CountingPacer::new(), RecSink::new());

    let album = client
        .search_album("Radiohead", "Kid A")
        .await
        .into_option()
        .expect("album found");
    assert_eq!(album.title, "Kid A");
    let (route, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(route, "/123/searchalbum.php");
    assert_eq!(query_value(&query, "s").as_deref(), Some("Radiohead"));
    assert_eq!(query_value(&query, "a").as_deref(), Some("Kid A"));
}

#[tokio::test]
async fn audiodb_missing_identity_fails_decode_and_records() {
    // No `idArtist`/`strArtist`: an empty success must never come back.
    let payload = serde_json::json!({"artists": [{"strArtistThumb": "https://x/y.jpg"}]});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = audiodb_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.artist_by_mbid("mbid").await;

    assert!(outcome.into_option().is_none(), "no empty success");
    assert_eq!(sink.count(), 1, "schema failure records");
    assert_eq!(sink.sources(), vec!["audiodb".to_owned()]);
    assert!(
        sink.messages()[0].contains("Schema error"),
        "message names the failure: {}",
        sink.messages()[0]
    );
}

#[tokio::test]
async fn audiodb_empty_envelopes_are_missing() {
    for body in [
        serde_json::json!({"artists": null}),
        serde_json::json!({"artists": []}),
        serde_json::json!({}),
    ] {
        let fake = Fake::start(always(ScriptedResponse::json(200, &body))).await;
        let sink = RecSink::new();
        let client = audiodb_client(&fake, CountingPacer::new(), sink.clone());

        let outcome = client.artist_by_mbid("mbid").await;

        assert!(
            matches!(outcome, audiodb::Outcome::Missing),
            "null, empty, and absent lists mean no match: {body}"
        );
        assert_eq!(sink.count(), 0, "no match records nothing");
    }
}

#[tokio::test]
async fn audiodb_non_list_member_is_missing() {
    let payload = serde_json::json!({"artists": "bogus"});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = audiodb_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.artist_by_mbid("mbid").await;

    assert!(matches!(outcome, audiodb::Outcome::Missing));
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn audiodb_http_404_is_missing() {
    let fake = Fake::start(always(ScriptedResponse::empty(404))).await;
    let sink = RecSink::new();
    let client = audiodb_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.artist_by_mbid("mbid").await;

    assert!(matches!(outcome, audiodb::Outcome::Missing));
    assert_eq!(sink.count(), 0, "404 is an answer, not a fault");
}

#[tokio::test]
async fn audiodb_http_429_hints_sixty_seconds() {
    let fake = Fake::start(always(ScriptedResponse::empty(429))).await;
    let sink = RecSink::new();
    let client = audiodb_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.artist_by_mbid("mbid").await;

    match outcome {
        audiodb::Outcome::Unavailable {
            retry_after_secs, ..
        } => assert_eq!(retry_after_secs, Some(60.0)),
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn audiodb_http_500_and_bad_json_record() {
    for scripted in [
        ScriptedResponse::empty(500),
        ScriptedResponse::text(200, "not json"),
    ] {
        let fake = Fake::start(always(scripted)).await;
        let sink = RecSink::new();
        let client = audiodb_client(&fake, CountingPacer::new(), sink.clone());

        let outcome = client.album_by_mbid("mbid").await;

        assert!(
            matches!(outcome, audiodb::Outcome::Unavailable { .. }),
            "server errors and bad JSON are Unavailable"
        );
        assert_eq!(sink.count(), 1);
    }
}

#[tokio::test]
async fn audiodb_dead_source_records_and_yields_none() {
    let sink = RecSink::new();
    let client = audiodb::AudioDbClient::new(
        http(),
        &dead_url().await,
        CountingPacer::new(),
        sink.clone(),
    );

    let outcome = client.artist_by_mbid("mbid").await;

    assert!(outcome.into_option().is_none());
    assert_eq!(sink.count(), 1, "a dead source records once");
    assert_eq!(sink.sources(), vec!["audiodb".to_owned()]);
}

#[tokio::test]
async fn audiodb_disabled_short_circuits() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &audiodb_artist_payload(),
    )))
    .await;
    let sink = RecSink::new();
    let client = audiodb_client(&fake, CountingPacer::new(), sink.clone()).with_enabled(false);

    let outcome = client.artist_by_mbid("mbid").await;

    assert!(matches!(outcome, audiodb::Outcome::Missing));
    assert_eq!(fake.hit_count(), 0, "disabled never touches the wire");
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn audiodb_blank_lookups_skip_the_wire() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &audiodb_artist_payload(),
    )))
    .await;
    let client = audiodb_client(&fake, CountingPacer::new(), RecSink::new());

    assert!(matches!(
        client.artist_by_mbid("").await,
        audiodb::Outcome::Missing
    ));
    assert!(matches!(
        client.search_album("Radiohead", "").await,
        audiodb::Outcome::Missing
    ));
    assert_eq!(fake.hit_count(), 0);
}

#[tokio::test]
async fn audiodb_settings_key_replaces_free_key() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &audiodb_artist_payload(),
    )))
    .await;
    let client =
        audiodb_client(&fake, CountingPacer::new(), RecSink::new()).with_api_key("premium-key");

    let _ = client.artist_by_mbid("mbid").await;

    let (route, _) = route_and_query(&fake.hits()[0].path);
    assert_eq!(route, "/premium-key/artist-mb.php");
}

#[tokio::test]
async fn audiodb_every_call_paces_once() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &audiodb_artist_payload(),
    )))
    .await;
    let pacer = CountingPacer::new();
    let client = audiodb_client(&fake, pacer.clone(), RecSink::new());

    let _ = client.artist_by_mbid("a").await;
    let _ = client.search_artist("b").await;

    assert_eq!(pacer.count(), 2);
}

#[tokio::test]
async fn audiodb_pacing_constants_match_v2_buckets() {
    assert_eq!(audiodb::FREE_RATE_PER_SEC, 0.5, "free 30/minute");
    assert_eq!(audiodb::FREE_BURST, 2);
    assert_eq!(audiodb::PREMIUM_RATE_PER_SEC, 5.0);
    assert_eq!(audiodb::PREMIUM_BURST, 10);
    assert_eq!(audiodb::RATE_LIMIT_RETRY_SECS, 60.0);
}

// ---------------------------------------------------------------------------
// AcoustID briefs
// ---------------------------------------------------------------------------

fn acoustid_client(
    base_url: &str,
    pacer: CountingPacer,
    sink: RecSink,
) -> acoustid::AcoustIdClient<CountingPacer, RecSink> {
    acoustid::AcoustIdClient::new(http(), base_url, pacer, sink)
}

fn acoustid_match_payload() -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "results": [{
            "score": 0.95,
            "id": "some-acoustid",
            "recordings": [{
                "id": "REC-1",
                "title": "  Weird Fishes ",
                "artists": [{"name": "Radiohead"}, {"name": "  "}, {"nope": 1}],
                "duration": 318,
                "releasegroups": [{"id": "rg1"}, {"id": "rg1"}, {"id": " rg2 "}],
                "futureField": "ignored"
            },
            {"id": "rec-1", "title": "casefold duplicate"}],
            "releasegroups": [{"id": "rg3"}, {"id": "rg1"}],
            "futureResultField": 1
        }],
        "futureTopField": true
    })
}

#[tokio::test]
async fn acoustid_lookup_posts_form_and_finds_match() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &acoustid_match_payload(),
    )))
    .await;
    let sink = RecSink::new();
    let pacer = CountingPacer::new();
    let client = acoustid_client(&fake.base_url, pacer.clone(), sink.clone());

    let outcome = client.lookup("api-key", "fingerprint-body", 212).await;

    let found = outcome.into_option().expect("match found");
    assert_eq!(found.recording_id, "REC-1");
    assert_eq!(found.recording_ids, vec!["REC-1".to_owned()]);
    assert!((found.score - 0.95).abs() < f64::EPSILON);
    assert_eq!(found.title.as_deref(), Some("Weird Fishes"));
    assert_eq!(found.artist.as_deref(), Some("Radiohead"));
    assert_eq!(found.duration_secs, Some(318));
    assert_eq!(found.release_group_ids, vec!["rg1", "rg2", "rg3"]);
    assert_eq!(sink.count(), 0, "a hit records nothing");
    assert_eq!(pacer.count(), 1, "one wire attempt paces once");
    let hits = fake.hits();
    assert_eq!(hits.len(), 1);
    let (route, _) = route_and_query(&hits[0].path);
    assert_eq!(route, "/v2/lookup");
    assert_eq!(hits[0].method, "POST");
    let form = pairs(&hits[0].text_body());
    assert_eq!(query_value(&form, "client").as_deref(), Some("api-key"));
    assert_eq!(
        query_value(&form, "fingerprint").as_deref(),
        Some("fingerprint-body")
    );
    assert_eq!(query_value(&form, "duration").as_deref(), Some("212"));
    assert_eq!(
        query_value(&form, "meta").as_deref(),
        Some("recordings releasegroups")
    );
}

#[tokio::test]
async fn acoustid_empty_and_low_score_results_are_missing() {
    for body in [
        serde_json::json!({"status": "ok", "results": []}),
        serde_json::json!({"status": "ok"}),
        serde_json::json!({"status": "ok", "results": null}),
        serde_json::json!({"status": "ok", "results": [{"score": 0.5}]}),
        // A missing score reads as 0.0 and falls below the threshold (v2).
        serde_json::json!({"status": "ok", "results": [{"recordings": []}]}),
    ] {
        let fake = Fake::start(always(ScriptedResponse::json(200, &body))).await;
        let sink = RecSink::new();
        let client = acoustid_client(&fake.base_url, CountingPacer::new(), sink.clone());

        let outcome = client.lookup("key", "print", 200).await;

        assert!(
            matches!(outcome, acoustid::Outcome::Missing),
            "no confident match means Missing: {body}"
        );
        assert_eq!(sink.count(), 0, "no match records nothing");
    }
}

#[tokio::test]
async fn acoustid_malformed_payloads_record() {
    for body in [
        // No status at all.
        serde_json::json!({}),
        // A non-list `results`.
        serde_json::json!({"status": "ok", "results": {}}),
        // A non-object best result.
        serde_json::json!({"status": "ok", "results": [7]}),
        // A boolean score must not pass as a number.
        serde_json::json!({"status": "ok", "results": [{"score": true}]}),
        // A string score is unusable.
        serde_json::json!({"status": "ok", "results": [{"score": "high"}]}),
        // A non-list `recordings`.
        serde_json::json!({"status": "ok", "results": [{"score": 0.9, "recordings": {}}]}),
    ] {
        let fake = Fake::start(always(ScriptedResponse::json(200, &body))).await;
        let sink = RecSink::new();
        let client = acoustid_client(&fake.base_url, CountingPacer::new(), sink.clone());

        let outcome = client.lookup("key", "print", 200).await;

        match outcome {
            acoustid::Outcome::Unavailable { recorded, .. } => assert!(recorded),
            other => panic!("expected recorded Unavailable, got {other:?} for {body}"),
        }
        assert_eq!(sink.count(), 1, "malformed payload records once");
        assert_eq!(sink.sources(), vec!["acoustid".to_owned()]);
    }
}

#[tokio::test]
async fn acoustid_non_ok_status_records_with_upstream_text() {
    let payload = serde_json::json!({"status": "error", "error": {"code": 1}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = acoustid_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let outcome = client.lookup("key", "print", 200).await;

    match outcome {
        acoustid::Outcome::Unavailable {
            message, recorded, ..
        } => {
            assert!(recorded);
            assert_eq!(message, "error");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn acoustid_confident_match_without_recording_id_is_never_missing() {
    // v2 FAIL: confident audio, nothing to key the row on. It must not read
    // as a negative, so it surfaces Unavailable (wiring routes it to manual
    // review, exactly like v2).
    let payload = serde_json::json!({
        "status": "ok",
        "results": [{"score": 0.99, "recordings": [{"title": "no id"}, 7, {"id": "  "}]}]
    });
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = acoustid_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let outcome = client.lookup("key", "print", 200).await;

    match outcome {
        acoustid::Outcome::Unavailable { recorded, .. } => assert!(!recorded),
        other => panic!("expected unrecorded Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn acoustid_first_valid_recording_wins() {
    let payload = serde_json::json!({
        "status": "ok",
        "results": [{"score": 0.9, "recordings": [
            {"title": "skipped, no id"},
            {"id": "first-valid", "title": "Winner"},
            {"id": "second-valid", "title": "Runner-up"}
        ]}]
    });
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = acoustid_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let found = client
        .lookup("key", "print", 200)
        .await
        .into_option()
        .expect("match found");

    assert_eq!(found.recording_id, "first-valid");
    assert_eq!(found.recording_ids, vec!["first-valid", "second-valid"]);
    assert_eq!(found.title.as_deref(), Some("Winner"));
}

#[tokio::test]
async fn acoustid_duration_lenient_but_strict_about_shape() {
    for (duration, expected) in [
        (serde_json::json!(212), Some(212)),
        (serde_json::json!(212.0), Some(212)),
        (serde_json::json!(212.5), None),
        (serde_json::json!("212"), None),
        (serde_json::json!(true), None),
    ] {
        let payload = serde_json::json!({
            "status": "ok",
            "results": [{"score": 0.9, "recordings": [{"id": "r", "duration": duration}]}]
        });
        let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
        let client = acoustid_client(&fake.base_url, CountingPacer::new(), RecSink::new());

        let found = client
            .lookup("key", "print", 200)
            .await
            .into_option()
            .expect("match found");

        assert_eq!(found.duration_secs, expected, "duration {duration}");
    }
}

#[tokio::test]
async fn acoustid_blank_title_and_non_list_artists_read_absent() {
    let payload = serde_json::json!({
        "status": "ok",
        "results": [{"score": 0.9, "recordings": [
            {"id": "r", "title": "   ", "artists": "bogus"}
        ]}]
    });
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = acoustid_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let found = client
        .lookup("key", "print", 200)
        .await
        .into_option()
        .expect("match found");

    assert_eq!(found.title, None);
    assert_eq!(found.artist, None);
}

#[tokio::test]
async fn acoustid_429_honors_retry_after_then_defaults_to_sixty() {
    let with_header = Fake::start(always(
        ScriptedResponse::empty(429).with_header("retry-after", "30"),
    ))
    .await;
    let client = acoustid_client(&with_header.base_url, CountingPacer::new(), RecSink::new());
    match client.lookup("key", "print", 200).await {
        acoustid::Outcome::Unavailable {
            retry_after_secs, ..
        } => assert_eq!(retry_after_secs, Some(30.0)),
        other => panic!("expected Unavailable, got {other:?}"),
    }

    for scripted in [
        ScriptedResponse::empty(429),
        ScriptedResponse::empty(429).with_header("retry-after", "bogus"),
    ] {
        let fake = Fake::start(always(scripted)).await;
        let sink = RecSink::new();
        let client = acoustid_client(&fake.base_url, CountingPacer::new(), sink.clone());
        match client.lookup("key", "print", 200).await {
            acoustid::Outcome::Unavailable {
                retry_after_secs,
                recorded,
                ..
            } => {
                assert_eq!(retry_after_secs, Some(60.0));
                assert!(recorded);
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert_eq!(sink.count(), 1);
    }
}

#[tokio::test]
async fn acoustid_4xx_rejection_is_unrecorded() {
    let fake = Fake::start(always(ScriptedResponse::empty(400))).await;
    let sink = RecSink::new();
    let client = acoustid_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let outcome = client.lookup("bad-key", "print", 200).await;

    match outcome {
        acoustid::Outcome::Unavailable {
            message,
            recorded,
            retry_after_secs,
        } => {
            assert!(!recorded, "deterministic rejection never records");
            assert_eq!(retry_after_secs, None, "rejection is never retried");
            assert!(message.contains("400"), "message: {message}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn acoustid_5xx_records() {
    let fake = Fake::start(always(ScriptedResponse::empty(500))).await;
    let sink = RecSink::new();
    let client = acoustid_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let outcome = client.lookup("key", "print", 200).await;

    assert!(matches!(
        outcome,
        acoustid::Outcome::Unavailable { recorded: true, .. }
    ));
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn acoustid_dead_source_records_and_yields_none() {
    let sink = RecSink::new();
    let client = acoustid_client(&dead_url().await, CountingPacer::new(), sink.clone());

    let outcome = client.lookup("key", "print", 200).await;

    assert!(outcome.into_option().is_none());
    assert_eq!(sink.count(), 1, "a dead source records once");
    assert_eq!(sink.sources(), vec!["acoustid".to_owned()]);
}

#[tokio::test]
async fn acoustid_empty_key_means_disabled_without_wire() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &acoustid_match_payload(),
    )))
    .await;
    let sink = RecSink::new();
    let client = acoustid_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let outcome = client.lookup("", "print", 200).await;

    assert!(matches!(outcome, acoustid::Outcome::Missing));
    assert_eq!(fake.hit_count(), 0, "no key means no wire call");
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn acoustid_pacing_constants_match_v2() {
    assert_eq!(acoustid::RATE_PER_SEC, 3.0);
    assert_eq!(acoustid::BURST, 3);
    assert_eq!(acoustid::MIN_SCORE, 0.70);
    assert_eq!(acoustid::DEFAULT_RETRY_AFTER_SECS, 60.0);
}

#[test]
fn acoustid_fpcalc_tolerates_short_track_exit() {
    // fpcalc exits non-zero past EOF on sub-120s tracks yet still emits a
    // valid fingerprint; the fingerprint is used and flagged partial.
    let stdout = "DURATION=95\nFINGERPRINT=AQAAAAE\n";
    let parsed =
        acoustid::parse_fpcalc_output(stdout, false, "Error decoding audio frame (End of file)")
            .expect("tolerated exit parses");

    assert_eq!(parsed.fingerprint, "AQAAAAE");
    assert_eq!(parsed.duration_secs, 95);
    assert!(parsed.partial_decode);
    assert!(parsed.stderr.contains("End of file"), "stderr preserved");

    let clean = acoustid::parse_fpcalc_output(stdout, true, "").expect("clean exit parses");
    assert!(!clean.partial_decode);
}

#[test]
fn acoustid_fpcalc_rejects_unusable_output() {
    // Non-zero exit with no fingerprint is a real failure.
    assert!(matches!(
        acoustid::parse_fpcalc_output("DURATION=95\n", false, "boom"),
        Err(acoustid::FpCalcError::MissingFingerprint(_))
    ));
    // A zero duration would read as a silent no-match downstream.
    assert!(matches!(
        acoustid::parse_fpcalc_output("DURATION=0\nFINGERPRINT=x\n", true, ""),
        Err(acoustid::FpCalcError::BadDuration(_))
    ));
    // Fractional durations truncate, like v2's `int(float(...))`.
    let parsed = acoustid::parse_fpcalc_output("DURATION=212.9\nFINGERPRINT=x\n", true, "")
        .expect("fractional duration parses");
    assert_eq!(parsed.duration_secs, 212);
}

#[test]
fn acoustid_split_artist_credit_splits_aggressively() {
    assert_eq!(
        acoustid::split_artist_credit("A feat. B & C"),
        vec!["A".to_owned(), "B".to_owned(), "C".to_owned()]
    );
    assert_eq!(
        acoustid::split_artist_credit("Solo"),
        vec!["Solo".to_owned()]
    );
}

// ---------------------------------------------------------------------------
// Last.fm briefs
// ---------------------------------------------------------------------------

fn lastfm_client(
    fake: &Fake,
    pacer: CountingPacer,
    sink: RecSink,
) -> lastfm::LastFmClient<CountingPacer, RecSink> {
    lastfm::LastFmClient::new(http(), &fake.base_url, pacer, sink)
}

fn lastfm_creds() -> lastfm::LastFmCredentials {
    lastfm::LastFmCredentials {
        api_key: "user-key".to_owned(),
        shared_secret: "user-secret".to_owned(),
        username: None,
        session_key: None,
    }
}

fn lastfm_linked_creds() -> lastfm::LastFmCredentials {
    lastfm::LastFmCredentials {
        api_key: "user-key".to_owned(),
        shared_secret: "user-secret".to_owned(),
        username: Some("listener".to_owned()),
        session_key: Some("session-key".to_owned()),
    }
}

#[tokio::test]
async fn lastfm_request_token_signs_without_session() {
    let payload = serde_json::json!({"token": "approve-me"});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let token = client
        .request_token(&lastfm_creds())
        .await
        .into_option()
        .expect("token found");

    assert_eq!(token.token, "approve-me");
    assert_eq!(sink.count(), 0);
    let hits = fake.hits();
    assert_eq!(hits.len(), 1);
    let (_, query) = route_and_query(&hits[0].path);
    assert_eq!(
        query_value(&query, "method").as_deref(),
        Some("auth.getToken")
    );
    assert_eq!(query_value(&query, "api_key").as_deref(), Some("user-key"));
    assert_eq!(query_value(&query, "format").as_deref(), Some("json"));
    assert_eq!(query_value(&query, "sk"), None, "no session rides yet");
    // The signature recomputes from the captured parameters.
    let captured_sig = query_value(&query, "api_sig").expect("call is signed");
    let unsigned: Vec<(String, String)> = query
        .into_iter()
        .filter(|(key, _)| key != "api_sig")
        .collect();
    assert_eq!(
        lastfm::api_sig(&unsigned, "user-secret"),
        captured_sig,
        "wire signature matches the documented scheme"
    );
}

#[tokio::test]
async fn lastfm_request_token_missing_field_fails_decode() {
    let payload = serde_json::json!({"unexpected": true});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.request_token(&lastfm_creds()).await;

    assert!(outcome.into_option().is_none(), "no empty token");
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn lastfm_exchange_session_reads_nested_and_top_level() {
    for payload in [
        serde_json::json!({"session": {"name": "listener", "key": "sk", "subscriber": 1}}),
        serde_json::json!({"name": "listener", "key": "sk"}),
    ] {
        let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
        let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());

        let session = client
            .exchange_session(&lastfm_creds(), "approved-token")
            .await
            .into_option()
            .expect("session exchanged");

        assert_eq!(session.name, "listener");
        assert_eq!(session.key, "sk");
        let (_, query) = route_and_query(&fake.hits()[0].path);
        assert_eq!(
            query_value(&query, "method").as_deref(),
            Some("auth.getSession")
        );
        assert_eq!(
            query_value(&query, "token").as_deref(),
            Some("approved-token")
        );
        assert!(query_value(&query, "api_sig").is_some(), "signed");
    }
}

#[tokio::test]
async fn lastfm_exchange_session_missing_identity_fails() {
    let payload = serde_json::json!({"session": {"name": "listener"}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.exchange_session(&lastfm_creds(), "token").await;

    assert!(outcome.into_option().is_none(), "no empty session");
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn lastfm_error_29_hints_one_second_backoff() {
    // Last.fm-29: the documented rate-limit code, decoded from the envelope
    // and covered here rather than provoked live (management notes).
    let payload = serde_json::json!({"error": 29, "message": "Rate limit exceeded"});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;

    match outcome {
        lastfm::Outcome::Unavailable {
            retry_after_secs,
            recorded,
            message,
        } => {
            assert_eq!(retry_after_secs, Some(1.0));
            assert!(recorded);
            assert!(message.contains("Rate limit"), "message: {message}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 1);
    assert_eq!(sink.sources(), vec!["lastfm".to_owned()]);
}

#[tokio::test]
async fn lastfm_error_6_is_missing() {
    let payload = serde_json::json!({"error": 6, "message": "Artist not found"});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.artist_info(&lastfm_creds(), "Nobody", None).await;

    assert!(matches!(outcome, lastfm::Outcome::Missing));
    assert_eq!(sink.count(), 0, "unknown entity records nothing");
}

#[tokio::test]
async fn lastfm_credential_errors_are_unrecorded() {
    for (code, fragment) in [
        (4, "Authentication failed"),
        (9, "Session key expired"),
        (10, "Invalid API key"),
        (17, "Authentication required"),
        (26, "suspended"),
    ] {
        let payload = serde_json::json!({"error": code, "message": "upstream says no"});
        let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
        let sink = RecSink::new();
        let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

        let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;

        match outcome {
            lastfm::Outcome::Unavailable {
                recorded, message, ..
            } => {
                assert!(!recorded, "credential failures never record");
                assert!(message.contains(fragment), "message: {message}");
            }
            other => panic!("expected Unavailable for {code}, got {other:?}"),
        }
        assert_eq!(sink.count(), 0);
    }
}

#[tokio::test]
async fn lastfm_error_14_maps_to_token_not_authorized() {
    let payload = serde_json::json!({"error": 14, "message": "Token not authorized"});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.exchange_session(&lastfm_creds(), "token").await;

    match outcome {
        lastfm::Outcome::Unavailable {
            recorded, message, ..
        } => {
            assert!(!recorded);
            assert!(
                message.contains("Token not yet authorized"),
                "message: {message}"
            );
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    // Wiring adapts this outcome onto the stage-3 `TokenNotAuthorized` error.
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn lastfm_service_errors_record() {
    for payload in [
        serde_json::json!({"error": 11, "message": "offlining"}),
        serde_json::json!({"error": 2, "message": "no such service"}),
        serde_json::json!({"error": 99, "message": "unknown code"}),
    ] {
        let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
        let sink = RecSink::new();
        let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

        let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;

        assert!(
            matches!(outcome, lastfm::Outcome::Unavailable { recorded: true, .. }),
            "service failures record: {payload}"
        );
        assert_eq!(sink.count(), 1);
    }
}

#[tokio::test]
async fn lastfm_non_200_never_decodes_the_envelope() {
    // The live invalid-key probe saw HTTP 403 with error 10; v2 raises on
    // the status before parsing, so the envelope is never decoded here.
    // Credential rejections stay unrecorded, like the error-10 envelope
    // inside a 200.
    let payload = serde_json::json!({"error": 10, "message": "Invalid API key"});
    let fake = Fake::start(always(ScriptedResponse::json(403, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;

    match outcome {
        lastfm::Outcome::Unavailable {
            recorded, message, ..
        } => {
            assert!(!recorded);
            assert!(message.contains("403"), "message: {message}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn lastfm_http_statuses_map_without_the_envelope() {
    // 401 reads like 403: a credential rejection, never recorded.
    let fake = Fake::start(always(ScriptedResponse::empty(401))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());
    let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;
    assert!(
        matches!(
            outcome,
            lastfm::Outcome::Unavailable {
                recorded: false,
                ..
            }
        ),
        "got {outcome:?}"
    );
    assert_eq!(sink.count(), 0);

    // 404 is authoritative absence, never recorded.
    let fake = Fake::start(always(ScriptedResponse::empty(404))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());
    let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;
    assert!(matches!(outcome, lastfm::Outcome::Missing));
    assert_eq!(sink.count(), 0);

    // HTTP 429 (distinct from the error-29 envelope) records with the
    // honored Retry-After.
    let scripted = ScriptedResponse::empty(429).with_header("retry-after", "12");
    let fake = Fake::start(always(scripted)).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());
    let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;
    match outcome {
        lastfm::Outcome::Unavailable {
            retry_after_secs,
            recorded,
            ..
        } => {
            assert!(recorded);
            assert_eq!(retry_after_secs, Some(12.0));
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 1);

    // Anything else unusual still records.
    let fake = Fake::start(always(ScriptedResponse::empty(500))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());
    let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;
    assert!(
        matches!(outcome, lastfm::Outcome::Unavailable { recorded: true, .. }),
        "got {outcome:?}"
    );
    assert_eq!(sink.count(), 1);
}

#[test]
fn lastfm_api_sig_sorts_and_skips_format_and_callback() {
    let params = vec![
        ("method".to_owned(), "auth.getToken".to_owned()),
        ("api_key".to_owned(), "key".to_owned()),
        ("format".to_owned(), "json".to_owned()),
        ("callback".to_owned(), "cb".to_owned()),
    ];
    // Sorted, without format/callback, plus the secret, MD5 hexed.
    let expected = format!("{:x}", md5::compute(b"api_keykeymethodauth.getTokensecret"));
    assert_eq!(lastfm::api_sig(&params, "secret"), expected);
}

#[tokio::test]
async fn lastfm_artist_info_found_with_lenient_counts() {
    let payload = serde_json::json!({
        "artist": {
            "name": "Radiohead",
            "mbid": "",
            "url": "https://last.fm/a",
            "stats": {"listeners": "1234567", "playcount": 890},
            "bio": {"summary": "Oxford band"},
            "tags": {"tag": [{"name": "rock", "url": "https://t"}]},
            "similar": {"artist": [
                {"name": "Thom Yorke", "mbid": "", "match": "0.92", "url": "https://s"},
                {"name": "Atoms", "match": 0.5}
            ]},
            "futureField": "ignored"
        }
    });
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let info = client
        .artist_info(&lastfm_creds(), "Radiohead", None)
        .await
        .into_option()
        .expect("artist info found");

    assert_eq!(info.name, "Radiohead");
    assert_eq!(info.mbid, None, "blank mbid reads absent");
    assert_eq!(info.listeners, 1234567, "string counts parse");
    assert_eq!(info.playcount, 890);
    assert_eq!(info.bio_summary, "Oxford band");
    assert_eq!(info.tags.len(), 1);
    assert_eq!(info.similar.len(), 2);
    assert!((info.similar[0].score - 0.92).abs() < f64::EPSILON);
    assert_eq!(sink.count(), 0);
    let (_, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(
        query_value(&query, "method").as_deref(),
        Some("artist.getInfo")
    );
    assert_eq!(query_value(&query, "artist").as_deref(), Some("Radiohead"));
    assert_eq!(
        query_value(&query, "api_sig"),
        None,
        "info reads are unsigned"
    );
}

#[tokio::test]
async fn lastfm_artist_info_prefers_mbid_lookup() {
    let payload = serde_json::json!({"artist": {"name": "Radiohead"}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());

    let info = client
        .artist_info(&lastfm_creds(), "ignored", Some("artist-mbid"))
        .await
        .into_option()
        .expect("artist info found");

    assert_eq!(info.name, "Radiohead");
    let (_, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(query_value(&query, "mbid").as_deref(), Some("artist-mbid"));
    assert_eq!(query_value(&query, "artist"), None);
}

#[tokio::test]
async fn lastfm_artist_info_missing_name_fails() {
    let payload = serde_json::json!({"artist": {"mbid": "x"}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;

    assert!(outcome.into_option().is_none(), "no nameless success");
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn lastfm_album_info_picks_extralarge_then_last_image() {
    let with_xl = serde_json::json!({"album": {
        "name": "OK Computer", "artist": "Radiohead",
        "image": [
            {"size": "small", "#text": "https://small"},
            {"size": "extralarge", "#text": "https://xl"},
            {"size": "mega", "#text": "https://mega"}
        ],
        "tracks": {"track": [
            {"name": "Airbag", "duration": "240", "url": "https://t", "@attr": {"rank": "1"}}
        ]},
        "wiki": {"summary": "1997"}
    }});
    let fake = Fake::start(always(ScriptedResponse::json(200, &with_xl))).await;
    let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());
    let info = client
        .album_info(&lastfm_creds(), "Radiohead", "OK Computer", None)
        .await
        .into_option()
        .expect("album info found");
    assert_eq!(info.image_url, "https://xl", "extralarge wins");
    assert_eq!(info.tracks.len(), 1);
    assert_eq!(info.tracks[0].duration_secs, 240);
    assert_eq!(info.tracks[0].rank, 1);
    assert_eq!(info.summary, "1997");

    let without_xl = serde_json::json!({"album": {
        "name": "Kid A", "artist": "Radiohead",
        "image": [
            {"size": "small", "#text": "https://small"},
            {"size": "large", "#text": "https://large"}
        ]
    }});
    let fake = Fake::start(always(ScriptedResponse::json(200, &without_xl))).await;
    let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());
    let info = client
        .album_info(&lastfm_creds(), "Radiohead", "Kid A", None)
        .await
        .into_option()
        .expect("album info found");
    assert_eq!(
        info.image_url, "https://large",
        "otherwise the last image wins"
    );
    assert!(info.tracks.is_empty());
}

#[tokio::test]
async fn lastfm_similar_artists_found() {
    let payload = serde_json::json!({"similarartists": {"artist": [
        {"name": "Thom Yorke", "mbid": "m", "match": 1.0, "url": "https://u"}
    ]}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());

    let similar = client
        .similar_artists(&lastfm_creds(), "Radiohead", None, 5)
        .await
        .into_option()
        .expect("similar artists found");

    assert_eq!(similar.len(), 1);
    assert_eq!(similar[0].name, "Thom Yorke");
    assert_eq!(similar[0].mbid.as_deref(), Some("m"));
    let (_, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(
        query_value(&query, "method").as_deref(),
        Some("artist.getSimilar")
    );
    assert_eq!(query_value(&query, "limit").as_deref(), Some("5"));
}

#[tokio::test]
async fn lastfm_artist_top_genres_send_autocorrect_zero() {
    let payload = serde_json::json!({"toptags": {
        "tag": [
            {"name": "rock", "count": 90, "url": "https://t"},
            {"name": "   ", "count": 50, "url": "https://t"},
            {"name": "alternative", "count": 70, "url": "https://t"}
        ],
        "@attr": {"artist": "Radiohead"}
    }});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let tags = client
        .artist_top_genres(&lastfm_creds(), "Radiohead")
        .await
        .into_option()
        .expect("genres found");

    assert_eq!(tags.len(), 2, "blank names are filtered");
    assert_eq!(tags[0].name, "rock");
    assert_eq!(tags[0].weight, 90);
    assert_eq!(tags[1].name, "alternative");
    assert_eq!(sink.count(), 0);
    let (_, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(
        query_value(&query, "method").as_deref(),
        Some("artist.getTopTags")
    );
    assert_eq!(query_value(&query, "autocorrect").as_deref(), Some("0"));
}

#[tokio::test]
async fn lastfm_album_top_genres_found() {
    let payload = serde_json::json!({"toptags": {
        "tag": [{"name": "art rock", "count": 42, "url": "https://t"}]
    }});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());

    let tags = client
        .album_top_genres(&lastfm_creds(), "Radiohead", "OK Computer")
        .await
        .into_option()
        .expect("genres found");

    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "art rock");
    let (_, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(
        query_value(&query, "method").as_deref(),
        Some("album.getTopTags")
    );
    assert_eq!(query_value(&query, "autocorrect").as_deref(), Some("0"));
}

#[tokio::test]
async fn lastfm_genre_unknown_entity_is_empty_found() {
    let payload = serde_json::json!({"error": 6, "message": "not found"});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());

    let tags = client
        .artist_top_genres(&lastfm_creds(), "Nobody")
        .await
        .into_option()
        .expect("unknown artist yields empty tags, not failure");

    assert!(tags.is_empty());
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn lastfm_missing_key_never_touches_the_wire() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &serde_json::json!({"artist": {"name": "x"}}),
    )))
    .await;
    let sink = RecSink::new();
    let client = lastfm_client(&fake, CountingPacer::new(), sink.clone());
    let mut creds = lastfm_creds();
    creds.api_key.clear();

    let outcome = client.artist_info(&creds, "Radiohead", None).await;

    match outcome {
        lastfm::Outcome::Unavailable { recorded, .. } => assert!(!recorded),
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(fake.hit_count(), 0);
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn lastfm_signed_call_without_secret_fails_locally() {
    let fake = Fake::start(always(ScriptedResponse::json(
        200,
        &serde_json::json!({"token": "x"}),
    )))
    .await;
    let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());
    let mut creds = lastfm_creds();
    creds.shared_secret.clear();

    let outcome = client.request_token(&creds).await;

    assert!(outcome.into_option().is_none());
    assert_eq!(fake.hit_count(), 0);
}

#[tokio::test]
async fn lastfm_linked_session_rides_as_sk() {
    let payload = serde_json::json!({"token": "x"});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());

    let _ = client.request_token(&lastfm_linked_creds()).await;

    let (_, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(query_value(&query, "sk").as_deref(), Some("session-key"));
}

#[tokio::test]
async fn lastfm_per_user_credentials_stay_isolated() {
    // R7: one client, two users; each call carries its own key and the
    // client stores neither.
    let payload = serde_json::json!({"artist": {"name": "x"}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = lastfm_client(&fake, CountingPacer::new(), RecSink::new());
    let mut other = lastfm_creds();
    other.api_key = "other-key".to_owned();

    let _ = client.artist_info(&lastfm_creds(), "a", None).await;
    let _ = client.artist_info(&other, "a", None).await;

    let hits = fake.hits();
    assert_eq!(hits.len(), 2);
    let (_, first) = route_and_query(&hits[0].path);
    let (_, second) = route_and_query(&hits[1].path);
    assert_eq!(query_value(&first, "api_key").as_deref(), Some("user-key"));
    assert_eq!(
        query_value(&second, "api_key").as_deref(),
        Some("other-key")
    );
}

#[tokio::test]
async fn lastfm_dead_source_records_and_yields_none() {
    let sink = RecSink::new();
    let client = lastfm::LastFmClient::new(
        http(),
        &dead_url().await,
        CountingPacer::new(),
        sink.clone(),
    );

    let outcome = client.artist_info(&lastfm_creds(), "Radiohead", None).await;

    assert!(outcome.into_option().is_none());
    assert_eq!(sink.count(), 1, "a dead source records once");
    assert_eq!(sink.sources(), vec!["lastfm".to_owned()]);
}

#[tokio::test]
async fn lastfm_every_call_paces_once() {
    let payload = serde_json::json!({"artist": {"name": "x"}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let pacer = CountingPacer::new();
    let client = lastfm_client(&fake, pacer.clone(), RecSink::new());

    let _ = client.artist_info(&lastfm_creds(), "a", None).await;
    let _ = client.artist_info(&lastfm_creds(), "b", None).await;

    assert_eq!(pacer.count(), 2, "each call takes one community token");
}

#[tokio::test]
async fn lastfm_pacing_constants_match_v2_community_bucket() {
    assert_eq!(lastfm::RATE_PER_SEC, 5.0, "community 5/second");
    assert_eq!(lastfm::BURST, 10);
    assert_eq!(lastfm::ERROR_RATE_LIMITED, 29);
    assert_eq!(lastfm::RATE_LIMIT_RETRY_SECS, 1.0);
}

// ---------------------------------------------------------------------------
// ListenBrainz briefs
// ---------------------------------------------------------------------------

fn listenbrainz_client(
    base_url: &str,
    pacer: CountingPacer,
    sink: RecSink,
) -> listenbrainz::ListenBrainzClient<CountingPacer, RecSink> {
    listenbrainz::ListenBrainzClient::new(http(), base_url, pacer, sink)
}

fn lb_anonymous() -> listenbrainz::ListenBrainzCredentials {
    listenbrainz::ListenBrainzCredentials {
        username: Some("listener".to_owned()),
        user_token: None,
    }
}

fn lb_authed() -> listenbrainz::ListenBrainzCredentials {
    listenbrainz::ListenBrainzCredentials {
        username: Some("listener".to_owned()),
        user_token: Some("lb-token".to_owned()),
    }
}

#[tokio::test]
async fn listenbrainz_required_headers_sent() {
    let payload = serde_json::json!({"payload": {"count": 12}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let validation = client
        .validate_username("listener", &lb_authed())
        .await
        .into_option()
        .expect("validation answered");
    assert!(validation.valid);

    let hits = fake.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].header("accept"), Some("application/json"));
    assert_eq!(hits[0].header("content-type"), Some("application/json"));
    assert_eq!(
        hits[0].header("authorization"),
        Some("Token lb-token"),
        "Token scheme, exactly like v2"
    );
}

#[tokio::test]
async fn listenbrainz_anonymous_read_sends_no_authorization() {
    let payload = serde_json::json!({"payload": {"count": 12}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let validation = client
        .validate_username("listener", &lb_anonymous())
        .await
        .into_option()
        .expect("validation answered");
    assert!(validation.valid);

    let hits = fake.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].header("authorization"), None);
    assert_eq!(hits[0].header("accept"), Some("application/json"));
}

#[tokio::test]
async fn listenbrainz_unsafe_tokens_rejected_without_wire() {
    for token in [
        "has\nnewline".to_owned(),
        "has space".to_owned(),
        "x".repeat(1025),
    ] {
        let fake = Fake::start(always(ScriptedResponse::json(
            200,
            &serde_json::json!({"payload": {"count": 1}}),
        )))
        .await;
        let sink = RecSink::new();
        let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());
        let creds = listenbrainz::ListenBrainzCredentials {
            username: Some("listener".to_owned()),
            user_token: Some(token),
        };

        let outcome = client.validate_username("listener", &creds).await;

        match outcome {
            listenbrainz::Outcome::Unavailable { recorded, .. } => assert!(!recorded),
            other => panic!("expected unrecorded Unavailable, got {other:?}"),
        }
        assert_eq!(fake.hit_count(), 0, "rejected tokens never reach the wire");
        assert_eq!(sink.count(), 0);
    }
}

#[tokio::test]
async fn listenbrainz_validate_username_routes_and_404_is_invalid() {
    let payload = serde_json::json!({"payload": {"count": 1200}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let validation = client
        .validate_username("listener", &lb_anonymous())
        .await
        .into_option()
        .expect("validation answered");
    assert!(validation.valid);
    assert!(
        validation.detail.contains("1200"),
        "detail: {}",
        validation.detail
    );
    let (route, _) = route_and_query(&fake.hits()[0].path);
    assert_eq!(route, "/1/user/listener/listen-count");

    let missing = Fake::start(always(ScriptedResponse::empty(404))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&missing.base_url, CountingPacer::new(), sink.clone());
    let validation = client
        .validate_username("nobody", &lb_anonymous())
        .await
        .into_option()
        .expect("404 is an ordinary invalid answer");
    assert!(!validation.valid);
    assert_eq!(sink.count(), 0, "accepted statuses stay neutral");
}

#[tokio::test]
async fn listenbrainz_validate_token_accepts_401_as_invalid() {
    let payload = serde_json::json!({"valid": true, "user_name": "listener"});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let validation = client
        .validate_token(&lb_authed())
        .await
        .into_option()
        .expect("validation answered");
    assert!(validation.valid);
    assert!(validation.detail.contains("listener"));

    let rejected = Fake::start(always(ScriptedResponse::empty(401))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&rejected.base_url, CountingPacer::new(), sink.clone());
    let validation = client
        .validate_token(&lb_authed())
        .await
        .into_option()
        .expect("401 is an ordinary invalid answer");
    assert!(!validation.valid);
    assert_eq!(sink.count(), 0);

    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());
    let validation = client
        .validate_token(&lb_anonymous())
        .await
        .into_option()
        .expect("missing token answers without wire");
    assert!(!validation.valid);
    assert_eq!(
        fake.hit_count(),
        1,
        "no extra wire call for the missing token"
    );
}

#[tokio::test]
async fn listenbrainz_user_listens_found_with_mbid_mapping() {
    let payload = serde_json::json!({"payload": {"listens": [
        {
            "listened_at": 1700000000,
            "track_metadata": {
                "track_name": "Weird Fishes",
                "artist_name": "Radiohead",
                "release_name": "In Rainbows",
                "additional_info": {"recording_mbid": "add-mbid", "release_mbid": "rel-add"},
                "mbid_mapping": {
                    "recording_mbid": "map-mbid",
                    "release_mbid": "rel-map",
                    "artist_mbids": ["a1"]
                },
                "futureField": "ignored"
            }
        },
        // Nameless items are skipped: names are identity, never placeholders.
        {"listened_at": 1, "track_metadata": {"artist_name": "No Track"}},
        {"listened_at": 2, "track_metadata": {"track_name": "No Artist"}}
    ]}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let listens = client
        .user_listens("listener", 25, &lb_anonymous())
        .await
        .into_option()
        .expect("listens found");

    assert_eq!(listens.len(), 1);
    assert_eq!(listens[0].track_name, "Weird Fishes");
    assert_eq!(listens[0].listened_at, 1700000000);
    assert_eq!(listens[0].recording_mbid.as_deref(), Some("map-mbid"));
    assert_eq!(listens[0].release_mbid.as_deref(), Some("rel-map"));
    assert_eq!(
        listens[0].artist_mbids,
        Some(vec!["a1".to_owned()]),
        "mapping wins over additional info"
    );
    assert_eq!(sink.count(), 0);
    let (route, _) = route_and_query(&fake.hits()[0].path);
    assert_eq!(route, "/1/user/listener/listens");
}

#[tokio::test]
async fn listenbrainz_user_listens_clamp_and_empty_user() {
    let payload = serde_json::json!({"payload": {"listens": []}});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let listens = client
        .user_listens("listener", 150, &lb_anonymous())
        .await
        .into_option()
        .expect("listens found");
    assert!(listens.is_empty());
    let (_, query) = route_and_query(&fake.hits()[0].path);
    assert_eq!(query_value(&query, "count").as_deref(), Some("100"));

    let empty = client
        .user_listens("", 25, &lb_anonymous())
        .await
        .into_option()
        .expect("empty user answers empty");
    assert!(empty.is_empty());
    assert_eq!(fake.hit_count(), 1, "empty user skips the wire");
}

#[tokio::test]
async fn listenbrainz_recording_metadata_posts_and_resolves() {
    let payload = serde_json::json!({
        "rec-a": {"release": {"release_group_mbid": "rg-a"}},
        "rec-b": {"release": {}},
        "future": "ignored"
    });
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let resolved = client
        .recording_release_groups(
            &[
                "rec-a".to_owned(),
                "rec-b".to_owned(),
                "rec-a".to_owned(),
                String::new(),
            ],
            &lb_anonymous(),
        )
        .await
        .into_option()
        .expect("metadata resolved");

    assert_eq!(resolved.len(), 1, "unknown ids are left out, not errors");
    assert_eq!(resolved.get("rec-a").map(String::as_str), Some("rg-a"));
    let hits = fake.hits();
    assert_eq!(hits.len(), 1, "input dedupes into one batch");
    assert_eq!(hits[0].method, "POST");
    let (route, _) = route_and_query(&hits[0].path);
    assert_eq!(route, "/1/metadata/recording/");
    let body: serde_json::Value = serde_json::from_slice(&hits[0].body).expect("JSON body");
    assert_eq!(
        body.get("inc").and_then(|inc| inc.as_str()),
        Some("release")
    );
}

#[tokio::test]
async fn listenbrainz_recording_metadata_batches_at_fifty() {
    let payload = serde_json::json!({});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let pacer = CountingPacer::new();
    let client = listenbrainz_client(&fake.base_url, pacer.clone(), RecSink::new());
    let ids: Vec<String> = (0..51).map(|n| format!("rec-{n:03}")).collect();

    let resolved = client
        .recording_release_groups(&ids, &lb_anonymous())
        .await
        .into_option()
        .expect("metadata resolved");

    assert!(resolved.is_empty());
    assert_eq!(fake.hit_count(), 2, "51 ids batch at 50");
    assert_eq!(pacer.count(), 2, "each batch paces");
}

#[tokio::test]
async fn listenbrainz_recording_metadata_non_object_records_and_skips() {
    let payload = serde_json::json!([1, 2, 3]);
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let resolved = client
        .recording_release_groups(&["rec-a".to_owned()], &lb_anonymous())
        .await
        .into_option()
        .expect("partial results survive a bad batch");

    assert!(resolved.is_empty());
    assert_eq!(sink.count(), 1);
}

#[tokio::test]
async fn listenbrainz_release_group_genres_get_only_with_inc() {
    let payload = serde_json::json!({
        "rg-known": {
            "artist": {"name": "Radiohead"},
            "release_group": {"name": "OK Computer"},
            "release": {"name": "same summary shape, not a list"},
            "tag": {
                "artist": [{"tag": "rock", "count": 5}],
                "release_group": [{"tag": "alternative", "count": 8, "genre_mbid": "g1"}]
            },
            "futureField": "ignored"
        }
    });
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let resolved = client
        .release_group_genres(
            &["rg-known".to_owned(), "rg-unknown".to_owned()],
            &lb_anonymous(),
        )
        .await
        .expect("batch within ceiling")
        .into_option()
        .expect("genres found");

    let known = resolved.get("rg-known").expect("known id resolved");
    assert_eq!(known.artist_tags.len(), 1);
    assert_eq!(known.artist_tags[0].tag, "rock");
    assert_eq!(
        known.artist_tags[0].genre_mbid, None,
        "folksonomy omits the mbid"
    );
    assert_eq!(known.release_group_tags.len(), 1);
    assert_eq!(
        known.release_group_tags[0].genre_mbid.as_deref(),
        Some("g1")
    );
    let unknown = resolved.get("rg-unknown").expect("unknown id present");
    assert!(unknown.artist_tags.is_empty() && unknown.release_group_tags.is_empty());
    assert_eq!(sink.count(), 0);
    let hits = fake.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].method, "GET",
        "release groups are GET-only (live notes)"
    );
    let (route, query) = route_and_query(&hits[0].path);
    assert_eq!(route, "/1/metadata/release_group/");
    assert_eq!(
        query_value(&query, "inc").as_deref(),
        Some("artist tag release")
    );
    let ids = query_value(&query, "release_group_mbids").expect("batched ids");
    assert!(
        ids.contains("rg-known") && ids.contains("rg-unknown"),
        "ids: {ids}"
    );
}

#[tokio::test]
async fn listenbrainz_release_group_genres_batch_ceiling() {
    let payload = serde_json::json!({});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let pacer = CountingPacer::new();
    let client = listenbrainz_client(&fake.base_url, pacer.clone(), RecSink::new());
    let ids: Vec<String> = (0..26).map(|n| format!("rg-{n:03}")).collect();

    let resolved = client
        .release_group_genres(&ids, &lb_anonymous())
        .await
        .expect("batch within ceiling")
        .into_option()
        .expect("genres found");

    assert_eq!(resolved.len(), 26, "every id resolves, even to empty");
    assert_eq!(fake.hit_count(), 2, "26 ids batch at 25");
    assert_eq!(pacer.count(), 2);
}

#[tokio::test]
async fn listenbrainz_release_group_genres_reject_over_five_hundred() {
    let fake = Fake::start(always(ScriptedResponse::json(200, &serde_json::json!({})))).await;
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());
    let ids: Vec<String> = (0..501).map(|n| format!("rg-{n}")).collect();

    let outcome = client.release_group_genres(&ids, &lb_anonymous()).await;

    assert!(outcome.is_err(), "501 ids exceed the local ceiling");
    assert_eq!(fake.hit_count(), 0, "rejected before the wire");
}

#[tokio::test]
async fn listenbrainz_genres_204_is_unavailable_unrecorded() {
    let fake = Fake::start(always(ScriptedResponse::empty(204))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let outcome = client
        .release_group_genres(&["rg".to_owned()], &lb_anonymous())
        .await
        .expect("batch within ceiling");

    match outcome {
        listenbrainz::Outcome::Unavailable { recorded, .. } => assert!(!recorded),
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn listenbrainz_genres_invalid_json_records_once_at_classifier() {
    let fake = Fake::start(always(ScriptedResponse::text(200, "nope"))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let outcome = client
        .release_group_genres(&["rg".to_owned()], &lb_anonymous())
        .await
        .expect("batch within ceiling");

    match outcome {
        listenbrainz::Outcome::Unavailable { recorded, .. } => assert!(recorded),
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 1, "invalid JSON records once, like v2");
    assert!(
        sink.messages()[0].contains("invalid JSON"),
        "message: {}",
        sink.messages()[0]
    );
}

#[tokio::test]
async fn listenbrainz_popularity_keeps_zeros_and_skips_malformed_items() {
    let payload = serde_json::json!([
        {"release_group_mbid": "a", "total_listen_count": 10},
        {"release_group_mbid": "b", "total_listen_count": 0},
        {"release_group_mbid": "", "total_listen_count": 5},
        {"release_group_mbid": "c"},
        {"release_group_mbid": "d", "total_listen_count": "lots"}
    ]);
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), RecSink::new());

    let counts = client
        .release_group_popularity(&["a".to_owned(), "b".to_owned()], &lb_anonymous())
        .await
        .into_option()
        .expect("popularity found");

    assert_eq!(counts.get("a"), Some(&10));
    assert_eq!(counts.get("b"), Some(&0), "zero is a count, not a miss");
    assert_eq!(counts.len(), 2);
    let (route, _) = route_and_query(&fake.hits()[0].path);
    assert_eq!(route, "/1/popularity/release-group");
}

#[tokio::test]
async fn listenbrainz_popularity_malformed_never_reads_as_zeros() {
    // v2's poisoning guard: a malformed answer returns partial results with
    // nothing written, never an authoritative empty map from an outage.
    let payload = serde_json::json!({"oops": true});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let counts = client
        .release_group_popularity(&["a".to_owned()], &lb_anonymous())
        .await
        .into_option()
        .expect("malformed popularity stays soft");

    assert!(counts.is_empty());
    assert_eq!(
        sink.count(),
        0,
        "popularity swallows malformed replies silently"
    );

    let empty = Fake::start(always(ScriptedResponse::json(200, &serde_json::json!([])))).await;
    let client = listenbrainz_client(&empty.base_url, CountingPacer::new(), RecSink::new());
    let counts = client
        .release_group_popularity(&["a".to_owned()], &lb_anonymous())
        .await
        .into_option()
        .expect("empty list is legitimate");
    assert!(counts.is_empty());
}

#[tokio::test]
async fn listenbrainz_artist_top_release_groups_parses_and_caps() {
    let payload = serde_json::json!([
        {"release_group_mbid": "rg-a", "total_listen_count": 10,
         "release_group": {"name": "Alpha"}},
        {"release_group_mbid": "rg-b", "total_listen_count": 0},
        {"release_group_mbid": "", "total_listen_count": 99},
        {"release_group_mbid": "rg-c"},
        {"release_group_mbid": "rg-d", "total_listen_count": "lots"},
        {"release_group_mbid": "rg-e", "total_listen_count": 3,
         "release_group": {"name": "Echo"}}
    ]);
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let pacer = CountingPacer::new();
    let client = listenbrainz_client(&fake.base_url, pacer.clone(), RecSink::new());

    let top = client
        .artist_top_release_groups("artist-mbid", 5, &lb_anonymous())
        .await
        .into_option()
        .expect("top release groups found");

    assert_eq!(
        top,
        vec![
            listenbrainz::TopReleaseGroup {
                release_group_mbid: "rg-a".to_owned(),
                name: "Alpha".to_owned(),
                listen_count: 10,
            },
            listenbrainz::TopReleaseGroup {
                release_group_mbid: "rg-b".to_owned(),
                name: String::new(),
                listen_count: 0,
            },
        ],
        "zero kept, name absence tolerated, blank mbid and malformed counts \
         skipped, the sixth row past the cap ignored"
    );
    assert_eq!(pacer.count(), 1, "one paced wire call");
    let (route, _) = route_and_query(&fake.hits()[0].path);
    assert_eq!(
        route,
        "/1/popularity/top-release-groups-for-artist/artist-mbid"
    );
    assert_eq!(
        fake.hits()[0].header("authorization"),
        None,
        "popularity reads stay anonymous"
    );
}

#[tokio::test]
async fn listenbrainz_artist_top_release_groups_degrades_soft() {
    let payload = serde_json::json!({"oops": true});
    let fake = Fake::start(always(ScriptedResponse::json(200, &payload))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let top = client
        .artist_top_release_groups("artist-mbid", 5, &lb_anonymous())
        .await
        .into_option()
        .expect("malformed top list stays soft");
    assert!(top.is_empty());
    assert_eq!(sink.count(), 0, "malformed replies swallow silently");

    let pacer = CountingPacer::new();
    let client = listenbrainz_client(&fake.base_url, pacer.clone(), RecSink::new());
    let blank = client
        .artist_top_release_groups("  ", 5, &lb_anonymous())
        .await
        .into_option()
        .expect("blank mbid stays soft");
    assert!(blank.is_empty());
    let zero = client
        .artist_top_release_groups("artist-mbid", 0, &lb_anonymous())
        .await
        .into_option()
        .expect("zero count stays soft");
    assert!(zero.is_empty());
    assert_eq!(fake.hit_count(), 1, "degenerate calls never touch the wire");
}

#[tokio::test]
async fn listenbrainz_429_prefers_explicit_delay_then_defaults() {
    let reset_in = Fake::start(always(
        ScriptedResponse::empty(429).with_header("x-ratelimit-reset-in", "12"),
    ))
    .await;
    let client = listenbrainz_client(&reset_in.base_url, CountingPacer::new(), RecSink::new());
    match client.validate_token(&lb_authed()).await {
        listenbrainz::Outcome::Unavailable {
            retry_after_secs, ..
        } => assert_eq!(retry_after_secs, Some(12.0)),
        other => panic!("expected Unavailable, got {other:?}"),
    }

    let retry_after = Fake::start(always(
        ScriptedResponse::empty(429).with_header("retry-after", "5"),
    ))
    .await;
    let client = listenbrainz_client(&retry_after.base_url, CountingPacer::new(), RecSink::new());
    match client.validate_token(&lb_authed()).await {
        listenbrainz::Outcome::Unavailable {
            retry_after_secs, ..
        } => assert_eq!(retry_after_secs, Some(5.0)),
        other => panic!("expected Unavailable, got {other:?}"),
    }

    for scripted in [
        ScriptedResponse::empty(429),
        ScriptedResponse::empty(429).with_header("retry-after", "bogus"),
        ScriptedResponse::empty(429).with_header("x-ratelimit-reset-in", "0"),
    ] {
        let fake = Fake::start(always(scripted)).await;
        let sink = RecSink::new();
        let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());
        match client.validate_token(&lb_authed()).await {
            listenbrainz::Outcome::Unavailable {
                retry_after_secs,
                recorded,
                ..
            } => {
                assert_eq!(retry_after_secs, Some(2.0));
                assert!(recorded);
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert_eq!(sink.count(), 1);
    }
}

#[tokio::test]
async fn listenbrainz_policy_blocks_fail_fast_as_unavailable() {
    for (status, body) in [
        (500, "Popularity API currently disabled due to high load"),
        (401, "Slow down! Please provide an Auth token"),
        (401, "blocked for AI scrapers, provide a token"),
    ] {
        let fake = Fake::start(always(ScriptedResponse::text(status, body))).await;
        let sink = RecSink::new();
        let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

        let outcome = client
            .release_group_popularity(&["a".to_owned()], &lb_anonymous())
            .await;

        match outcome {
            listenbrainz::Outcome::Unavailable {
                recorded, message, ..
            } => {
                assert!(recorded);
                assert!(
                    message.contains("unavailable upstream"),
                    "message: {message}"
                );
            }
            other => panic!("expected Unavailable for {status}, got {other:?}"),
        }
        assert_eq!(sink.count(), 1);
    }
}

#[tokio::test]
async fn listenbrainz_plain_401_is_credential_rejection() {
    let fake = Fake::start(always(ScriptedResponse::text(401, "bad token"))).await;
    let sink = RecSink::new();
    let client = listenbrainz_client(&fake.base_url, CountingPacer::new(), sink.clone());

    let outcome = client.user_listens("listener", 10, &lb_authed()).await;

    match outcome {
        listenbrainz::Outcome::Unavailable {
            recorded, message, ..
        } => {
            assert!(!recorded, "credential rejection never records");
            assert!(
                message.contains("credentials rejected"),
                "message: {message}"
            );
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(sink.count(), 0);
}

#[tokio::test]
async fn listenbrainz_dead_source_records_and_yields_none() {
    let sink = RecSink::new();
    let client = listenbrainz_client(&dead_url().await, CountingPacer::new(), sink.clone());

    let outcome = client.user_listens("listener", 10, &lb_anonymous()).await;

    assert!(outcome.into_option().is_none());
    assert_eq!(sink.count(), 1, "a dead source records once");
    assert_eq!(sink.sources(), vec!["listenbrainz".to_owned()]);
}

#[tokio::test]
async fn listenbrainz_pacing_constants_match_slice_spec() {
    assert_eq!(listenbrainz::RATE_PER_SEC, 1.0, "slice specifies 1/second");
    assert_eq!(listenbrainz::BURST, 1, "no burst, evenly paced");
    assert_eq!(listenbrainz::DEFAULT_RETRY_AFTER_SECS, 2.0);
    assert_eq!(listenbrainz::RELEASE_GROUP_BATCH, 25);
    assert_eq!(listenbrainz::RECORDING_BATCH, 50);
    assert_eq!(listenbrainz::MAX_GENRE_IDS, 500);
}

// ---------------------------------------------------------------------------
// Seam surface briefs
// ---------------------------------------------------------------------------

#[test]
fn provider_default_hosts_match_v2() {
    assert_eq!(
        audiodb::DEFAULT_BASE_URL,
        "https://www.theaudiodb.com/api/v1/json"
    );
    assert_eq!(acoustid::DEFAULT_BASE_URL, "https://api.acoustid.org");
    assert_eq!(acoustid::LOOKUP_PATH, "/v2/lookup");
    assert_eq!(
        lastfm::DEFAULT_BASE_URL,
        "https://ws.audioscrobbler.com/2.0/"
    );
    assert_eq!(
        listenbrainz::DEFAULT_BASE_URL,
        "https://api.listenbrainz.org"
    );
}

#[test]
fn provider_noop_sink_drops_records() {
    // The shared sink accepts records without effect for callers that
    // handle absence themselves.
    for source in [
        audiodb::SOURCE,
        acoustid::SOURCE,
        lastfm::SOURCE,
        listenbrainz::SOURCE,
    ] {
        <NoopSink as DegradationSink>::record(&NoopSink, source, "note".to_owned());
    }
    assert_eq!(audiodb::SOURCE, "audiodb");
    assert_eq!(acoustid::SOURCE, "acoustid");
    assert_eq!(lastfm::SOURCE, "lastfm");
    assert_eq!(listenbrainz::SOURCE, "listenbrainz");
}

#[test]
fn lastfm_can_sign_needs_secret_and_session() {
    assert!(!lastfm_creds().can_sign(), "no session, cannot sign");
    assert!(lastfm_linked_creds().can_sign());
    let mut secret_only = lastfm_creds();
    secret_only.session_key = Some(String::new());
    assert!(
        secret_only.can_sign(),
        "presence, not content, gates signing"
    );
}

#[test]
fn listenbrainz_missing_collapses_to_none() {
    let missing: listenbrainz::Outcome<String> = listenbrainz::Outcome::Missing;
    assert_eq!(missing.into_option(), None);
}
