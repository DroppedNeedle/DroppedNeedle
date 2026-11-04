//! MusicBrainz, the BrainzMash mirror and the Cover Art Archive over a
//! scripted wire: tolerant decode with required identity ids,
//! identity-critical failure, Retry-After, the canonical-id rules
//! (redirects, ranking without substitution), the mirror's path and URL
//! allowlists, and CAA URL and image-type checks.

use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_kernel::runtime_config::sections::{MbSourceMode, MusicBrainzSettings};
use droppedneedle_providers::executor::{Method, Scripted, ScriptedWire, Sent};
use droppedneedle_providers::{
    PayloadKind, ProviderEndpoints, ProviderError, Providers, RequestPriority, TransportKind,
    coverart, musicbrainz,
};

use std::sync::Arc;
use std::time::Duration;

const RELEASE_MBID: &str = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b";
const AVALON_MBID: &str = "0687c8a5-40a2-4a0c-bdc9-c1d80d94bef5";
const IMMUNITY_MBID: &str = "c85ad49c-6bfb-4bdc-96f8-f5f305a8799e";
const RETIRED_RECORDING_MBID: &str = "5224cfc7-b3bb-4008-a41b-21b168dc631f";
const CANONICAL_RECORDING_MBID: &str = "33ea7e59-3691-4414-92d3-6bce48d5d9cc";
const GROUP_MBID: &str = "dcff25f1-702d-3b5e-b0da-d48172e6e62a";

/// Scripted MusicBrainz: outcomes queue per URL (the last one repeating),
/// every request recorded. An unscripted URL gets no answer.
#[derive(Clone)]
struct FakeMb {
    wire: Arc<ScriptedWire>,
    providers: Arc<Providers>,
}

impl FakeMb {
    fn new() -> Self {
        let wire = Arc::new(ScriptedWire::new());
        let providers = Arc::new(Providers::over_wire(
            wire.clone(),
            Arc::new(SystemClock),
            ProviderEndpoints {
                musicbrainz: musicbrainz::MB_API_BASE.to_owned(),
                brainzmash: musicbrainz::BRAINZMASH_API_BASE.to_owned(),
                coverart: coverart::COVER_ART_ARCHIVE_BASE.to_owned(),
                ..ProviderEndpoints::closed()
            },
        ));
        Self { wire, providers }
    }

    /// Queue an answer for `url`, or a dead connection for `Err`.
    fn script(&self, url: &str, outcome: Result<Scripted, String>) {
        let outcome = outcome.unwrap_or(Scripted::Transport(TransportKind::Connect));
        self.wire.on(Method::Get, url, outcome);
    }

    fn seen(&self) -> Vec<Sent> {
        self.wire.sent()
    }

    /// Reads against `source` at user priority.
    fn client(&self, source: musicbrainz::MbSource) -> musicbrainz::MusicBrainzView {
        self.providers
            .musicbrainz
            .fixed(source, RequestPriority::UserInitiated)
    }
}

/// One scripted answer.
fn raw(status: u16, headers: Vec<(&str, &str)>, body: Vec<u8>) -> Scripted {
    Scripted::Answer {
        status,
        headers: headers
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        body,
    }
}

fn fast_mb() -> FakeMb {
    FakeMb::new()
}

fn official_client(fake: FakeMb) -> musicbrainz::MusicBrainzView {
    fake.client(musicbrainz::MbSource::official())
}

fn mb_url(path: &str) -> String {
    format!("{}{path}", musicbrainz::MB_API_BASE)
}

fn query_value(request: &Sent, key: &str) -> Option<String> {
    request.query(key)
}

/// Run `read` in a request scope and say whether it marked the request
/// degraded.
async fn degrades<T>(read: impl std::future::Future<Output = T>) -> (T, bool) {
    let (value, context) = droppedneedle_providers::scoped(read).await;
    (value, context.has_degradation())
}

// ---------------------------------------------------------------------------
// Contract: tolerant wire, strict identity, honest absence.
// ---------------------------------------------------------------------------

/// Absence, typed failure and degradation stay apart: a missing id or a
/// malformed mbid fails typed, a 404 is absence and degrades nothing, a
/// dead or failing provider fails an identity-critical read, and a 429 or
/// 503 surfaces its wait while a best-effort read degrades to absence.
#[tokio::test(start_paused = true)]
async fn musicbrainz_answers_keep_absence_failure_and_degradation_apart() {
    // Missing identity id: a lookup and a search hit both fail decoding.
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(200, vec![], br#"{"title": "id-less"}"#.to_vec())),
    );
    let client = official_client(fake.clone());
    let error = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("missing id: the lookup must fail");
    assert!(
        matches!(error, ProviderError::Payload { .. }),
        "missing id lookup: got {error:?}"
    );

    let fake = fast_mb();
    fake.script(
        &mb_url("/release"),
        Ok(raw(
            200,
            vec![],
            br#"{"count": 1, "offset": 0, "releases": [{"title": "id-less"}]}"#.to_vec(),
        )),
    );
    let client = official_client(fake.clone());
    let error = client
        .search_releases("x", "y", 5, musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("missing id: an id-less hit must fail");
    assert!(
        matches!(error, ProviderError::Payload { .. }),
        "missing id search: got {error:?}"
    );

    let error = client
        .resolve_recording_mbid("not-a-mbid", musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("malformed mbid must fail");
    assert!(
        matches!(error, ProviderError::Invalid { .. }),
        "malformed mbid: got {error:?}"
    );

    // Not found: None or an empty page, never an empty struct.
    let fake = fast_mb();
    let missing = "00000000-0000-0000-0000-000000000001";
    fake.script(
        &mb_url(&format!("/release/{missing}")),
        Ok(raw(404, vec![], br#"{"error":"Not Found"}"#.to_vec())),
    );
    fake.script(&mb_url("/release"), Ok(raw(404, vec![], Vec::new())));
    fake.script(&mb_url("/url"), Ok(raw(404, vec![], Vec::new())));
    let client = official_client(fake.clone());
    let ((), degraded) = degrades(async {
        let found = client
            .lookup_release(missing, &[], musicbrainz::Criticality::IdentityCritical)
            .await
            .expect("not found: 404 lookup resolves");
        assert!(
            found.is_none(),
            "not found: 404 is None, not an empty release"
        );
        let page = client
            .search_releases(
                "nothing",
                "nobody",
                5,
                musicbrainz::Criticality::IdentityCritical,
            )
            .await
            .expect("not found: 404 search resolves");
        assert!(
            page.items.is_empty() && page.count == 0,
            "not found: 404 search is an empty page"
        );
        let resolved = client
            .resolve_url(
                "https://www.discogs.com/release/1",
                &["release-rels"],
                musicbrainz::Criticality::IdentityCritical,
            )
            .await
            .expect("not found: 404 url resolves");
        assert!(
            resolved.relations.is_empty(),
            "not found: 404 url has no relations"
        );
    })
    .await;
    assert!(
        !degraded,
        "not found: definitive absence records no degradation"
    );

    // Dead provider: an identity-critical read fails typed.
    let critical = musicbrainz::Criticality::IdentityCritical;
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Err("connection refused".to_owned()),
    );
    let client = official_client(fake.clone());
    let error = client
        .lookup_release(RELEASE_MBID, &[], critical)
        .await
        .expect_err("dead provider fails identity-critical");
    assert!(
        matches!(error, ProviderError::Transport { .. }),
        "dead provider: got {error:?}"
    );

    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(500, vec![], Vec::new())),
    );
    let client = official_client(fake.clone());
    let error = client
        .lookup_release(RELEASE_MBID, &[], critical)
        .await
        .expect_err("500 fails identity-critical");
    assert!(
        matches!(error, ProviderError::Server { status: 500, .. }),
        "500: got {error:?}"
    );

    // Rate limit: the wait surfaces, a best-effort read degrades.
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(429, vec![("Retry-After", "5")], Vec::new())),
    );
    let client = official_client(fake.clone());
    let error = client
        .lookup_release(
            RELEASE_MBID,
            &[],
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect_err("429 fails identity-critical");
    match error {
        ProviderError::RateLimited { retry_after, .. } => assert_eq!(
            retry_after,
            Some(Duration::from_secs(5)),
            "429: Retry-After surfaces"
        ),
        other => panic!("429: got {other:?}"),
    }

    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(503, vec![], Vec::new())),
    );
    let client = official_client(fake.clone());
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
            ProviderError::RateLimited {
                retry_after: Some(wait),
                ..
            } if wait == Duration::from_secs(1)
        ),
        "503: got {error:?}"
    );

    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(429, vec![], Vec::new())),
    );
    let client = official_client(fake.clone());
    let (found, degraded) =
        degrades(client.lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort))
            .await;
    assert!(
        found.expect("429 degrades best-effort").is_none(),
        "best effort: 429 reads as absence"
    );
    assert!(degraded, "best effort: the request is marked degraded");
}

#[tokio::test(start_paused = true)]
async fn requests_carry_user_agent_fmt_and_sorted_includes() {
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(
            200,
            vec![],
            format!(r#"{{"id": "{RELEASE_MBID}"}}"#).into_bytes(),
        )),
    );
    let client = official_client(fake.clone());
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
        request
            .url
            .path()
            .ends_with(&format!("/release/{RELEASE_MBID}")),
        "mbid normalized: {}",
        request.url
    );
}

/// An answer past the cap is refused unread, even when the server never
/// says how long it is.
#[tokio::test]
async fn an_answer_over_the_cap_is_refused_unread() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut chunk = [0u8; 1024];
                while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => raw.extend_from_slice(&chunk[..read]),
                    }
                }
                // The client hangs up once it has seen too much, so a
                // failed write here is the refusal working.
                let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n";
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket
                    .write_all(&vec![b' '; musicbrainz::MAX_BODY_BYTES + 1])
                    .await;
            });
        }
    });
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
    let base = format!("http://{address}/ws/2");
    let providers = Providers::over_wire(
        Arc::new(droppedneedle_providers::ReqwestWire::new(&http)),
        Arc::new(SystemClock),
        ProviderEndpoints {
            musicbrainz: base.clone(),
            ..ProviderEndpoints::closed()
        },
    );
    let client = providers.musicbrainz.fixed(
        musicbrainz::MbSource::Official { base_url: base },
        RequestPriority::UserInitiated,
    );

    let error = client
        .lookup_release(
            RELEASE_MBID,
            &[],
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect_err("an oversized answer is refused");
    assert!(
        matches!(
            error,
            ProviderError::Payload {
                kind: PayloadKind::TooLarge,
                ..
            }
        ),
        "got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Quirks: every live-cited behavior, with its citation.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn explicit_nulls_decode_instead_of_failing() {
    // Live 2026-07-28 (null packaging pair, null catalogue number),
    // 2026-08-03 (null work type pair), 2026-08-15 (null status and
    // primary-type pairs): each pair is nullable independently.
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(
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
    let client = official_client(fake.clone());
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

    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(
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
    let client = official_client(fake.clone());
    let error = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("relation type-id stays required (live 2026-08-15)");
    assert!(
        matches!(error, ProviderError::Payload { .. }),
        "got {error:?}"
    );
}

#[test]
fn ranking_orders_one_recording_without_substitution() {
    // Live 2026-07-20: ranking only chooses among the release groups
    // already attached to a recording. Official Album beats Bootleg Live;
    // version-changing secondaries lose; dates break remaining ties.
    let group = |id: &str, primary: &str, secondaries: Vec<&str>| musicbrainz::ReleaseGroupRef {
        genres: Vec::new(),
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

#[tokio::test(start_paused = true)]
async fn exact_release_fails_closed_without_provider_group() {
    // Live 2026-08-10: Clairo _Immunity_ omits `release-group` without the
    // include; absence after that request never becomes an assumed group.
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{IMMUNITY_MBID}")),
        Ok(raw(
            200,
            vec![],
            format!(r#"{{"id": "{IMMUNITY_MBID}", "title": "Immunity"}}"#).into_bytes(),
        )),
    );
    let client = official_client(fake.clone());
    let error = client
        .lookup_exact_release(IMMUNITY_MBID, musicbrainz::Criticality::IdentityCritical)
        .await
        .expect_err("missing group fails closed");
    assert!(
        matches!(error, ProviderError::Payload { .. }),
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

    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{CLASSICAL_MBID}")),
        Ok(raw(
            200,
            vec![],
            include_bytes!("../fixtures/musicbrainz/release_lookup_classical.json").to_vec(),
        )),
    );
    fake.script(
        &mb_url(&format!("/release/{CLASSICAL_MBID}")),
        Err("reset".to_owned()),
    );
    let client = official_client(fake.clone());
    let found = client
        .lookup_exact_release(CLASSICAL_MBID, musicbrainz::Criticality::IdentityCritical)
        .await
        .expect("exact release with group decodes");
    let release = found.expect("present").entity;
    assert_eq!(
        release.release_group.expect("group").id,
        CLASSICAL_GROUP_MBID
    );
    let group = client
        .resolve_release_to_release_group(CLASSICAL_MBID, musicbrainz::Criticality::BestEffort)
        .await
        .expect("a dead follow-up degrades to absence");
    assert!(
        group.is_none(),
        "dead provider on best-effort resolves to None"
    );
}

#[tokio::test(start_paused = true)]
async fn retired_recording_redirect_proves_canonical_id() {
    // Live 2026-08-10: retired recording MBIDs 301 to a canonical
    // replacement; equivalence holds only after the lookup proves it.
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/recording/{RETIRED_RECORDING_MBID}")),
        Ok(raw(
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
        Ok(raw(
            200,
            vec![],
            include_bytes!("../fixtures/musicbrainz/recording_lookup_plain.json").to_vec(),
        )),
    );
    fake.script(
        &mb_url(&format!("/recording/{RETIRED_RECORDING_MBID}")),
        Err("reset".to_owned()),
    );
    let client = official_client(fake.clone());
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
        "a dead follow-up fails loudly, never silently"
    );
}

#[tokio::test(start_paused = true)]
async fn foreign_and_browse_redirects_are_rejected() {
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{RELEASE_MBID}")),
        Ok(raw(
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
        Ok(raw(
            301,
            vec![(
                "Location",
                &format!("{}/release-group/{GROUP_MBID}", musicbrainz::MB_API_BASE),
            )],
            Vec::new(),
        )),
    );
    let client = official_client(fake.clone());
    let error = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("foreign redirect rejected");
    assert!(
        matches!(error, ProviderError::Redirect { .. }),
        "got {error:?}"
    );
    let error = client
        .search_releases("x", "y", 5, musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("search redirect rejected");
    assert!(
        matches!(error, ProviderError::Redirect { .. }),
        "got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// BrainzMash mirror.
// ---------------------------------------------------------------------------

/// The mirror's path and request URL allowlists, good and bad inputs.
#[test]
fn brainzmash_paths_and_urls_follow_the_allowlist() {
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
            "path {bad} must not validate"
        );
    }

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
            "url {bad} must not validate"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn brainzmash_binding_invalid_fails_closed_without_wire() {
    let fake = fast_mb();
    let client = fake.client(musicbrainz::MbSource::BrainzMash {
        base_url: musicbrainz::BRAINZMASH_API_BASE.to_owned(),
        binding_valid: false,
    });
    let error = client
        .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
        .await
        .expect_err("invalid binding fails closed");
    assert!(
        matches!(error, ProviderError::NotConfigured { .. }),
        "got {error:?}"
    );
    assert!(
        fake.seen().is_empty(),
        "nothing is sent without a valid binding"
    );
}

#[tokio::test(start_paused = true)]
async fn the_source_is_read_at_every_request() {
    let fake = fast_mb();
    let mirror_base = "http://mirror.test/ws/2";
    for base in [musicbrainz::MB_API_BASE, mirror_base] {
        fake.script(
            &format!("{base}/release/{RELEASE_MBID}"),
            Ok(raw(404, vec![], Vec::new())),
        );
    }
    let dir = crate::common::ScratchDir::new("mb-live-source");
    let config = crate::common::config_store(&dir);
    let client = fake.providers.musicbrainz.live(
        musicbrainz::LiveSource::new(
            config.live(),
            musicbrainz::MB_API_BASE,
            "http://127.0.0.1:9",
        ),
        RequestPriority::UserInitiated,
    );
    for mode in [MbSourceMode::Official, MbSourceMode::Mirror] {
        config
            .save_blocking(MusicBrainzSettings {
                source_mode: mode,
                selected_source_mode: mode,
                api_url: mirror_base.to_owned(),
                ..MusicBrainzSettings::default()
            })
            .expect("source saves");
        client
            .lookup_release(RELEASE_MBID, &[], musicbrainz::Criticality::BestEffort)
            .await
            .expect("404 resolves");
    }
    let urls: Vec<String> = fake
        .seen()
        .into_iter()
        .map(|request| request.url.to_string())
        .collect();
    assert!(urls[0].starts_with(musicbrainz::MB_API_BASE), "{urls:?}");
    assert!(
        urls[1].starts_with(mirror_base),
        "a settings switch applies at once: {urls:?}"
    );
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

/// The release whose listing is recorded in `release_found.json`.
const CAA_RELEASE_MBID: &str = "30702389-5c67-4438-9ea0-2351c8de0f1d";

/// The recorded listing of `CAA_RELEASE_MBID`: two images typed Front, the
/// first one flagged front.
fn caa_listing_body() -> Vec<u8> {
    include_bytes!("../fixtures/coverartarchive/release_found.json").to_vec()
}

fn caa() -> FakeMb {
    FakeMb::new()
}

fn caa_answer(status: u16, headers: Vec<(&str, &str)>, body: Vec<u8>) -> Result<Scripted, String> {
    Ok(raw(status, headers, body))
}

const CAA_PRIORITY: RequestPriority = RequestPriority::BackgroundSync;

#[tokio::test(start_paused = true)]
async fn caa_listing_decodes_the_live_shape() {
    let fake = caa();
    let url = caa_listing_url("release", CAA_RELEASE_MBID);
    // The recorded listing with http addresses, as older listings named
    // them: a candidate still answers https.
    let body = String::from_utf8(caa_listing_body())
        .expect("the recorded listing is text")
        .replace("https://coverartarchive.org", "http://coverartarchive.org");
    fake.script(&url, caa_answer(200, vec![], body.into_bytes()));
    let candidates = fake
        .providers
        .coverart
        .list_artwork(
            coverart::EntityKind::Release,
            CAA_RELEASE_MBID,
            coverart::DownloadSize::Size500,
            CAA_PRIORITY,
        )
        .await
        .expect("listing decodes");
    assert_eq!(candidates.len(), 2);
    let first = &candidates[0];
    assert_eq!(
        first.candidate_id,
        format!("caa:release:{CAA_RELEASE_MBID}:43287476628:500")
    );
    assert_eq!(first.source, "cover_art_archive_release");
    assert!(first.source_is_exact_release);
    assert!(first.approved && first.primary);
    assert_eq!(
        first.locator,
        format!("https://coverartarchive.org/release/{CAA_RELEASE_MBID}/43287476628-500.jpg")
    );
    assert_eq!(first.image_types, vec![coverart::ImageType::Front]);
    let second = &candidates[1];
    assert!(
        second.approved && !second.primary,
        "the second image is typed Front but not flagged front"
    );
    assert_eq!(second.image_types, vec![coverart::ImageType::Front]);
    // No recorded listing holds other types; the mapping is checked on the
    // labels the archive sends with a back flag.
    assert_eq!(
        coverart::classify_image_types(&["Back".to_owned(), "Spine".to_owned()], false, true),
        vec![coverart::ImageType::Back, coverart::ImageType::Spine]
    );
}

/// Artwork addresses stay on the archive and are answered over https, and
/// only raster bytes count as an image (SVG and short blobs are refused).
#[test]
fn caa_urls_and_image_bytes_are_checked() {
    assert_eq!(
        coverart::upgrade_artwork_url("http://coverartarchive.org/release/abc/front.png")
            .as_deref(),
        Some("https://coverartarchive.org/release/abc/front.png")
    );
    assert_eq!(
        coverart::upgrade_artwork_url("https://coverartarchive.org:443/a/b.jpg?x=1#frag")
            .as_deref(),
        Some("https://coverartarchive.org/a/b.jpg?x=1")
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
        assert_eq!(coverart::upgrade_artwork_url(bad), None, "url {bad}");
    }

    let jpeg = [b"\xff\xd8\xff".as_slice(), &[0u8; 16]].concat();
    assert_eq!(
        coverart::sniff_image_content_type(&jpeg),
        Some("image/jpeg"),
        "sniff jpeg"
    );
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 16]].concat();
    assert_eq!(
        coverart::sniff_image_content_type(&png),
        Some("image/png"),
        "sniff png"
    );
    let gif = [b"GIF89a".as_slice(), &[0u8; 16]].concat();
    assert_eq!(
        coverart::sniff_image_content_type(&gif),
        Some("image/gif"),
        "sniff gif"
    );
    let webp = [b"RIFFxxxxWEBP".as_slice(), &[0u8; 16]].concat();
    assert_eq!(
        coverart::sniff_image_content_type(&webp),
        Some("image/webp"),
        "sniff webp"
    );
    assert_eq!(
        coverart::sniff_image_content_type(b"<svg></svg>!!!!"),
        None,
        "sniff svg"
    );
    assert_eq!(
        coverart::sniff_image_content_type(b"short"),
        None,
        "sniff short"
    );
}

/// A 404 is empty artwork, a 500 is a failure, a malformed mbid sends
/// nothing; a 429 is retried after its wait, and a second 503 surfaces its
/// wait.
#[tokio::test(start_paused = true)]
async fn caa_listing_tells_absence_from_failure() {
    let fake = caa();
    // The recorded 404 answer for a release with no artwork.
    let missing = "fd6ebf0e-85e4-4aa1-995a-07d5ac0c2147";
    fake.script(
        &caa_listing_url("release", missing),
        caa_answer(
            404,
            vec![],
            include_bytes!("../fixtures/coverartarchive/release_not_found.html").to_vec(),
        ),
    );
    // A 500 was never recorded; the client reads it by its status alone.
    fake.script(
        &caa_listing_url("release", RELEASE_MBID),
        caa_answer(500, vec![], Vec::new()),
    );
    let client = &fake.providers.coverart;
    let candidates = client
        .list_artwork(
            coverart::EntityKind::Release,
            missing,
            coverart::DownloadSize::Full,
            CAA_PRIORITY,
        )
        .await
        .expect("404 is authoritative emptiness");
    assert!(candidates.is_empty(), "404: no artwork");
    let error = client
        .list_artwork(
            coverart::EntityKind::Release,
            RELEASE_MBID,
            coverart::DownloadSize::Full,
            CAA_PRIORITY,
        )
        .await
        .expect_err("500 is a provider failure, not empty artwork");
    assert!(
        matches!(error, ProviderError::Server { status: 500, .. }),
        "500: got {error:?}"
    );
    let before = fake.seen().len();
    let error = client
        .list_artwork(
            coverart::EntityKind::Release,
            "nope",
            coverart::DownloadSize::Full,
            CAA_PRIORITY,
        )
        .await
        .expect_err("malformed mbid fails locally");
    assert!(
        matches!(error, ProviderError::Invalid { .. }),
        "malformed mbid: got {error:?}"
    );
    assert_eq!(fake.seen().len(), before, "malformed mbid sends nothing");

    // The archive never answered 429 or 503 while recording, so those
    // answers are written here; the retried answer is the recorded listing.
    let fake = caa();
    let url = caa_listing_url("release", CAA_RELEASE_MBID);
    fake.script(
        &url,
        caa_answer(429, vec![("Retry-After", "1")], Vec::new()),
    );
    fake.script(&url, caa_answer(200, vec![], caa_listing_body()));
    let candidates = fake
        .providers
        .coverart
        .list_artwork(
            coverart::EntityKind::Release,
            CAA_RELEASE_MBID,
            coverart::DownloadSize::Full,
            RequestPriority::UserInitiated,
        )
        .await
        .expect("retry after backoff succeeds");
    assert_eq!(candidates.len(), 2, "429: the retried listing is read");
    assert_eq!(fake.seen().len(), 2, "429: one retry");

    let fake = caa();
    fake.script(&url, caa_answer(503, vec![], Vec::new()));
    fake.script(
        &url,
        caa_answer(503, vec![("Retry-After", "2")], Vec::new()),
    );
    let error = fake
        .providers
        .coverart
        .list_artwork(
            coverart::EntityKind::Release,
            CAA_RELEASE_MBID,
            coverart::DownloadSize::Full,
            RequestPriority::UserInitiated,
        )
        .await
        .expect_err("second 503 surfaces");
    assert!(
        matches!(error, ProviderError::Unavailable { retry_after: Some(wait), .. } if wait == Duration::from_secs(2)),
        "503: got {error:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn caa_download_sniffs_when_declarations_lie() {
    let png: Vec<u8> = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 16]].concat();
    assert!(png.len() >= 12);
    let fake = caa();
    let listing = caa_listing_url("release", CAA_RELEASE_MBID);
    fake.script(&listing, caa_answer(200, vec![], caa_listing_body()));
    let client = &fake.providers.coverart;
    let candidates = client
        .list_artwork(
            coverart::EntityKind::Release,
            CAA_RELEASE_MBID,
            coverart::DownloadSize::Full,
            CAA_PRIORITY,
        )
        .await
        .expect("listing decodes");
    let front = candidates
        .into_iter()
        .find(|candidate| candidate.primary)
        .expect("front");

    fake.script(
        &front.locator,
        caa_answer(200, vec![("Content-Type", "text/html")], png.clone()),
    );
    fake.script(
        &front.locator,
        caa_answer(
            200,
            vec![("Content-Type", "image/jpeg; charset=binary")],
            png.clone(),
        ),
    );
    fake.script(&front.locator, caa_answer(200, vec![], png.clone()));
    let bytes = client
        .download_artwork(&front, 1024 * 1024, CAA_PRIORITY)
        .await
        .expect("download decodes");
    assert_eq!(bytes.bytes, png);
    assert_eq!(bytes.content_type, "image/png", "bytes beat a lying header");

    let bytes = client
        .download_artwork(&front, 1024 * 1024, CAA_PRIORITY)
        .await
        .expect("download decodes");
    assert_eq!(bytes.content_type, "image/jpeg; charset=binary");

    let error = client
        .download_artwork(&front, 4, CAA_PRIORITY)
        .await
        .expect_err("over-limit download fails");
    assert!(
        matches!(
            error,
            ProviderError::Payload {
                kind: PayloadKind::TooLarge,
                ..
            }
        ),
        "got {error:?}"
    );

    let foreign = coverart::ArtworkCandidate {
        source: "someone-else".to_owned(),
        ..front.clone()
    };
    let error = client
        .download_artwork(&foreign, 1024, CAA_PRIORITY)
        .await
        .expect_err("foreign source refused");
    assert!(
        matches!(error, ProviderError::Invalid { .. }),
        "got {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Recording and work credits (recorded BrainzMash answers, 2026-10-15).
// ---------------------------------------------------------------------------

const CLASSICAL_MBID: &str = "7ce871a0-34b2-40e0-a77e-39b1268f1f61";
const CLASSICAL_GROUP_MBID: &str = "94416ade-2459-4d90-af9e-0a8615e9177f";

fn relations_probe(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/tests/fixtures/musicbrainz/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).expect("probe reads")).expect("probe parses")
}

async fn lookup_relations(
    body: &serde_json::Value,
) -> droppedneedle_library::matching::model::ReleaseRelations {
    use droppedneedle_library::identify::sources::{RELATION_INCLUDES, relations_from_wire};
    let fake = fast_mb();
    fake.script(
        &mb_url(&format!("/release/{CLASSICAL_MBID}")),
        Ok(raw(200, vec![], serde_json::to_vec(body).expect("body"))),
    );
    let client = official_client(fake.clone());
    let lookup = client
        .lookup_release(
            CLASSICAL_MBID,
            &RELATION_INCLUDES,
            musicbrainz::Criticality::IdentityCritical,
        )
        .await
        .expect("lookup decodes")
        .expect("found");
    assert_eq!(
        query_value(&fake.seen()[0], "inc").as_deref(),
        Some("artist-rels+recording-level-rels+recordings+work-level-rels+work-rels")
    );
    relations_from_wire(&lookup.entity, 7)
}

/// The credit lookup decodes both recorded answers, takes the
/// recording's credits and those of each work it performs, and maps
/// MusicBrainz's types to tag roles as Picard does (a performing
/// orchestra is a performer, a writer its own role). A recording or work
/// that came without its relationship list is left out, never read as
/// "no credits".
#[tokio::test(start_paused = true)]
async fn credits_decode_from_the_recorded_answers() {
    let relations = lookup_relations(&relations_probe("release_relations_classical.json")).await;
    assert_eq!(relations.fetched_at_ms, 7);
    assert_eq!(relations.tracks.len(), 7);
    let first = &relations.tracks["ed4f20f0-61cf-439d-9c64-1d9acb043cbb"];
    assert_eq!(first.recording_id, "33ea7e59-3691-4414-92d3-6bce48d5d9cc");
    let roles: Vec<(&str, &str, &str)> = first
        .credits
        .iter()
        .map(|credit| {
            (
                credit.role.as_str(),
                credit.relation_type.as_str(),
                credit.name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        roles,
        vec![
            ("conductor", "conductor", "Herbert von Karajan"),
            ("performer", "instrument", "Glenn Gould"),
            (
                "performer",
                "performing orchestra",
                "Berliner Philharmoniker"
            ),
            ("composer", "composer", "Ludwig van Beethoven"),
            ("composer", "composer", "Ludwig van Beethoven"),
        ]
    );
    assert_eq!(first.credits[1].attributes, vec!["piano".to_owned()]);
    assert_eq!(first.credits[1].sort_name.as_deref(), Some("Gould, Glenn"));
    assert_eq!(first.works.len(), 1);
    assert!(
        relations.tracks["c1d6d454-853d-428f-b4d1-8ac6d9b46180"]
            .works
            .is_empty(),
        "the spoken track performs no work"
    );

    let pop = lookup_relations(&relations_probe("release_relations_pop.json")).await;
    let alejandro = &pop.tracks["aa235c36-5662-3709-ab97-06a1b4757d7e"];
    let names = |role: &str| -> Vec<&str> {
        alejandro
            .credits
            .iter()
            .filter(|credit| credit.role == role)
            .map(|credit| credit.name.as_str())
            .collect()
    };
    assert_eq!(names("producer"), vec!["Lady Gaga", "RedOne"]);
    assert_eq!(names("remixer"), vec!["The Sound of Arrows"]);
    assert_eq!(names("composer"), vec!["Lady Gaga", "RedOne"]);
    assert_eq!(names("lyricist"), vec!["Lady Gaga", "RedOne"]);
    let bad_romance = &pop.tracks["9ab08ef5-1ed9-3175-bcf4-aa232008a5f2"];
    let writers: Vec<(&str, &str, Option<&str>)> = bad_romance
        .credits
        .iter()
        .map(|credit| {
            (
                credit.role.as_str(),
                credit.name.as_str(),
                credit.credited.as_deref(),
            )
        })
        .filter(|(role, _, _)| *role == "writer")
        .collect();
    assert_eq!(
        writers,
        vec![
            ("writer", "Lady Gaga", Some("Stefani Germanotta")),
            ("writer", "RedOne", Some("Nadir Khayat")),
        ]
    );

    let mut body = relations_probe("release_relations_classical.json");
    let tracks = body["media"][0]["tracks"].as_array_mut().expect("tracks");
    tracks[0]["recording"]
        .as_object_mut()
        .expect("recording")
        .remove("relations");
    tracks[1]["recording"]["relations"]
        .as_array_mut()
        .expect("relations")
        .iter_mut()
        .filter(|relation| relation["type"] == "performance")
        .for_each(|relation| {
            relation["work"]
                .as_object_mut()
                .expect("work")
                .remove("relations");
        });
    let partial = lookup_relations(&body).await;
    assert_eq!(partial.tracks.len(), 5);
    assert!(
        !partial
            .tracks
            .contains_key("ed4f20f0-61cf-439d-9c64-1d9acb043cbb")
    );
    assert!(
        !partial
            .tracks
            .contains_key("c638fd85-2e96-4529-9e4e-61ec2085a16f")
    );
}

/// The recorded names lookups decode with their aliases (locale,
/// primary, sort name, type id; a null `primary` is not primary), keyed
/// by the credited artists; an artist with none has an empty list, and
/// relationship artists carry no alias list at all. The relations in the
/// same answer still parse, with the name each artist is credited as.
#[test]
fn names_decode_from_the_recorded_answers() {
    use droppedneedle_library::identify::sources::{names_from_wire, relations_from_wire};
    use droppedneedle_providers::musicbrainz::MbRelease;
    let decode = |name: &str| -> MbRelease {
        serde_json::from_value(relations_probe(name)).expect("probe decodes")
    };

    let sakamoto = decode("release_names_sakamoto.json");
    let names = names_from_wire(&sakamoto, 3);
    assert_eq!(names.fetched_at_ms, 3);
    let artist = &names.artists["a7f7df4a-77d8-4f12-8acd-5c60c93f4de8"];
    assert_eq!(artist.name, "坂本龍一");
    assert_eq!(artist.sort_name.as_deref(), Some("Sakamoto, Ryūichi"));
    let english = artist
        .aliases
        .as_ref()
        .expect("aliases fetched")
        .iter()
        .find(|alias| alias.locale.as_deref() == Some("en"))
        .expect("an English alias");
    assert_eq!(english.name, "Ryūichi Sakamoto");
    assert_eq!(english.sort_name.as_deref(), Some("Sakamoto, Ryūichi"));
    assert!(english.primary);
    assert_eq!(
        english.type_id.as_deref(),
        Some("894afba6-2816-3c24-8072-eadb66bd04bc")
    );
    assert_eq!(
        names.artists["7493b80f-de18-4f9c-a7bd-08d1ea5adcff"].aliases,
        Some(Vec::new()),
        "Takatani has no aliases"
    );

    let utada = names_from_wire(&decode("release_names_utada.json"), 0);
    assert_eq!(
        utada.artists.len(),
        4,
        "Utada and the three featured artists"
    );

    let classical = decode("release_names_classical.json");
    let names = names_from_wire(&classical, 0);
    let plain = names.artists["7002bf88-1269-4965-a772-4ba1e7a91eaa"]
        .aliases
        .as_ref()
        .expect("Gould's aliases");
    assert!(
        plain
            .iter()
            .any(|alias| !alias.primary && alias.locale.is_none()),
        "a null primary and locale decode"
    );
    let recording = classical.media[0].tracks[0]
        .recording
        .as_ref()
        .expect("recording");
    assert!(
        recording
            .relations
            .iter()
            .flatten()
            .filter_map(|relation| relation.artist.as_ref())
            .all(|artist| artist.aliases.is_none()),
        "relationship artists carry no aliases"
    );
    let relations = relations_from_wire(&classical, 0);
    assert_eq!(relations.tracks.len(), 7);
    assert!(
        relations
            .tracks
            .values()
            .flat_map(|track| &track.credits)
            .all(|credit| credit.credited.is_none()),
        "everyone is credited under their own name here"
    );
}
