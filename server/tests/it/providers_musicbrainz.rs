//! MusicBrainz + Cover Art Archive provider briefs.
//!
//! Per-provider contract briefs (tolerant unknown fields,
//! required-identity-field decode failure, no `default()` empties), quirk
//! briefs per live-cited behavior, limiter briefs per policy row, and the
//! identity-critical failure brief. Transport is hand-rolled scripted fakes
//! only: no live network, no HTTP-mocking libraries. Each fake keys scripted
//! outcomes by request URL and records every request for header, query, and
//! include assertions.

use droppedneedle::providers::DegradationSink;
use droppedneedle::providers::{coverart, musicbrainz};

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

const RELEASE_MBID: &str = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b";
const AVALON_MBID: &str = "0687c8a5-40a2-4a0c-bdc9-c1d80d94bef5";
const IMMUNITY_MBID: &str = "c85ad49c-6bfb-4bdc-96f8-f5f305a8799e";
const IMMUNITY_GROUP_MBID: &str = "fc97b087-221c-4ea4-9dd9-5277a52eb84a";
const RETIRED_RECORDING_MBID: &str = "5224cfc7-b3bb-4008-a41b-21b168dc631f";
const CANONICAL_RECORDING_MBID: &str = "beaf82cd-24f9-4163-b1a9-022339a30f77";
const ARTIST_MBID: &str = "5441c29d-3602-4898-b1a1-b77fa23b8e50";
const GROUP_MBID: &str = "dcff25f1-702d-3b5e-b0da-d48172e6e62a";
const BRAINZMASH_RETIRED_MBID: &str = "77a698a8-98da-401d-a59b-1ae4bc28df56";
const BRAINZMASH_SURVIVOR_MBID: &str = "9cb4af06-1c2d-4e5f-8a7b-6c5d4e3f2a10";

/// Scripted MusicBrainz transport: outcomes queue per request URL, every
/// request recorded. Unscripted URLs fail loudly so briefs stay explicit.
struct FakeMb {
    scripts: Mutex<HashMap<String, VecDeque<Result<musicbrainz::RawResponse, String>>>>,
    seen: Mutex<Vec<musicbrainz::MbRequest>>,
}

impl FakeMb {
    fn new() -> Self {
        Self {
            scripts: Mutex::new(HashMap::new()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn script(&self, url: &str, outcome: Result<musicbrainz::RawResponse, String>) {
        self.scripts
            .lock()
            .unwrap()
            .entry(url.to_owned())
            .or_default()
            .push_back(outcome);
    }

    fn seen(&self) -> Vec<musicbrainz::MbRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl musicbrainz::MbTransport for FakeMb {
    async fn get(
        &self,
        request: &musicbrainz::MbRequest,
    ) -> Result<musicbrainz::RawResponse, musicbrainz::TransportError> {
        self.seen.lock().unwrap().push(request.clone());
        let mut scripts = self.scripts.lock().unwrap();
        match scripts.get_mut(&request.url).and_then(VecDeque::pop_front) {
            Some(Ok(response)) => Ok(response),
            Some(Err(cause)) => Err(musicbrainz::TransportError(cause)),
            None => Err(musicbrainz::TransportError(format!(
                "unscripted {}",
                request.url
            ))),
        }
    }
}

/// Scripted CAA transport with the same record-and-replay discipline.
struct FakeCaa {
    scripts: Mutex<HashMap<String, VecDeque<Result<coverart::RawResponse, String>>>>,
    seen: Mutex<Vec<coverart::CaaRequest>>,
}

impl FakeCaa {
    fn new() -> Self {
        Self {
            scripts: Mutex::new(HashMap::new()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn script(&self, url: &str, outcome: Result<coverart::RawResponse, String>) {
        self.scripts
            .lock()
            .unwrap()
            .entry(url.to_owned())
            .or_default()
            .push_back(outcome);
    }

    fn seen(&self) -> Vec<coverart::CaaRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl coverart::CaaTransport for FakeCaa {
    async fn get(
        &self,
        request: &coverart::CaaRequest,
    ) -> Result<coverart::RawResponse, coverart::TransportError> {
        self.seen.lock().unwrap().push(request.clone());
        let mut scripts = self.scripts.lock().unwrap();
        match scripts.get_mut(&request.url).and_then(VecDeque::pop_front) {
            Some(Ok(response)) => Ok(response),
            Some(Err(cause)) => Err(coverart::TransportError(cause)),
            None => Err(coverart::TransportError(format!(
                "unscripted {}",
                request.url
            ))),
        }
    }
}

/// Degradation sink that keeps every record for assertions.
struct VecSink {
    records: Mutex<Vec<(String, String)>>,
}

impl VecSink {
    fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<(String, String)> {
        self.records.lock().unwrap().clone()
    }
}

/// Shared sink handle: the client records through this while the brief
/// reads the same `Arc`. A newtype because the orphan rule forbids
/// implementing the foreign sink for `Arc` directly.
#[derive(Clone)]
struct SharedSink(Arc<VecSink>);

impl DegradationSink for SharedSink {
    fn record(&self, source: &'static str, message: String) {
        self.0
            .records
            .lock()
            .unwrap()
            .push((source.to_owned(), message));
    }
}

use std::sync::Arc;

fn fast_mb(
    scripts: Vec<(String, Result<musicbrainz::RawResponse, String>)>,
) -> (Arc<SharedFakeMb>, Arc<VecSink>) {
    let fake = Arc::new(SharedFakeMb::new());
    for (url, outcome) in scripts {
        fake.script(&url, outcome);
    }
    let sink = Arc::new(VecSink::new());
    (fake, sink)
}

/// Reference-counted fake so client and brief share one script.
struct SharedFakeMb {
    inner: FakeMb,
}

impl SharedFakeMb {
    fn new() -> Self {
        Self {
            inner: FakeMb::new(),
        }
    }

    fn script(&self, url: &str, outcome: Result<musicbrainz::RawResponse, String>) {
        self.inner.script(url, outcome);
    }

    fn seen(&self) -> Vec<musicbrainz::MbRequest> {
        self.inner.seen()
    }
}

impl musicbrainz::MbTransport for SharedFakeMb {
    async fn get(
        &self,
        request: &musicbrainz::MbRequest,
    ) -> Result<musicbrainz::RawResponse, musicbrainz::TransportError> {
        musicbrainz::MbTransport::get(&self.inner, request).await
    }
}

/// Shared fake transport: the client and the brief hold clones of one
/// script. A newtype because the orphan rule forbids implementing the
/// foreign port for `Arc` directly.
#[derive(Clone)]
struct SharedMb(Arc<SharedFakeMb>);

impl musicbrainz::MbTransport for SharedMb {
    async fn get(
        &self,
        request: &musicbrainz::MbRequest,
    ) -> Result<musicbrainz::RawResponse, musicbrainz::TransportError> {
        musicbrainz::MbTransport::get(self.0.as_ref(), request).await
    }
}

/// Shared cover-art fake, newtyped for the same orphan-rule reason.
#[derive(Clone)]
struct SharedCaa(Arc<FakeCaa>);

impl coverart::CaaTransport for SharedCaa {
    async fn get(
        &self,
        request: &coverart::CaaRequest,
    ) -> Result<coverart::RawResponse, coverart::TransportError> {
        coverart::CaaTransport::get(self.0.as_ref(), request).await
    }
}

fn fast_gate() -> musicbrainz::RateGate {
    musicbrainz::RateGate::new(1000.0)
}

fn fast_scheduler() -> musicbrainz::BrainzMashScheduler {
    musicbrainz::BrainzMashScheduler::with_gate(musicbrainz::RateGate::new(1000.0))
}

fn official_client(
    fake: Arc<SharedFakeMb>,
    sink: Arc<VecSink>,
) -> musicbrainz::MusicBrainzClient<SharedMb, SharedSink> {
    musicbrainz::MusicBrainzClient::official(SharedMb(fake))
        .with_sink(SharedSink(sink))
        .with_gates(fast_gate(), fast_scheduler())
}

fn brainzmash_client(
    fake: Arc<SharedFakeMb>,
    sink: Arc<VecSink>,
) -> musicbrainz::MusicBrainzClient<SharedMb, SharedSink> {
    musicbrainz::MusicBrainzClient::brainzmash(SharedMb(fake), true)
        .with_sink(SharedSink(sink))
        .with_gates(fast_gate(), fast_scheduler())
}

fn fast_caa(fake: Arc<FakeCaa>) -> coverart::CaaClient<SharedCaa> {
    coverart::CaaClient::new(SharedCaa(fake))
        .with_gate(coverart::RateGate::new(1000.0))
        .with_backoff(Duration::from_millis(5), Duration::from_millis(1))
}

fn mb_url(path: &str) -> String {
    format!("{}{path}", musicbrainz::MB_API_BASE)
}

fn query_value(request: &musicbrainz::MbRequest, key: &str) -> Option<String> {
    request
        .query
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

// ---------------------------------------------------------------------------
// Contract briefs: tolerant wire, strict identity, honest absence.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_tolerates_unknown_fields_at_every_level() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url("/release"),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "count": 1,
                "created": "2026-07-21T00:00:00Z",
                "offset": 0,
                "future-envelope-key": {"nested": true},
                "releases": [{
                    "id": RELEASE_MBID,
                    "score": 100,
                    "title": "Goldberg Variations",
                    "status": "Official",
                    "future-hit-key": [1, 2],
                    "artist-credit": [{
                        "name": "Glenn Gould",
                        "joinphrase": "",
                        "future-credit-key": "x",
                        "artist": {"id": ARTIST_MBID, "name": "Glenn Gould", "sort-name": "Gould, Glenn"}
                    }],
                    "release-group": {"id": GROUP_MBID, "title": "Goldberg Variations"},
                    "media": [{"position": 1, "format": "CD", "track-count": 34}],
                    "label-info": [{"catalog-number": "ABC-1", "label": {"id": GROUP_MBID, "name": "CBS"}}]
                }]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    let page = client
        .search_releases(
            "Goldberg Variations",
            "Glenn Gould",
            10,
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("search decodes");
    assert_eq!(page.count, 1);
    assert_eq!(page.offset, 0);
    assert_eq!(page.items.len(), 1);
    let hit = &page.items[0];
    assert_eq!(hit.id, RELEASE_MBID);
    assert_eq!(musicbrainz::hit_score(hit.score, hit.ext_score), 100);
    assert_eq!(hit.title.as_deref(), Some("Goldberg Variations"));
    assert_eq!(hit.media.len(), 1);
    assert_eq!(hit.label_info[0].catalog_number.as_deref(), Some("ABC-1"));
    assert_eq!(fake.seen().len(), 1);
}

#[tokio::test]
async fn lookup_tolerates_unknown_fields_and_sparse_facets() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": RELEASE_MBID,
                "title": "Goldberg Variations",
                "future-release-key": {"deep": [true]},
                "media": [{"position": 1, "tracks": []}],
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake, sink);
    let found = client
        .lookup_release(
            RELEASE_MBID,
            &["recordings"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("lookup decodes");
    let lookup = found.expect("release present");
    assert_eq!(lookup.entity.id, RELEASE_MBID);
    assert!(lookup.entity.barcode.is_none());
    assert!(lookup.entity.release_group.is_none());
    assert!(lookup.redirects.is_empty());
}

#[tokio::test]
async fn missing_identity_id_fails_decoding() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            br#"{"title": "id-less"}"#.to_vec(),
        )),
    );
    let client = official_client(fake, sink);
    let error = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("missing id must fail");
    assert!(
        matches!(error, musicbrainz::MbError::Contract(_)),
        "got {error:?}"
    );

    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url("/release"),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            br#"{"count": 1, "offset": 0, "releases": [{"title": "id-less"}]}"#.to_vec(),
        )),
    );
    let client = official_client(fake, sink);
    let error = client
        .search_releases("x", "y", 5, musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("id-less hit must fail");
    assert!(
        matches!(error, musicbrainz::MbError::Contract(_)),
        "got {error:?}"
    );

    let error = client
        .resolve_recording_mbid("not-a-mbid", musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("malformed mbid must fail");
    assert!(
        matches!(error, musicbrainz::MbError::InvalidMbid(_)),
        "got {error:?}"
    );
}

#[tokio::test]
async fn not_found_is_absence_never_an_empty_struct() {
    let (fake, sink) = fast_mb(vec![]);
    let missing = "00000000-0000-0000-0000-000000000001";
    fake.script(
        &mb_url(&format!("/release/{missing}")),
        Ok(musicbrainz::RawResponse::new(
            404,
            vec![],
            br#"{"error":"Not Found"}"#.to_vec(),
        )),
    );
    fake.script(
        &mb_url("/release"),
        Ok(musicbrainz::RawResponse::new(404, vec![], Vec::new())),
    );
    fake.script(
        &mb_url("/url"),
        Ok(musicbrainz::RawResponse::new(404, vec![], Vec::new())),
    );
    let client = official_client(fake, sink.clone());
    let found = client
        .lookup_release(missing, &[], musicbrainz::Criticality::IdentityCritical)
        .await
        .expect("404 lookup resolves");
    assert!(found.is_none(), "404 is None, not an empty release");
    let page = client
        .search_releases(
            "nothing",
            "nobody",
            5,
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect("404 search resolves");
    assert!(page.items.is_empty() && page.count == 0);
    let resolved = client
        .resolve_url(
            "https://www.discogs.com/release/1",
            &["release-rels"],
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect("404 url resolves");
    assert!(resolved.relations.is_empty());
    assert!(
        sink.records().is_empty(),
        "definitive absence records no degradation"
    );
}

#[tokio::test]
async fn requests_carry_user_agent_fmt_and_sorted_includes() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            format!(r#"{{"id": "{RELEASE_MBID}"}}"#).into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    client
        .lookup_release(
            &RELEASE_MBID.to_ascii_uppercase(),
            &["recordings", "artist-credits", "recordings"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("lookup decodes");
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(
        request.header("User-Agent"),
        Some(droppedneedle::http_client::USER_AGENT)
    );
    assert_eq!(query_value(request, "fmt").as_deref(), Some("json"));
    assert_eq!(
        query_value(request, "inc").as_deref(),
        Some("artist-credits+recordings")
    );
    assert!(
        request.url.ends_with(&format!("/release/{RELEASE_MBID}")),
        "mbid normalized: {}",
        request.url
    );
}

#[tokio::test]
async fn limiters_match_the_verified_policy_table() {
    assert_eq!(musicbrainz::RateGate::musicbrainz().rate_per_sec(), 1.0);
    assert_eq!(musicbrainz::RateGate::brainzmash().rate_per_sec(), 10.0);
    assert_eq!(coverart::RateGate::coverart().rate_per_sec(), 1.0);
    assert_eq!(
        droppedneedle::provider_policy::lookup("musicbrainz")
            .expect("mb row")
            .limit,
        "1 req/s"
    );
    assert!(
        droppedneedle::provider_policy::lookup("coverartarchive")
            .expect("caa row")
            .limit
            .contains("~1/s")
    );

    let gate = musicbrainz::RateGate::new(100.0);
    let start = tokio::time::Instant::now();
    gate.acquire().await;
    gate.acquire().await;
    assert!(
        start.elapsed() >= Duration::from_millis(8),
        "second acquire waits out the interval"
    );
}

#[tokio::test]
async fn identity_critical_dead_provider_fails_typed() {
    for critical in [musicbrainz::Criticality::IdentityCritical] {
        let (fake, sink) = fast_mb(vec![]);
        fake.script(
            &mb_url(&format!("/release/{RELEASE_MBID}")),
            Err("connection refused".to_owned()),
        );
        let client = official_client(fake, sink.clone());
        let error = client
            .lookup_release(RELEASE_MBID, &[], critical)
            .await
            .expect_err("dead provider fails identity-critical");
        assert!(
            matches!(error, musicbrainz::MbError::Unavailable(_)),
            "got {error:?}"
        );
        assert!(
            sink.records().is_empty(),
            "typed failure needs no degradation record"
        );

        let (fake, sink) = fast_mb(vec![]);
        fake.script(
            &mb_url(&format!("/release/{RELEASE_MBID}")),
            Ok(musicbrainz::RawResponse::new(500, vec![], Vec::new())),
        );
        let client = official_client(fake, sink);
        let error = client
            .lookup_release(RELEASE_MBID, &[], critical)
            .await
            .expect_err("500 fails identity-critical");
        assert!(
            matches!(error, musicbrainz::MbError::Unavailable(_)),
            "got {error:?}"
        );
    }
}

#[tokio::test]
async fn best_effort_dead_provider_records_and_resolves_absence() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Err("connection refused".to_owned()),
    );
    fake.script(
        &mb_url("/release"),
        Ok(musicbrainz::RawResponse::new(500, vec![], Vec::new())),
    );
    let client = official_client(fake, sink.clone());
    let found = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect("degraded lookup resolves");
    assert!(found.is_none());
    let page = client
        .search_releases("x", "y", 5, musicbrainz::Criticality::BestEffort)
        .await
        .expect("degraded search resolves");
    assert!(page.items.is_empty());
    let records = sink.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].0, "musicbrainz");
    assert_eq!(records[1].0, "musicbrainz");
    assert!(
        records[0].1.starts_with("lookup_release: "),
        "operation folds into the message: {}",
        records[0].1
    );
    assert!(
        records[1].1.starts_with("search_releases: "),
        "operation folds into the message: {}",
        records[1].1
    );
}

#[tokio::test]
async fn rate_limit_surfaces_retry_after_and_degrades_best_effort() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            429,
            vec![("Retry-After", "5")],
            Vec::new(),
        )),
    );
    let client = official_client(fake, sink.clone());
    let error = client
        .lookup_release(
            RELEASE_MBID,
            &[],
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect_err("429 fails identity-critical");
    match error {
        musicbrainz::MbError::RateLimited { retry_after_secs } => {
            assert_eq!(retry_after_secs, Some(5.0))
        }
        other => panic!("got {other:?}"),
    }

    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(503, vec![], Vec::new())),
    );
    let client = official_client(fake, sink.clone());
    let error = client
        .lookup_release(
            RELEASE_MBID,
            &[],
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect_err("503 fails identity-critical");
    assert!(
        matches!(
            error,
            musicbrainz::MbError::RateLimited {
                retry_after_secs: None
            }
        ),
        "got {error:?}"
    );

    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(429, vec![], Vec::new())),
    );
    let client = official_client(fake, sink.clone());
    let found = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect("429 degrades best-effort");
    assert!(found.is_none());
    assert_eq!(sink.records().len(), 1);
}

#[test]
fn retry_after_accepts_all_legal_shapes() {
    assert_eq!(musicbrainz::parse_retry_after_secs(Some("5")), Some(5.0));
    assert_eq!(
        musicbrainz::parse_retry_after_secs(Some("9999")),
        Some(60.0)
    );
    assert_eq!(musicbrainz::parse_retry_after_secs(Some("garbage"),), None);
    assert_eq!(musicbrainz::parse_retry_after_secs(Some("-3")), None);
    assert_eq!(musicbrainz::parse_retry_after_secs(None), None);
    assert_eq!(
        musicbrainz::parse_retry_after_secs(Some("Tue, 21 Jul 2035 00:00:00 GMT")),
        Some(60.0)
    );
    assert_eq!(
        musicbrainz::parse_retry_after_secs(Some("Tuesday, 21-Jul-35 00:00:00 GMT")),
        Some(60.0)
    );
    assert_eq!(
        musicbrainz::parse_retry_after_secs(Some("Tue Jul 21 00:00:00 2035")),
        Some(60.0)
    );
    assert_eq!(
        musicbrainz::parse_retry_after_secs(Some("Tue, 21 Jul 2000 00:00:00 GMT")),
        None
    );
    assert_eq!(coverart::parse_retry_after_secs(Some("7")), Some(7.0));
    assert_eq!(
        coverart::parse_retry_after_secs(Some("Tuesday, 21-Jul-35 00:00:00 GMT")),
        Some(60.0)
    );
}

#[tokio::test]
async fn invalid_mbid_and_rejected_requests_stay_distinct() {
    let (fake, sink) = fast_mb(vec![]);
    let zeroes = "00000000-0000-0000-0000-000000000000";
    fake.script(
        &mb_url(&format!("/release/{zeroes}")),
        Ok(musicbrainz::RawResponse::new(
            400,
            vec![],
            br#"{"error":"Invalid mbid."}"#.to_vec(),
        )),
    );
    fake.script(
        &mb_url("/artist"),
        Ok(musicbrainz::RawResponse::new(403, vec![], Vec::new())),
    );
    let client = official_client(fake.clone(), sink);
    let error = client
        .lookup_release(zeroes, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("400 is not absence");
    assert!(
        matches!(error, musicbrainz::MbError::InvalidMbid(_)),
        "got {error:?}"
    );
    let error = client
        .search_artists("x", 5, musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("403 is not absence");
    assert!(
        matches!(error, musicbrainz::MbError::Rejected(403)),
        "got {error:?}"
    );

    let before = fake.seen().len();
    let error = client
        .resolve_recording_mbid("  ", musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("blank mbid fails locally");
    assert!(
        matches!(error, musicbrainz::MbError::InvalidMbid(_)),
        "got {error:?}"
    );
    assert_eq!(fake.seen().len(), before, "malformed mbid sends nothing");
}

#[test]
fn mbid_shape_and_normalization() {
    assert!(musicbrainz::is_valid_mbid(RELEASE_MBID));
    assert!(musicbrainz::is_valid_mbid(
        &RELEASE_MBID.to_ascii_uppercase()
    ));
    assert!(!musicbrainz::is_valid_mbid("unknown_1"));
    assert!(!musicbrainz::is_valid_mbid("not-a-mbid"));
    assert!(!musicbrainz::is_valid_mbid(""));
    assert!(coverart::is_valid_mbid(RELEASE_MBID));
    assert!(!coverart::is_valid_mbid("xyz"));
    assert_eq!(musicbrainz::normalize_mb_id("  ABC-DEF "), "abc-def");
}

// ---------------------------------------------------------------------------
// Quirk briefs: every live-cited behavior, with its citation.
// ---------------------------------------------------------------------------

#[test]
fn lucene_builders_match_the_verified_shapes() {
    // Live-verified against MusicBrainz WS/2 on 2026-08-13 (v2 query
    // builders in `musicbrainz_base.py`).
    assert_eq!(
        musicbrainz::build_release_search_query("Discovery", "Daft Punk"),
        r#"release:"Discovery" AND artist:"Daft Punk""#
    );
    assert_eq!(
        musicbrainz::build_release_search_query("Discovery", ""),
        r#"release:"Discovery""#
    );
    assert_eq!(
        musicbrainz::build_release_group_search_query("Discovery", "Daft Punk"),
        r#"(releasegroup:"Discovery" OR release:"Discovery") AND artist:"Daft Punk""#
    );
    assert_eq!(
        musicbrainz::build_recording_search_query("One More Time", "Daft Punk"),
        r#"recording:"One More Time" AND artist:"Daft Punk""#
    );
    assert_eq!(
        musicbrainz::build_release_search_query("AC/DC (Live!)", "x"),
        r#"release:"AC\/DC \(Live\!\)" AND artist:"x""#
    );
    assert_eq!(musicbrainz::escape_lucene_phrase("a+b"), r"a\+b");
}

#[test]
fn hit_score_reads_both_observed_keys() {
    assert_eq!(musicbrainz::hit_score(Some(90), Some(80)), 90);
    assert_eq!(musicbrainz::hit_score(None, Some(80)), 80);
    assert_eq!(musicbrainz::hit_score(None, None), 0);
}

#[test]
fn credit_display_name_prefers_credited_then_canonical() {
    let credited = musicbrainz::ArtistCreditName {
        name: "Prince".to_owned(),
        joinphrase: String::new(),
        artist: musicbrainz::ArtistRef {
            id: ARTIST_MBID.to_owned(),
            name: "Prince Rogers Nelson".to_owned(),
            sort_name: None,
        },
    };
    let canonical = musicbrainz::ArtistCreditName {
        name: String::new(),
        joinphrase: String::new(),
        artist: musicbrainz::ArtistRef {
            id: ARTIST_MBID.to_owned(),
            name: "Prince Rogers Nelson".to_owned(),
            sort_name: Some("Prince".to_owned()),
        },
    };
    assert_eq!(
        musicbrainz::credit_display_name(std::slice::from_ref(&credited)),
        Some("Prince")
    );
    assert_eq!(
        musicbrainz::credit_display_name(std::slice::from_ref(&canonical)),
        Some("Prince Rogers Nelson")
    );
    assert_eq!(musicbrainz::credit_display_name(&[]), None);
    assert_eq!(musicbrainz::parse_year(Some("2026-07-21")), Some(2026));
    assert_eq!(musicbrainz::parse_year(Some("2026")), Some(2026));
    assert_eq!(musicbrainz::parse_year(Some("")), None);
    assert_eq!(musicbrainz::parse_year(Some("soon")), None);
    assert_eq!(musicbrainz::parse_year(None), None);
}

#[tokio::test]
async fn release_track_title_and_credit_win_over_recording() {
    // Live 2026-07-29 (Avalon track 14 vs its recording) and 2026-07-31
    // (Bach on the track, Gould on the recording): edition surfaces prefer
    // the release track, and track.id is never the recording MBID.
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{AVALON_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": AVALON_MBID,
                "title": "Avalon",
                "media": [{
                    "position": 1,
                    "track-count": 14,
                    "tracks": [{
                        "id": "741e8bcd-9d03-3b61-bb04-ecf22f4784e1",
                        "position": 14,
                        "number": "14",
                        "title": "The Fisherman Will Be Bewildered",
                        "length": 200_000,
                        "artist-credit": [{
                            "name": "Anthony Green",
                            "joinphrase": "",
                            "artist": {"id": ARTIST_MBID, "name": "Anthony Green"}
                        }],
                        "recording": {
                            "id": "ec935e35-b2fa-4925-aa83-052d9e3e69f1",
                            "title": "The Fishermen Will Be Bewildered",
                            "length": 199_000,
                            "artist-credit": [{
                                "name": "Somebody Else",
                                "joinphrase": "",
                                "artist": {"id": GROUP_MBID, "name": "Somebody Else"}
                            }]
                        }
                    }, {
                        "id": "aaaaaaaa-9d03-3b61-bb04-ecf22f4784e1",
                        "recording": {
                            "id": "bbbbbbbb-b2fa-4925-aa83-052d9e3e69f1",
                            "title": "Fallback Title",
                            "length": 100_000,
                        }
                    }]
                }]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake, sink);
    let found = client
        .lookup_release(
            AVALON_MBID,
            &["artist-credits", "recordings"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("avalon decodes");
    let release = found.expect("avalon present").entity;
    let first = &release.media[0].tracks[0];
    assert_ne!(first.id, first.recording.as_ref().expect("recording").id);
    assert_eq!(
        first.display_title(),
        Some("The Fisherman Will Be Bewildered")
    );
    assert_eq!(first.credit().len(), 1);
    assert_eq!(first.credit()[0].name, "Anthony Green");
    assert_eq!(first.length_ms(), Some(200_000));
    let second = &release.media[0].tracks[1];
    assert_eq!(second.display_title(), Some("Fallback Title"));
    assert!(second.credit().is_empty());
    assert_eq!(second.length_ms(), Some(100_000));
}

#[tokio::test]
async fn explicit_nulls_decode_instead_of_failing() {
    // Live 2026-07-28 (null packaging pair, null catalogue number),
    // 2026-08-03 (null work type pair), 2026-08-15 (null status and
    // primary-type pairs): each pair is nullable independently.
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": RELEASE_MBID,
                "status": serde_json::Value::Null,
                "status-id": serde_json::Value::Null,
                "packaging": serde_json::Value::Null,
                "packaging-id": serde_json::Value::Null,
                "label-info": [{"catalog-number": serde_json::Value::Null, "label": {"id": GROUP_MBID, "name": "Self-released"}}],
                "release-group": {
                    "id": GROUP_MBID,
                    "primary-type": serde_json::Value::Null,
                    "primary-type-id": serde_json::Value::Null,
                    "secondary-types": ["Compilation"],
                },
                "relations": [{
                    "type": "performance",
                    "type-id": "a3005666-a872-32c3-ad06-98af558e99b0",
                    "work": {
                        "id": "fa1f9350-3d27-35c8-abc2-a4455cf68d24",
                        "type": serde_json::Value::Null,
                        "type-id": serde_json::Value::Null,
                    }
                }]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake, sink);
    let found = client
        .lookup_release(
            RELEASE_MBID,
            &["labels"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("nulls decode");
    let release = found.expect("release present").entity;
    assert!(release.status.is_none() && release.status_id.is_none());
    assert!(release.packaging.is_none() && release.packaging_id.is_none());
    assert!(release.label_info[0].catalog_number.is_none());
    assert!(release.label_info[0].label.is_some());
    let group = release.release_group.expect("group present");
    assert!(group.primary_type.is_none() && group.primary_type_id.is_none());

    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": RELEASE_MBID,
                "relations": [{"type": "performance"}]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake, sink);
    let error = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("relation type-id stays required (live 2026-08-15)");
    assert!(
        matches!(error, musicbrainz::MbError::Contract(_)),
        "got {error:?}"
    );
}

#[tokio::test]
async fn artist_credit_order_and_joinphrases_survive() {
    // Live 2026-07-31: two ordered entries with exact join phrases
    // ("; " then "") on the Goldberg release credit.
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": RELEASE_MBID,
                "artist-credit": [
                    {"name": "Johann Sebastian Bach", "joinphrase": "; ",
                     "artist": {"id": ARTIST_MBID, "name": "Johann Sebastian Bach", "sort-name": "Bach, Johann Sebastian"}},
                    {"name": "Glenn Gould", "joinphrase": "",
                     "artist": {"id": GROUP_MBID, "name": "Glenn Gould", "sort-name": "Gould, Glenn"}}
                ]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake, sink);
    let found = client
        .lookup_release(
            RELEASE_MBID,
            &["artist-credits"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("credit decodes");
    let credit = &found.expect("release present").entity.artist_credit;
    assert_eq!(credit.len(), 2);
    assert_eq!(credit[0].joinphrase, "; ");
    assert_eq!(credit[1].joinphrase, "");
    assert_eq!(
        credit[0].artist.sort_name.as_deref(),
        Some("Bach, Johann Sebastian")
    );
}

#[tokio::test]
async fn recording_lookup_carries_releases_and_rankable_groups() {
    // Live 2026-07-20: `inc=releases+release-groups` puts id/status/date
    // on each release and the ranking fields on each group.
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/recording/{CANONICAL_RECORDING_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": CANONICAL_RECORDING_MBID,
                "title": "Some Track",
                "length": 180_000,
                "isrcs": ["USABC1234567"],
                "releases": [{
                    "id": RELEASE_MBID,
                    "status": "Official",
                    "date": "1982-01-01",
                    "release-group": {
                        "id": GROUP_MBID,
                        "title": "Some Album",
                        "primary-type": "Album",
                        "secondary-types": ["Compilation"],
                        "first-release-date": "1982-01-01"
                    }
                }]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    let found = client
        .lookup_recording(
            CANONICAL_RECORDING_MBID,
            &["releases", "release-groups"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("recording decodes");
    let recording = &found.expect("recording present").entity;
    assert_eq!(recording.isrcs, vec!["USABC1234567".to_owned()]);
    assert_eq!(recording.releases.len(), 1);
    let request = &fake.seen()[0];
    assert_eq!(
        query_value(request, "inc").as_deref(),
        Some("release-groups+releases")
    );
}

#[test]
fn ranking_orders_one_recording_without_substitution() {
    // Live 2026-07-20: ranking only chooses among the release groups
    // already attached to a recording. Official Album beats Bootleg Live;
    // version-changing secondaries lose; dates break remaining ties.
    let group = |id: &str, primary: &str, secondaries: Vec<&str>| musicbrainz::ReleaseGroupRef {
        id: id.to_owned(),
        title: Some("T".to_owned()),
        first_release_date: None,
        primary_type: Some(primary.to_owned()),
        primary_type_id: None,
        secondary_types: secondaries.into_iter().map(str::to_owned).collect(),
        artist_credit: Vec::new(),
    };
    let official_album = musicbrainz::RecordingRelease {
        id: RELEASE_MBID.to_owned(),
        status: Some("Official".to_owned()),
        date: Some("1982-01-01".to_owned()),
        release_group: Some(group(
            "aaaaaaaa-0000-0000-0000-000000000001",
            "Album",
            vec![],
        )),
    };
    let bootleg_live = musicbrainz::RecordingRelease {
        id: AVALON_MBID.to_owned(),
        status: Some("Bootleg".to_owned()),
        date: Some("1980-01-01".to_owned()),
        release_group: Some(group(
            "bbbbbbbb-0000-0000-0000-000000000002",
            "Album",
            vec!["Live"],
        )),
    };
    let releases = vec![bootleg_live.clone(), official_album.clone()];
    let (best, _) = musicbrainz::best_release_group_for_recording(&releases).expect("a group wins");
    assert_eq!(best.id, "aaaaaaaa-0000-0000-0000-000000000001");
    assert!(musicbrainz::best_release_group_for_recording(&[]).is_none());
    assert!(
        musicbrainz::best_release_group_for_recording(&[musicbrainz::RecordingRelease {
            id: RELEASE_MBID.to_owned(),
            status: None,
            date: None,
            release_group: None,
        }])
        .is_none()
    );

    let rank = musicbrainz::recording_release_group_rank(
        Some("Official"),
        &[],
        Some("Album"),
        Some("1982-01-01"),
        "aaaaaaaa-0000-0000-0000-000000000001",
    );
    let worse = musicbrainz::recording_release_group_rank(
        Some("Official"),
        &["Live".to_owned()],
        Some("Album"),
        Some("1982-01-01"),
        "aaaaaaaa-0000-0000-0000-000000000001",
    );
    assert!(rank < worse);
}

#[tokio::test]
async fn exact_release_fails_closed_without_provider_group() {
    // Live 2026-08-10: Clairo _Immunity_ omits `release-group` without the
    // include; absence after that request never becomes an assumed group.
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{IMMUNITY_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            format!(r#"{{"id": "{IMMUNITY_MBID}", "title": "Immunity"}}"#).into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    let error = client
        .lookup_exact_release(IMMUNITY_MBID, musicbrainz::Criticality::IdentityCritical)
        .await
        .expect_err("missing group fails closed");
    assert!(
        matches!(error, musicbrainz::MbError::Contract(_)),
        "got {error:?}"
    );
    let request = &fake.seen()[0];
    assert!(
        query_value(request, "inc")
            .as_deref()
            .unwrap_or("")
            .contains("release-groups"),
        "exact release always requests release-groups"
    );

    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{IMMUNITY_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": IMMUNITY_MBID,
                "release-group": {"id": IMMUNITY_GROUP_MBID, "title": "Immunity"}
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake, sink);
    let found = client
        .lookup_exact_release(IMMUNITY_MBID, musicbrainz::Criticality::IdentityCritical)
        .await
        .expect("exact release with group decodes");
    let release = found.expect("present").entity;
    assert_eq!(
        release.release_group.expect("group").id,
        IMMUNITY_GROUP_MBID
    );
    let group = client
        .resolve_release_to_release_group(IMMUNITY_MBID, musicbrainz::Criticality::BestEffort)
        .await
        .expect("unscripted follow-up degrades to absence");
    assert!(
        group.is_none(),
        "dead provider on best-effort resolves to None"
    );
}

#[tokio::test]
async fn release_to_group_resolution_returns_provider_group() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{IMMUNITY_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": IMMUNITY_MBID,
                "release-group": {"id": IMMUNITY_GROUP_MBID}
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake, sink);
    let group = client
        .resolve_release_to_release_group(IMMUNITY_MBID, musicbrainz::Criticality::BestEffort)
        .await
        .expect("resolution decodes");
    assert_eq!(group.as_deref(), Some(IMMUNITY_GROUP_MBID));
}

#[tokio::test]
async fn retired_recording_redirect_proves_canonical_id() {
    // Live 2026-08-10: retired recording MBIDs 301 to a canonical
    // replacement; equivalence holds only after the lookup proves it.
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/recording/{RETIRED_RECORDING_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            301,
            vec![(
                "Location",
                &format!(
                    "{}/recording/{CANONICAL_RECORDING_MBID}?fmt=json",
                    musicbrainz::MB_API_BASE
                ),
            )],
            Vec::new(),
        )),
    );
    fake.script(
        &mb_url(&format!("/recording/{CANONICAL_RECORDING_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            format!(r#"{{"id": "{CANONICAL_RECORDING_MBID}", "title": "Merged Track"}}"#)
                .into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    let resolved = client
        .resolve_recording_mbid(
            RETIRED_RECORDING_MBID,
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect("redirect resolves");
    assert_eq!(resolved.as_deref(), Some(CANONICAL_RECORDING_MBID));
    assert_eq!(fake.seen().len(), 2);

    let found = client
        .lookup_recording(
            RETIRED_RECORDING_MBID,
            &[],
            musicbrainz::Criticality::IdentityCritical,
        )
        .await;
    assert!(
        found.is_err(),
        "unscripted follow-up fails loudly, never silently"
    );
}

#[tokio::test]
async fn foreign_and_browse_redirects_are_rejected() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            301,
            vec![(
                "Location",
                "https://evil.example/ws/2/release/aaaaaaaa-0000-0000-0000-000000000001",
            )],
            Vec::new(),
        )),
    );
    fake.script(
        &mb_url("/release"),
        Ok(musicbrainz::RawResponse::new(
            301,
            vec![(
                "Location",
                &format!("{}/release-group/{GROUP_MBID}", musicbrainz::MB_API_BASE),
            )],
            Vec::new(),
        )),
    );
    let client = official_client(fake, sink);
    let error = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("foreign redirect rejected");
    assert!(
        matches!(error, musicbrainz::MbError::RedirectRejected(_)),
        "got {error:?}"
    );
    let error = client
        .search_releases("x", "y", 5, musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("search redirect rejected");
    assert!(
        matches!(error, musicbrainz::MbError::RedirectRejected(_)),
        "got {error:?}"
    );
}

#[test]
fn redirect_pair_validation_rejects_non_lookup_shapes() {
    let from = format!("/recording/{RETIRED_RECORDING_MBID}");
    let to = format!("/recording/{CANONICAL_RECORDING_MBID}?fmt=json");
    let hop = musicbrainz::lookup_redirect_pair(&from, &to).expect("lookup hop validates");
    assert_eq!(hop.entity, "recording");
    assert_eq!(hop.from_mbid, RETIRED_RECORDING_MBID);
    assert_eq!(hop.to_mbid, CANONICAL_RECORDING_MBID);

    assert!(
        musicbrainz::lookup_redirect_pair(&from, &format!("/release/{RELEASE_MBID}")).is_none()
    );
    assert!(
        musicbrainz::lookup_redirect_pair(
            "/release-group",
            &format!("/release-group/{GROUP_MBID}")
        )
        .is_none()
    );
    assert!(musicbrainz::lookup_redirect_pair(&from, "/recording/not-a-mbid").is_none());

    let request_url = mb_url(&from);
    let hop = musicbrainz::official_redirect_hop(
        &request_url,
        musicbrainz::MB_API_BASE,
        &from,
        &format!(
            "{}/recording/{CANONICAL_RECORDING_MBID}",
            musicbrainz::MB_API_BASE
        ),
    )
    .expect("same-origin hop validates");
    assert_eq!(hop.to_mbid, CANONICAL_RECORDING_MBID);
    assert!(
        musicbrainz::official_redirect_hop(
            &request_url,
            musicbrainz::MB_API_BASE,
            &from,
            "https://evil.example/ws/2/recording/beaf82cd-24f9-4163-b1a9-022339a30f77",
        )
        .is_none(),
        "foreign host rejected"
    );
    assert!(
        musicbrainz::official_redirect_hop(
            &request_url,
            musicbrainz::MB_API_BASE,
            &from,
            "http://musicbrainz.org/ws/2/recording/beaf82cd-24f9-4163-b1a9-022339a30f77",
        )
        .is_none(),
        "scheme downgrade rejected"
    );
}

#[tokio::test]
async fn url_resolution_keeps_multi_target_ambiguity() {
    // Live 2026-07-21: the relation list is retained in full so a
    // multi-target response reads as ambiguity, never a first pick.
    let (fake, sink) = fast_mb(vec![]);
    let resource = "https://www.discogs.com/release/3562468";
    fake.script(
        &mb_url("/url"),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "resource": resource,
                "relations": [
                    {"type": "discogs", "type-id": "4a78823c-1c53-4176-a5f3-58026c76f2bc",
                     "release": {"id": RELEASE_MBID}},
                    {"type": "discogs", "type-id": "4a78823c-1c53-4176-a5f3-58026c76f2bc",
                     "release": {"id": AVALON_MBID}}
                ]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    let resolved = client
        .resolve_url(
            resource,
            &["release-rels"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("url resolves");
    assert_eq!(resolved.resource.as_deref(), Some(resource));
    assert_eq!(resolved.relations.len(), 2);
    assert_eq!(
        query_value(&fake.seen()[0], "resource").as_deref(),
        Some(resource)
    );
}

#[tokio::test]
async fn artist_search_and_lookup_decode() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url("/artist"),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "count": 1,
                "offset": 0,
                "artists": [{
                    "id": ARTIST_MBID,
                    "ext:score": 95,
                    "name": "Daft Punk",
                    "sort-name": "Daft Punk",
                    "type": "Group",
                    "area": {"id": GROUP_MBID, "name": "France"}
                }]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    fake.script(
        &mb_url(&format!("/artist/{ARTIST_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": ARTIST_MBID,
                "name": "Daft Punk",
                "sort-name": "Daft Punk",
                "type": "Group",
                "life-span": {"begin": "1993", "end": "2021"}
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    let artists = client
        .search_artists("Daft Punk", 5, musicbrainz::Criticality::BestEffort)
        .await
        .expect("artist search decodes");
    assert_eq!(artists.items.len(), 1);
    assert_eq!(
        musicbrainz::hit_score(artists.items[0].score, artists.items[0].ext_score),
        95
    );
    let artist = client
        .lookup_artist(
            ARTIST_MBID,
            &["aliases", "tags", "url-rels"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("artist lookup decodes")
        .expect("artist present")
        .entity;
    assert_eq!(
        artist.life_span.expect("span").begin.as_deref(),
        Some("1993")
    );
    assert_eq!(
        query_value(&fake.seen()[0], "query").as_deref(),
        Some(r#"artist:"Daft Punk""#),
        "artist search sends the verified phrase shape"
    );
}

#[tokio::test]
async fn release_group_search_and_lookup_decode() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url("/release-group"),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "count": 1,
                "offset": 0,
                "release-groups": [{
                    "id": GROUP_MBID,
                    "score": 100,
                    "title": "Discovery",
                    "primary-type": "Album",
                    "first-release-date": "2001-02-26"
                }]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    fake.script(
        &mb_url(&format!("/release-group/{GROUP_MBID}")),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "id": GROUP_MBID,
                "title": "Discovery",
                "releases": [{"id": RELEASE_MBID, "status": "Official"}]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    let groups = client
        .search_release_groups(
            "Discovery",
            "Daft Punk",
            5,
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("group search decodes");
    assert_eq!(groups.items[0].primary_type.as_deref(), Some("Album"));
    let group = client
        .lookup_release_group(
            GROUP_MBID,
            &["releases"],
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("group lookup decodes")
        .expect("group present")
        .entity;
    assert_eq!(group.releases.len(), 1);
    assert!(
        query_value(&fake.seen()[0], "query")
            .as_deref()
            .unwrap_or("")
            .starts_with("(releasegroup:")
    );
}

#[tokio::test]
async fn recording_search_decodes() {
    let (fake, sink) = fast_mb(vec![]);
    fake.script(
        &mb_url("/recording"),
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "count": 1,
                "offset": 0,
                "recordings": [{
                    "id": CANONICAL_RECORDING_MBID,
                    "score": 100,
                    "title": "One More Time",
                    "length": 320_000,
                    "releases": [{"id": RELEASE_MBID, "status": "Official"}]
                }]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = official_client(fake.clone(), sink);
    let recordings = client
        .search_recordings(
            "One More Time",
            "Daft Punk",
            5,
            musicbrainz::Criticality::BestEffort,
        )
        .await
        .expect("recording search decodes");
    assert_eq!(recordings.items[0].releases.len(), 1);
    assert!(
        query_value(&fake.seen()[0], "query")
            .as_deref()
            .unwrap_or("")
            .starts_with("recording:")
    );
}

// ---------------------------------------------------------------------------
// BrainzMash lifecycle briefs.
// ---------------------------------------------------------------------------

#[test]
fn brainzmash_endpoint_stays_pinned() {
    assert_eq!(
        musicbrainz::validate_brainzmash_url("https://api.brainzmash.cc/ws/2"),
        Ok(musicbrainz::BRAINZMASH_API_BASE)
    );
    assert_eq!(
        musicbrainz::validate_brainzmash_url("https://api.brainzmash.cc/ws/2/"),
        Ok(musicbrainz::BRAINZMASH_API_BASE),
        "trailing slash normalizes to the pinned origin"
    );
    for bad in [
        "http://api.brainzmash.cc/ws/2",
        "https://api.brainzmash.cc:8443/ws/2",
        "https://evil.example/ws/2",
        "https://user@api.brainzmash.cc/ws/2",
        "https://api.brainzmash.cc/ws/2?x=1",
        "https://api.brainzmash.cc/other",
        "not-a-url",
    ] {
        assert!(
            musicbrainz::validate_brainzmash_url(bad).is_err(),
            "{bad} must not validate"
        );
    }
}

#[test]
fn brainzmash_path_allowlist_holds() {
    assert_eq!(
        musicbrainz::validate_brainzmash_path(&format!("/release/{RELEASE_MBID}")),
        Ok(format!("/release/{RELEASE_MBID}"))
    );
    assert_eq!(
        musicbrainz::validate_brainzmash_path("/artist"),
        Ok("/artist".to_owned())
    );
    assert_eq!(
        musicbrainz::validate_brainzmash_path("/release"),
        Ok("/release".to_owned())
    );
    for bad in [
        "/admin",
        "/release/a/b",
        "release/x",
        "/release/../x",
        "/release/a\\b",
        "/release/a?x=1",
        "/release/a#b",
        "/release/a%b",
        "/release//x",
        "/release/./x",
        "/release/a_b",
    ] {
        assert!(
            musicbrainz::validate_brainzmash_path(bad).is_err(),
            "{bad} must not validate"
        );
    }
}

#[test]
fn brainzmash_request_url_validation_holds() {
    assert!(
        musicbrainz::validate_brainzmash_request_url(
            "https://api.brainzmash.cc/ws/2/release/aaaaaaaa-0000-0000-0000-000000000001"
        )
        .is_ok()
    );
    assert!(
        musicbrainz::validate_brainzmash_request_url(
            "https://api.brainzmash.cc/ws/2/release/aaaaaaaa-0000-0000-0000-000000000001?fmt=json"
        )
        .is_ok(),
        "the live probe Location carries a query string"
    );
    for bad in [
        "http://api.brainzmash.cc/ws/2/release/aaaaaaaa-0000-0000-0000-000000000001",
        "https://evil.example/ws/2/release/aaaaaaaa-0000-0000-0000-000000000001",
        "https://api.brainzmash.cc:443/ws/2/release/aaaaaaaa-0000-0000-0000-000000000001",
        "https://api.brainzmash.cc/ws/2/release/aaaaaaaa-0000-0000-0000-000000000001#frag",
        "https://api.brainzmash.cc/other/release/aaaaaaaa-0000-0000-0000-000000000001",
    ] {
        assert!(
            musicbrainz::validate_brainzmash_request_url(bad).is_err(),
            "{bad} must not validate"
        );
    }
}

#[tokio::test]
async fn brainzmash_follows_the_live_probe_redirect_shape() {
    // Probed live 2026-09-12: merged release 301s to a same-origin
    // `/ws/2/release/<survivor>?fmt=json` Location that answers 200.
    let (fake, sink) = fast_mb(vec![]);
    let from = format!(
        "{}/release/{BRAINZMASH_RETIRED_MBID}",
        musicbrainz::BRAINZMASH_API_BASE
    );
    let to = format!(
        "{}/release/{BRAINZMASH_SURVIVOR_MBID}",
        musicbrainz::BRAINZMASH_API_BASE
    );
    fake.script(
        &from,
        Ok(musicbrainz::RawResponse::new(
            301,
            vec![("Location", &format!("{to}?fmt=json"))],
            Vec::new(),
        )),
    );
    fake.script(
        &to,
        Ok(musicbrainz::RawResponse::new(
            200,
            vec![],
            format!(r#"{{"id": "{BRAINZMASH_SURVIVOR_MBID}", "title": "Survivor"}}"#).into_bytes(),
        )),
    );
    let client = brainzmash_client(fake.clone(), sink);
    assert!(matches!(
        client.source(),
        musicbrainz::MbSource::BrainzMash { .. }
    ));
    let found = client
        .lookup_release(
            BRAINZMASH_RETIRED_MBID,
            &[],
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect("brainzmash redirect resolves");
    let lookup = found.expect("survivor present");
    assert_eq!(lookup.entity.id, BRAINZMASH_SURVIVOR_MBID);
    assert_eq!(lookup.redirects.len(), 1);
    assert_eq!(lookup.redirects[0].entity, "release");
    assert_eq!(lookup.redirects[0].from_mbid, BRAINZMASH_RETIRED_MBID);
    assert_eq!(lookup.redirects[0].to_mbid, BRAINZMASH_SURVIVOR_MBID);
    assert_eq!(fake.seen().len(), 2);
}

#[tokio::test]
async fn brainzmash_binding_invalid_fails_closed_without_wire() {
    let (fake, sink) = fast_mb(vec![]);
    let client = musicbrainz::MusicBrainzClient::brainzmash(SharedMb(fake.clone()), false)
        .with_sink(SharedSink(sink))
        .with_gates(fast_gate(), fast_scheduler());
    let error = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("invalid binding fails closed");
    assert!(
        matches!(error, musicbrainz::MbError::Misconfigured(_)),
        "got {error:?}"
    );
    assert!(
        fake.seen().is_empty(),
        "nothing is sent without a valid binding"
    );
}

#[test]
fn brainzmash_cooldown_honors_retry_after_then_backs_off() {
    let scheduler = musicbrainz::BrainzMashScheduler::default();
    assert!(scheduler.cooldown_remaining().is_zero());
    let selected = scheduler.note_cooldown(Some(5.0));
    assert_eq!(selected, 5.0);
    assert!(!scheduler.cooldown_remaining().is_zero());
    scheduler.note_success();
    assert!(scheduler.cooldown_remaining().is_zero());

    let first = scheduler.note_cooldown(None);
    assert!(
        (0.5..=1.0).contains(&first),
        "first backoff in [0.5, 1]: {first}"
    );
    let second = scheduler.note_cooldown(None);
    assert!(
        (1.0..=2.0).contains(&second),
        "second backoff in [1, 2]: {second}"
    );
    assert_eq!(scheduler.note_cooldown(Some(9999.0)), 60.0);
}

#[tokio::test]
async fn brainzmash_rate_limit_cools_down_and_degrades() {
    let (fake, sink) = fast_mb(vec![]);
    let url = format!(
        "{}/release/{RELEASE_MBID}",
        musicbrainz::BRAINZMASH_API_BASE
    );
    fake.script(
        &url,
        Ok(musicbrainz::RawResponse::new(
            429,
            vec![("Retry-After", "1")],
            Vec::new(),
        )),
    );
    let client = brainzmash_client(fake, sink.clone());
    let found = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect("429 degrades");
    assert!(found.is_none());
    assert_eq!(sink.records().len(), 1);
}

#[tokio::test]
async fn brainzmash_503_is_a_dead_mirror_not_rate_limiting() {
    // v2 labels only the official 503 rate-limited; a BrainzMash 503
    // routes through provider-dead instead.
    let (fake, sink) = fast_mb(vec![]);
    let url = format!(
        "{}/release/{RELEASE_MBID}",
        musicbrainz::BRAINZMASH_API_BASE
    );
    fake.script(
        &url,
        Ok(musicbrainz::RawResponse::new(503, vec![], Vec::new())),
    );
    let client = brainzmash_client(fake, sink.clone());
    let error = client
        .lookup_release(
            RELEASE_MBID,
            &[],
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect_err("503 fails identity-critical");
    assert!(
        matches!(error, musicbrainz::MbError::Unavailable(_)),
        "got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Cover Art Archive briefs (live provider boundary 2026-07-21).
// ---------------------------------------------------------------------------

fn caa_listing_url(entity: &str, mbid: &str) -> String {
    format!(
        "{coverart_base}/{entity}/{mbid}",
        coverart_base = coverart::COVER_ART_ARCHIVE_BASE
    )
}

fn caa_listing_body() -> Vec<u8> {
    serde_json::json!({
        "images": [{
            "approved": true,
            "front": true,
            "back": false,
            "comment": "Front cover",
            "id": 42,
            "image": "http://coverartarchive.org/release/abc/front.png",
            "thumbnails": {
                "1200": "http://coverartarchive.org/release/abc/front-1200.jpg",
                "500": "http://coverartarchive.org/release/abc/front-500.jpg",
                "250": "http://coverartarchive.org/release/abc/front-250.jpg",
                "small": "http://coverartarchive.org/release/abc/front-small.jpg"
            },
            "types": ["Front"],
            "future-image-key": 1
        }, {
            "approved": false,
            "front": false,
            "back": true,
            "comment": "",
            "id": 43,
            "image": "http://coverartarchive.org/release/abc/back.png",
            "thumbnails": {},
            "types": ["Back", "Spine"],
        }],
        "future-root-key": true
    })
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn caa_listing_decodes_the_live_shape() {
    let fake = Arc::new(FakeCaa::new());
    let url = caa_listing_url("release", RELEASE_MBID);
    fake.script(
        &url,
        Ok(coverart::RawResponse::new(200, vec![], caa_listing_body())),
    );
    let client = fast_caa(fake.clone());
    let candidates = client
        .list_artwork(
            coverart::EntityKind::Release,
            RELEASE_MBID,
            coverart::DownloadSize::Size500,
        )
        .await
        .expect("listing decodes");
    assert_eq!(candidates.len(), 2);
    let first = &candidates[0];
    assert_eq!(
        first.candidate_id,
        format!("caa:release:{RELEASE_MBID}:42:500")
    );
    assert_eq!(first.source, "cover_art_archive_release");
    assert!(first.source_is_exact_release);
    assert!(first.approved && first.primary);
    assert_eq!(
        first.locator,
        "https://coverartarchive.org/release/abc/front-500.jpg"
    );
    assert_eq!(first.image_types, vec![coverart::ImageType::Front]);
    let second = &candidates[1];
    assert_eq!(
        second.image_types,
        vec![coverart::ImageType::Back, coverart::ImageType::Spine]
    );
    let request = &fake.seen()[0];
    assert_eq!(
        request.header("User-Agent"),
        Some(droppedneedle::http_client::USER_AGENT)
    );
}

#[tokio::test]
async fn caa_release_group_is_labelled_fallback() {
    let fake = Arc::new(FakeCaa::new());
    let url = caa_listing_url("release-group", GROUP_MBID);
    fake.script(
        &url,
        Ok(coverart::RawResponse::new(200, vec![], caa_listing_body())),
    );
    let client = fast_caa(fake);
    let candidates = client
        .list_artwork(
            coverart::EntityKind::ReleaseGroup,
            GROUP_MBID,
            coverart::DownloadSize::Full,
        )
        .await
        .expect("group listing decodes");
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].source, "cover_art_archive_release_group");
    assert!(!candidates[0].source_is_exact_release);
    assert_eq!(
        candidates[0].locator,
        "https://coverartarchive.org/release/abc/front.png"
    );
}

#[test]
fn caa_artwork_url_upgrade_and_rejection() {
    assert_eq!(
        coverart::upgrade_artwork_url("http://coverartarchive.org/release/abc/front.png"),
        Ok("https://coverartarchive.org/release/abc/front.png".to_owned())
    );
    assert_eq!(
        coverart::upgrade_artwork_url("https://coverartarchive.org:443/a/b.jpg?x=1#frag"),
        Ok("https://coverartarchive.org/a/b.jpg?x=1".to_owned())
    );
    for bad in [
        "https://evil.example/a.jpg",
        "https://coverartarchive.org.evil.example/a.jpg",
        "ftp://coverartarchive.org/a.jpg",
        "https://user@coverartarchive.org/a.jpg",
        "https://coverartarchive.org:8443/a.jpg",
        "https://coverartarchive.org",
        "not-a-url",
    ] {
        assert_eq!(
            coverart::upgrade_artwork_url(bad),
            Err(coverart::CaaError::RejectedUrl),
            "{bad}"
        );
    }
}

#[test]
fn caa_type_classification_merges_flags_and_labels() {
    let back_spine = vec!["Back".to_owned(), "Spine".to_owned()];
    assert_eq!(
        coverart::classify_image_types(&back_spine, true, false),
        vec![
            coverart::ImageType::Front,
            coverart::ImageType::Back,
            coverart::ImageType::Spine
        ]
    );
    assert_eq!(
        coverart::classify_image_types(&["Front".to_owned()], true, false),
        vec![coverart::ImageType::Front]
    );
    assert_eq!(
        coverart::classify_image_types(&[], false, false),
        vec![coverart::ImageType::Other]
    );
    assert_eq!(
        coverart::classify_image_types(&["Hologram".to_owned()], false, false),
        vec![coverart::ImageType::Other]
    );
    for (label, expected) in [
        ("Booklet", coverart::ImageType::Booklet),
        ("Medium", coverart::ImageType::Medium),
        ("Tray", coverart::ImageType::Tray),
        ("Obi", coverart::ImageType::Obi),
        ("Track", coverart::ImageType::Track),
    ] {
        assert_eq!(
            coverart::classify_image_types(&[label.to_owned()], false, false),
            vec![expected],
            "{label}"
        );
    }
    assert_eq!(
        coverart::classify_image_types(&[], false, true),
        vec![coverart::ImageType::Back]
    );
}

#[tokio::test]
async fn caa_missing_is_empty_while_failures_are_errors() {
    let fake = Arc::new(FakeCaa::new());
    let missing = "00000000-0000-0000-0000-000000000001";
    fake.script(
        &caa_listing_url("release", missing),
        Ok(coverart::RawResponse::new(404, vec![], Vec::new())),
    );
    fake.script(
        &caa_listing_url("release", RELEASE_MBID),
        Ok(coverart::RawResponse::new(500, vec![], Vec::new())),
    );
    let client = fast_caa(fake.clone());
    let candidates = client
        .list_artwork(
            coverart::EntityKind::Release,
            missing,
            coverart::DownloadSize::Full,
        )
        .await
        .expect("404 is authoritative emptiness");
    assert!(candidates.is_empty());
    let error = client
        .list_artwork(
            coverart::EntityKind::Release,
            RELEASE_MBID,
            coverart::DownloadSize::Full,
        )
        .await
        .expect_err("500 is a provider failure, not empty artwork");
    assert!(
        matches!(error, coverart::CaaError::Unavailable(_)),
        "got {error:?}"
    );
    let before = fake.seen().len();
    let error = client
        .list_artwork(
            coverart::EntityKind::Release,
            "nope",
            coverart::DownloadSize::Full,
        )
        .await
        .expect_err("malformed mbid fails locally");
    assert!(
        matches!(error, coverart::CaaError::InvalidMbid(_)),
        "got {error:?}"
    );
    assert_eq!(fake.seen().len(), before, "malformed mbid sends nothing");
}

#[tokio::test]
async fn caa_size_selection_falls_back_to_original() {
    let fake = Arc::new(FakeCaa::new());
    fake.script(
        &caa_listing_url("release", RELEASE_MBID),
        Ok(coverart::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "images": [
                    {"id": 1, "image": "http://coverartarchive.org/r/1.png", "thumbnails": {}},
                    {"id": 2, "image": "", "thumbnails": {}},
                    {"id": 3, "image": "http://coverartarchive.org/r/3.png",
                     "thumbnails": {"250": "http://coverartarchive.org/r/3-250.jpg"}}
                ]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = fast_caa(fake);
    let at_250 = client
        .list_artwork(
            coverart::EntityKind::Release,
            RELEASE_MBID,
            coverart::DownloadSize::Size250,
        )
        .await
        .expect("250 listing decodes");
    assert_eq!(at_250.len(), 2, "the empty-url image is skipped");
    assert_eq!(at_250[0].locator, "https://coverartarchive.org/r/1.png");
    assert_eq!(at_250[1].locator, "https://coverartarchive.org/r/3-250.jpg");

    let fake = Arc::new(FakeCaa::new());
    fake.script(
        &caa_listing_url("release", RELEASE_MBID),
        Ok(coverart::RawResponse::new(
            200,
            vec![],
            serde_json::json!({
                "images": [
                    {"id": 7, "image": "http://coverartarchive.org/r/7.png",
                     "thumbnails": {"1200": "http://coverartarchive.org/r/7-1200.jpg"}}
                ]
            })
            .to_string()
            .into_bytes(),
        )),
    );
    let client = fast_caa(fake);
    let at_1200 = client
        .list_artwork(
            coverart::EntityKind::Release,
            RELEASE_MBID,
            coverart::DownloadSize::Size1200,
        )
        .await
        .expect("1200 listing decodes");
    assert_eq!(
        at_1200[0].candidate_id,
        format!("caa:release:{RELEASE_MBID}:7:1200")
    );
    assert_eq!(
        at_1200[0].locator,
        "https://coverartarchive.org/r/7-1200.jpg"
    );
}

#[tokio::test]
async fn caa_backoff_retries_once_then_surfaces_rate_limit() {
    let fake = Arc::new(FakeCaa::new());
    let url = caa_listing_url("release", RELEASE_MBID);
    fake.script(
        &url,
        Ok(coverart::RawResponse::new(
            429,
            vec![("Retry-After", "1")],
            Vec::new(),
        )),
    );
    fake.script(
        &url,
        Ok(coverart::RawResponse::new(
            200,
            vec![],
            br#"{"images": []}"#.to_vec(),
        )),
    );
    let client = fast_caa(fake.clone());
    let candidates = client
        .list_artwork(
            coverart::EntityKind::Release,
            RELEASE_MBID,
            coverart::DownloadSize::Full,
        )
        .await
        .expect("retry after backoff succeeds");
    assert!(candidates.is_empty());
    assert_eq!(fake.seen().len(), 2);

    let fake = Arc::new(FakeCaa::new());
    fake.script(
        &url,
        Ok(coverart::RawResponse::new(503, vec![], Vec::new())),
    );
    fake.script(
        &url,
        Ok(coverart::RawResponse::new(
            503,
            vec![("Retry-After", "2")],
            Vec::new(),
        )),
    );
    let client = fast_caa(fake);
    let error = client
        .list_artwork(
            coverart::EntityKind::Release,
            RELEASE_MBID,
            coverart::DownloadSize::Full,
        )
        .await
        .expect_err("second 503 surfaces");
    match error {
        coverart::CaaError::RateLimited { retry_after_secs } => {
            assert_eq!(retry_after_secs, Some(2.0));
        }
        other => panic!("got {other:?}"),
    }
}

#[tokio::test]
async fn caa_download_sniffs_when_declarations_lie() {
    let png: Vec<u8> = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 16]].concat();
    assert!(png.len() >= 12);
    let fake = Arc::new(FakeCaa::new());
    let listing = caa_listing_url("release", RELEASE_MBID);
    fake.script(
        &listing,
        Ok(coverart::RawResponse::new(200, vec![], caa_listing_body())),
    );
    let client = fast_caa(fake.clone());
    let candidates = client
        .list_artwork(
            coverart::EntityKind::Release,
            RELEASE_MBID,
            coverart::DownloadSize::Full,
        )
        .await
        .expect("listing decodes");
    let front = candidates
        .into_iter()
        .find(|candidate| candidate.primary)
        .expect("front");

    fake.script(
        &front.locator,
        Ok(coverart::RawResponse::new(
            200,
            vec![("Content-Type", "text/html")],
            png.clone(),
        )),
    );
    let bytes = client
        .download_artwork(&front, 1024 * 1024)
        .await
        .expect("download decodes");
    assert_eq!(bytes.bytes, png);
    assert_eq!(bytes.content_type, "image/png", "bytes beat a lying header");

    fake.script(
        &front.locator,
        Ok(coverart::RawResponse::new(
            200,
            vec![("Content-Type", "image/jpeg; charset=binary")],
            png.clone(),
        )),
    );
    let bytes = client
        .download_artwork(&front, 1024 * 1024)
        .await
        .expect("download decodes");
    assert_eq!(bytes.content_type, "image/jpeg; charset=binary");

    fake.script(
        &front.locator,
        Ok(coverart::RawResponse::new(200, vec![], png.clone())),
    );
    let error = client
        .download_artwork(&front, 4)
        .await
        .expect_err("over-limit download fails");
    assert!(
        matches!(error, coverart::CaaError::Contract(_)),
        "got {error:?}"
    );

    let foreign = coverart::ArtworkCandidate {
        source: "someone-else".to_owned(),
        ..front.clone()
    };
    let error = client
        .download_artwork(&foreign, 1024)
        .await
        .expect_err("foreign source refused");
    assert!(
        matches!(error, coverart::CaaError::Contract(_)),
        "got {error:?}"
    );
}

#[test]
fn sniff_accepts_raster_only() {
    let jpeg = [b"\xff\xd8\xff".as_slice(), &[0u8; 16]].concat();
    assert_eq!(
        coverart::sniff_image_content_type(&jpeg),
        Some("image/jpeg")
    );
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 16]].concat();
    assert_eq!(coverart::sniff_image_content_type(&png), Some("image/png"));
    let gif = [b"GIF89a".as_slice(), &[0u8; 16]].concat();
    assert_eq!(coverart::sniff_image_content_type(&gif), Some("image/gif"));
    let webp = [b"RIFFxxxxWEBP".as_slice(), &[0u8; 16]].concat();
    assert_eq!(
        coverart::sniff_image_content_type(&webp),
        Some("image/webp")
    );
    assert_eq!(coverart::sniff_image_content_type(b"<svg></svg>!!!!"), None);
    assert_eq!(coverart::sniff_image_content_type(b"short"), None);
}

#[test]
fn production_adapters_build_without_touching_the_network() {
    assert!(musicbrainz::ReqwestMbTransport::build().is_ok());
    assert!(coverart::ReqwestCaaTransport::build().is_ok());
}
