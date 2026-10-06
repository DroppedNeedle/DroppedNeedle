//! MusicBrainz, the BrainzMash mirror and the Cover Art Archive over a
//! scripted transport: tolerant decode with required identity ids,
//! identity-critical failure, Retry-After, the canonical-id rules
//! (redirects, ranking without substitution), the mirror's path and URL
//! allowlists, and CAA URL and image-type checks.

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

/// Scripted MusicBrainz transport: outcomes queue per request URL, every
/// request recorded. Unscripted URLs fail loudly so tests stay explicit.
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

/// Shared sink handle: the client records through this while the test
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

/// Reference-counted fake so client and test share one script.
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

/// Shared fake transport: the client and the test hold clones of one
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

fn unpaced() -> musicbrainz::MbPacing {
    musicbrainz::MbPacing::new(Arc::new(droppedneedle::providers::Providers::unpaced()))
}

fn official_client(
    fake: Arc<SharedFakeMb>,
    sink: Arc<VecSink>,
) -> musicbrainz::MusicBrainzClient<SharedMb, SharedSink> {
    musicbrainz::MusicBrainzClient::official(SharedMb(fake), unpaced()).with_sink(SharedSink(sink))
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
// Contract: tolerant wire, strict identity, honest absence.
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

// ---------------------------------------------------------------------------
// Quirks: every live-cited behavior, with its citation.
// ---------------------------------------------------------------------------

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
        disambiguation: None,
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

// ---------------------------------------------------------------------------
// BrainzMash mirror.
// ---------------------------------------------------------------------------

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
async fn brainzmash_binding_invalid_fails_closed_without_wire() {
    let (fake, sink) = fast_mb(vec![]);
    let client =
        musicbrainz::MusicBrainzClient::brainzmash(SharedMb(fake.clone()), false, unpaced())
            .with_sink(SharedSink(sink));
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
    let scheduler = musicbrainz::BrainzMashCooldown::default();
    assert!(scheduler.remaining().is_zero());
    let selected = scheduler.note_cooldown(Some(5.0));
    assert_eq!(selected, 5.0);
    assert!(!scheduler.remaining().is_zero());
    scheduler.note_success();
    assert!(scheduler.remaining().is_zero());

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

// ---------------------------------------------------------------------------
// Cover Art Archive (live provider boundary 2026-07-21).
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
