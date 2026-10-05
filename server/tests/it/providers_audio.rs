//! AudioDB, AcoustID, Last.fm and ListenBrainz clients against a scripted
//! loopback server: the decode of each real wire shape, rate limits and
//! Retry-After, degradation recording, and credential handling.

use droppedneedle::providers::{DegradationSink, Pacer};
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
// AudioDB
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

// ---------------------------------------------------------------------------
// AcoustID
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

// ---------------------------------------------------------------------------
// Last.fm
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

// ---------------------------------------------------------------------------
// ListenBrainz
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
