//! The AudioDB, AcoustID, Last.fm and ListenBrainz clients against the
//! scripted provider wire: the decode of each real wire shape, what goes
//! out on the wire, and credential handling. What each status and error
//! code becomes is pinned in `providers_core`.

use droppedneedle_providers::executor::{Body, Method, Scripted, ScriptedWire, Sent};
use droppedneedle_providers::{
    PayloadKind, ProviderEndpoints, ProviderError, Providers, RequestPriority,
};
use droppedneedle_providers::{acoustid, audiodb, lastfm, listenbrainz};

use std::sync::Arc;

const PRIORITY: RequestPriority = RequestPriority::UserInitiated;

/// The providers over `wire`, unpaced, with AudioDB, AcoustID and Last.fm
/// at their real addresses.
fn audio_providers(wire: &Arc<ScriptedWire>) -> Providers {
    Providers::unpaced_at(
        wire.clone(),
        ProviderEndpoints {
            lastfm: lastfm::DEFAULT_BASE_URL.to_owned(),
            audiodb: audiodb::DEFAULT_BASE_URL.to_owned(),
            acoustid: acoustid::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    )
}

fn json_answer(status: u16, body: &serde_json::Value) -> Scripted {
    bytes_answer(status, body.to_string().into_bytes())
}

fn bytes_answer(status: u16, body: Vec<u8>) -> Scripted {
    Scripted::Answer {
        status,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body,
    }
}

/// One form field of a sent request.
fn form_value(sent: &Sent, name: &str) -> Option<String> {
    let Body::Form(form) = &sent.body else {
        return None;
    };
    form.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

// ---------------------------------------------------------------------------
// AudioDB
// ---------------------------------------------------------------------------

fn audiodb_url(endpoint: &str) -> String {
    format!("{}/123/{endpoint}", audiodb::DEFAULT_BASE_URL)
}

/// The recorded artist lookup for Radiohead, and the recorded no-match answer.
const AUDIODB_ARTIST_FOUND: &str = include_str!("../fixtures/audiodb/artist_mb_found.json");
const AUDIODB_NOT_FOUND: &str = include_str!("../fixtures/audiodb/artist_mb_not_found.json");

#[tokio::test]
async fn audiodb_artist_by_mbid_found_tolerates_unknown_fields() {
    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        Method::Get,
        &audiodb_url("artist-mb.php"),
        bytes_answer(200, AUDIODB_ARTIST_FOUND.as_bytes().to_vec()),
    );
    let providers = audio_providers(&wire);

    let artist = providers
        .audiodb
        .artist_by_mbid("", "a74b1b7f-71a5-4011-9441-d0b5e4122711", PRIORITY)
        .await
        .expect("answered")
        .expect("artist found");

    assert_eq!(artist.id_artist, "111418");
    assert_eq!(artist.name, "Radiohead");
    assert_eq!(
        artist.mbid.as_deref(),
        Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
    );
    assert_eq!(
        artist.thumb.as_deref(),
        Some("https://r2.theaudiodb.com/images/media/artist/thumb/35poth1668098458.jpg")
    );
    let sent = wire.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].query("i").as_deref(),
        Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
    );
}

/// Nothing found is `{"artists": null}`: no match, never a failure.
#[tokio::test]
async fn audiodb_not_found_is_no_match() {
    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        Method::Get,
        &audiodb_url("artist-mb.php"),
        bytes_answer(200, AUDIODB_NOT_FOUND.as_bytes().to_vec()),
    );
    let found = audio_providers(&wire)
        .audiodb
        .artist_by_mbid("", "00000000-0000-0000-0000-000000000000", PRIORITY)
        .await;
    assert!(matches!(found, Ok(None)), "{found:?}");
}

/// An entry missing its identity, or an answer that is not JSON, is a
/// payload failure: never an empty success, never "no match".
#[tokio::test]
async fn audiodb_unusable_answers_are_payload_failures() {
    for body in [
        serde_json::json!({"artists": [{"strArtistThumb": "https://x/y.jpg"}]})
            .to_string()
            .into_bytes(),
        b"not json".to_vec(),
    ] {
        let wire = Arc::new(ScriptedWire::new());
        wire.on(
            Method::Get,
            &audiodb_url("artist-mb.php"),
            bytes_answer(200, body),
        );
        let found = audio_providers(&wire)
            .audiodb
            .artist_by_mbid("", "mbid", PRIORITY)
            .await;
        assert!(
            matches!(
                found,
                Err(ProviderError::Payload {
                    kind: PayloadKind::Decode,
                    ..
                })
            ),
            "got {found:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// AcoustID
// ---------------------------------------------------------------------------

fn acoustid_url() -> String {
    format!("{}/v2/lookup", acoustid::DEFAULT_BASE_URL)
}

fn acoustid_providers(
    answers: impl IntoIterator<Item = Scripted>,
) -> (Arc<ScriptedWire>, Providers) {
    let wire = Arc::new(ScriptedWire::new());
    for answer in answers {
        wire.on(Method::Post, &acoustid_url(), answer);
    }
    let providers = audio_providers(&wire);
    (wire, providers)
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
    let (wire, providers) = acoustid_providers([json_answer(200, &acoustid_match_payload())]);

    let found = providers
        .acoustid
        .lookup("api-key", "fingerprint-body", 212, PRIORITY)
        .await
        .expect("answered")
        .expect("match found");

    assert_eq!(found.recording_id, "REC-1");
    assert_eq!(found.recording_ids, vec!["REC-1".to_owned()]);
    assert!((found.score - 0.95).abs() < f64::EPSILON);
    assert_eq!(found.title.as_deref(), Some("Weird Fishes"));
    assert_eq!(found.artist.as_deref(), Some("Radiohead"));
    assert_eq!(found.duration_secs, Some(318));
    assert_eq!(found.release_group_ids, vec!["rg1", "rg2", "rg3"]);
    let sent = wire.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, Method::Post);
    assert_eq!(form_value(&sent[0], "client").as_deref(), Some("api-key"));
    assert_eq!(
        form_value(&sent[0], "fingerprint").as_deref(),
        Some("fingerprint-body")
    );
    assert_eq!(form_value(&sent[0], "duration").as_deref(), Some("212"));
    assert_eq!(
        form_value(&sent[0], "meta").as_deref(),
        Some("recordings releasegroups")
    );
}

/// The batch answer for `count` prints: two strong recordings and one weak
/// one per print.
fn batch_payload(count: usize) -> serde_json::Value {
    let fingerprints: Vec<serde_json::Value> = (0..count)
        .map(|index| {
            serde_json::json!({
                "index": index,
                "results": [
                    {"score": 0.9, "id": "a", "recordings": [
                        {"id": format!("REC-{index}"), "releases": [{"id": "REL-1"}]},
                        {"id": format!("rec-{index}-single")}
                    ]},
                    {"score": 0.4, "id": "b", "recordings": [{"id": "rec-weak"}]}
                ]
            })
        })
        .collect();
    serde_json::json!({"status": "ok", "fingerprints": fingerprints})
}

/// Twenty-five prints go out as two batch requests (20 + 5); answers map
/// back by index, and every recording above 0.5 is kept.
#[tokio::test]
async fn acoustid_batch_posts_twenty_per_request() {
    let (wire, providers) = acoustid_providers([
        json_answer(200, &batch_payload(20)),
        json_answer(200, &batch_payload(5)),
    ]);
    let prints: Vec<String> = (0..25).map(|index| format!("print-{index}")).collect();
    let queries: Vec<acoustid::BatchQuery<'_>> = prints
        .iter()
        .map(|print| acoustid::BatchQuery {
            fingerprint: print,
            duration_secs: 200,
        })
        .collect();

    let found = providers
        .acoustid
        .lookup_batch("api-key", &queries, PRIORITY)
        .await;

    let sent = wire.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(form_value(&sent[0], "batch").as_deref(), Some("1"));
    assert_eq!(
        form_value(&sent[0], "fingerprint.19").as_deref(),
        Some("print-19")
    );
    assert_eq!(form_value(&sent[0], "fingerprint.20"), None);
    assert_eq!(
        form_value(&sent[1], "fingerprint.4").as_deref(),
        Some("print-24")
    );
    assert_eq!(form_value(&sent[1], "duration.4").as_deref(), Some("200"));
    assert_eq!(found.len(), 25);
    let last = &found[&24];
    assert_eq!(last.recording_ids, vec!["rec-4", "rec-4-single"]);
    assert_eq!(last.release_ids, vec!["rel-1"]);
    // An empty key never dials out.
    assert!(
        providers
            .acoustid
            .lookup_batch("", &queries, PRIORITY)
            .await
            .is_empty()
    );
    assert_eq!(wire.sent().len(), 2);
}

fn acoustid_fixture(name: &str) -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/acoustid")
        .join(name);
    std::fs::read(path).expect("fixture reads")
}

fn one_query(print: &str) -> [acoustid::BatchQuery<'_>; 1] {
    [acoustid::BatchQuery {
        fingerprint: print,
        duration_secs: 218,
    }]
}

/// The recorded batch answer decodes with AcoustID's own track id and
/// score, and identification reads the same recording and releases.
#[tokio::test]
async fn acoustid_recorded_batch_answer_decodes() {
    let (_, providers) =
        acoustid_providers([bytes_answer(200, acoustid_fixture("batch_lookup.json"))]);

    let outcome = providers
        .acoustid
        .lookup_batch_request("key", &one_query("print"), PRIORITY)
        .await;

    let acoustid::BatchOutcome::Answered(entries) = outcome else {
        panic!("expected an answer, got {outcome:?}");
    };
    assert_eq!(entries.len(), 1);
    let (index, results) = &entries[0];
    assert_eq!(*index, 0);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "a2558b28-8f9d-44ca-b97b-f800ff935a1b");
    assert_eq!(results[0].score(), 0.8158072);
    let recordings = results[0].recordings();
    assert_eq!(recordings.len(), 1);
    assert_eq!(recordings[0].id, "fe000616-c72c-43d4-8cba-a4083e99636b");
    assert_eq!(recordings[0].releases.as_deref().map(<[_]>::len), Some(43));

    let found = providers
        .acoustid
        .lookup_batch("key", &one_query("print"), PRIORITY)
        .await;
    assert_eq!(
        found[&0].recording_ids,
        vec!["fe000616-c72c-43d4-8cba-a4083e99636b"]
    );
    assert_eq!(found[&0].release_ids.len(), 43);
}

/// A bad key reads as rejected; an outage, a 429 (with its `Retry-After`)
/// or a result without its id as unavailable, so the batch waits.
#[tokio::test(start_paused = true)]
async fn acoustid_batch_tells_rejected_from_unavailable() {
    let lookup = |answer: Scripted| async move {
        let (_, providers) = acoustid_providers([answer]);
        providers
            .acoustid
            .lookup_batch_request("key", &one_query("print"), PRIORITY)
            .await
    };
    assert!(matches!(
        lookup(bytes_answer(400, acoustid_fixture("invalid_key.json"))).await,
        acoustid::BatchOutcome::Rejected { .. }
    ));
    match lookup(bytes_answer(503, Vec::new())).await {
        acoustid::BatchOutcome::Unavailable {
            retry_after_secs, ..
        } => assert_eq!(retry_after_secs, None),
        other => panic!("expected unavailable, got {other:?}"),
    }
    let limited = Scripted::Answer {
        status: 429,
        headers: vec![("retry-after".to_owned(), "30".to_owned())],
        body: Vec::new(),
    };
    match lookup(limited).await {
        acoustid::BatchOutcome::Unavailable {
            retry_after_secs, ..
        } => assert_eq!(retry_after_secs, Some(30.0)),
        other => panic!("expected unavailable, got {other:?}"),
    }
    let no_id = serde_json::json!({
        "status": "ok",
        "fingerprints": [{"index": 0, "results": [
            {"score": 0.9, "recordings": [{"id": "fe000616-c72c-43d4-8cba-a4083e99636b"}]}
        ]}]
    });
    assert!(matches!(
        lookup(json_answer(200, &no_id)).await,
        acoustid::BatchOutcome::Unavailable { .. }
    ));
}

#[tokio::test]
async fn acoustid_empty_and_low_score_results_are_no_match() {
    for body in [
        serde_json::json!({"status": "ok", "results": []}),
        serde_json::json!({"status": "ok"}),
        serde_json::json!({"status": "ok", "results": null}),
        serde_json::json!({"status": "ok", "results": [{"score": 0.5}]}),
        // A missing score reads as 0.0 and falls below the threshold (v2).
        serde_json::json!({"status": "ok", "results": [{"recordings": []}]}),
    ] {
        let (_, providers) = acoustid_providers([json_answer(200, &body)]);

        let found = providers
            .acoustid
            .lookup("key", "print", 200, PRIORITY)
            .await;

        assert!(
            matches!(found, Ok(None)),
            "no confident match means no match: {body}"
        );
    }
}

#[tokio::test]
async fn acoustid_confident_match_without_recording_id_is_never_no_match() {
    // v2 FAIL: confident audio, nothing to key the row on. It must not read
    // as a negative, so it is a contract failure (wiring routes it to
    // manual review, exactly like v2).
    let payload = serde_json::json!({
        "status": "ok",
        "results": [{"score": 0.99, "recordings": [{"title": "no id"}, 7, {"id": "  "}]}]
    });
    let (_, providers) = acoustid_providers([json_answer(200, &payload)]);

    let found = providers
        .acoustid
        .lookup("key", "print", 200, PRIORITY)
        .await;

    assert!(
        matches!(
            found,
            Err(ProviderError::Payload {
                kind: PayloadKind::Contract,
                ..
            })
        ),
        "got {found:?}"
    );
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

fn lastfm_providers(answer: &serde_json::Value) -> (Arc<ScriptedWire>, Providers) {
    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        Method::Get,
        lastfm::DEFAULT_BASE_URL,
        json_answer(200, answer),
    );
    let providers = audio_providers(&wire);
    (wire, providers)
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
    let (wire, providers) = lastfm_providers(&serde_json::json!({"token": "approve-me"}));

    let token = providers
        .lastfm
        .request_token(&lastfm_creds(), PRIORITY)
        .await
        .expect("token found");

    assert_eq!(token.token, "approve-me");
    let sent = wire.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].query("method").as_deref(), Some("auth.getToken"));
    assert_eq!(sent[0].query("api_key").as_deref(), Some("user-key"));
    assert_eq!(sent[0].query("format").as_deref(), Some("json"));
    assert_eq!(sent[0].query("sk"), None, "no session rides yet");
    // The signature recomputes from the sent parameters.
    let captured_sig = sent[0].query("api_sig").expect("call is signed");
    let unsigned: Vec<(String, String)> = sent[0]
        .url
        .query_pairs()
        .filter(|(key, _)| key != "api_sig")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(
        lastfm::api_sig(&unsigned, "user-secret"),
        captured_sig,
        "wire signature matches the documented scheme"
    );
}

fn lastfm_recording(body: &str) -> serde_json::Value {
    serde_json::from_str(body).expect("recording parses")
}

#[tokio::test]
async fn lastfm_artist_info_found_with_lenient_counts() {
    let (wire, providers) = lastfm_providers(&lastfm_recording(include_str!(
        "../fixtures/lastfm/artist_getinfo.json"
    )));

    let info = providers
        .lastfm
        .artist_info(&lastfm_creds(), "Radiohead", None, PRIORITY)
        .await
        .expect("answered")
        .expect("artist info found");

    assert_eq!(info.name, "Radiohead");
    assert_eq!(
        info.mbid.as_deref(),
        Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
    );
    assert_eq!(info.listeners, 8_489_583, "string counts parse");
    assert_eq!(info.playcount, 1_431_560_207);
    assert!(info.bio_summary.starts_with("Placeholder summary."));
    assert_eq!(info.tags.len(), 5);
    assert_eq!(info.similar.len(), 5);
    assert_eq!(info.similar[0].name, "Thom Yorke");
    assert_eq!(
        info.similar[0].mbid, None,
        "info's similar rows carry no id"
    );
    let sent = wire.sent();
    assert_eq!(sent[0].query("method").as_deref(), Some("artist.getInfo"));
    assert_eq!(sent[0].query("artist").as_deref(), Some("Radiohead"));
    assert_eq!(sent[0].query("api_sig"), None, "info reads are unsigned");
}

/// Last.fm's recorded error answers read as their typed outcome, whatever
/// status they came with: an unknown artist (error 6 in a 200) and an
/// unknown user (error 6 in a 404) are absence, a refused key (error 10 in
/// a 403) is unauthorized.
#[tokio::test]
async fn lastfm_recorded_errors_read_as_typed_outcomes() {
    let serve = |status: u16, body: &str| {
        let wire = Arc::new(ScriptedWire::new());
        wire.on(
            Method::Get,
            lastfm::DEFAULT_BASE_URL,
            json_answer(status, &lastfm_recording(body)),
        );
        audio_providers(&wire)
    };

    let unknown_artist = serve(
        200,
        include_str!("../fixtures/lastfm/artist_not_found.json"),
    )
    .lastfm
    .artist_info(&lastfm_creds(), "nobody", None, PRIORITY)
    .await;
    assert!(matches!(unknown_artist, Ok(None)), "{unknown_artist:?}");

    let unknown_user = serve(404, include_str!("../fixtures/lastfm/user_not_found.json"))
        .lastfm
        .user_top_albums(&lastfm_creds(), "nobody", "overall", 3, PRIORITY)
        .await;
    assert!(
        matches!(&unknown_user, Ok(rows) if rows.is_empty()),
        "{unknown_user:?}"
    );

    let refused = serve(403, include_str!("../fixtures/lastfm/invalid_key.json"))
        .lastfm
        .artist_info(&lastfm_creds(), "Radiohead", None, PRIORITY)
        .await;
    assert!(
        matches!(refused, Err(ProviderError::Unauthorized { .. })),
        "{refused:?}"
    );
}

#[tokio::test]
async fn lastfm_per_user_credentials_stay_isolated() {
    // One client, two users; each call carries its own key and the
    // client stores neither.
    let (wire, providers) = lastfm_providers(&serde_json::json!({"artist": {"name": "x"}}));
    let mut other = lastfm_creds();
    other.api_key = "other-key".to_owned();

    let _ = providers
        .lastfm
        .artist_info(&lastfm_creds(), "a", None, PRIORITY)
        .await;
    let _ = providers
        .lastfm
        .artist_info(&other, "a", None, PRIORITY)
        .await;

    let sent = wire.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].query("api_key").as_deref(), Some("user-key"));
    assert_eq!(sent[1].query("api_key").as_deref(), Some("other-key"));
}

// ---------------------------------------------------------------------------
// ListenBrainz
// ---------------------------------------------------------------------------

/// The providers over `wire`, with ListenBrainz at its real address.
fn listenbrainz_providers(
    wire: &Arc<droppedneedle_providers::executor::ScriptedWire>,
) -> droppedneedle_providers::Providers {
    droppedneedle_providers::Providers::unpaced_at(
        wire.clone(),
        droppedneedle_providers::ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            ..droppedneedle_providers::ProviderEndpoints::closed()
        },
    )
}

fn lb_url(path: &str) -> String {
    format!("{}{path}", listenbrainz::DEFAULT_BASE_URL)
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

const LB_PRIORITY: droppedneedle_providers::RequestPriority =
    droppedneedle_providers::RequestPriority::UserInitiated;

#[tokio::test]
async fn listenbrainz_required_headers_sent() {
    let wire = Arc::new(droppedneedle_providers::executor::ScriptedWire::new());
    wire.answer(
        &lb_url("/1/user/listener/listens"),
        200,
        &[],
        r#"{"payload": {"listens": []}}"#,
    );
    let providers = listenbrainz_providers(&wire);

    providers
        .listenbrainz
        .user_listens("listener", 25, &lb_authed(), LB_PRIORITY)
        .await
        .expect("listens answered");

    let sent = wire.sent();
    assert_eq!(sent.len(), 1);
    let header = |name: &str| {
        sent[0]
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("accept"), Some("application/json"));
    assert_eq!(header("content-type"), Some("application/json"));
    assert_eq!(
        header("authorization"),
        Some("Token lb-token"),
        "Token scheme, exactly like v2"
    );
}

#[tokio::test]
async fn listenbrainz_unsafe_tokens_rejected_without_wire() {
    let wire = Arc::new(droppedneedle_providers::executor::ScriptedWire::new());
    wire.answer(
        &lb_url("/1/user/listener/listens"),
        200,
        &[],
        r#"{"payload": {"listens": []}}"#,
    );
    let providers = listenbrainz_providers(&wire);
    for token in [
        "has\nnewline".to_owned(),
        "has space".to_owned(),
        "x".repeat(1025),
    ] {
        let creds = listenbrainz::ListenBrainzCredentials {
            username: Some("listener".to_owned()),
            user_token: Some(token.clone()),
        };

        let error = providers
            .listenbrainz
            .user_listens("listener", 25, &creds, LB_PRIORITY)
            .await
            .expect_err("an unsafe token is refused");
        assert!(
            matches!(
                error,
                droppedneedle_providers::ProviderError::Invalid { .. }
            ),
            "{error:?}"
        );
        let refused = providers
            .listenbrainz
            .submit_listens(&token, b"{}".to_vec(), LB_PRIORITY)
            .await
            .expect_err("an unsafe token is refused");
        assert!(
            matches!(
                refused,
                droppedneedle_providers::ProviderError::Invalid { .. }
            ),
            "{refused:?}"
        );
    }
    assert!(
        wire.sent().is_empty(),
        "rejected tokens never reach the wire"
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
    let wire = Arc::new(droppedneedle_providers::executor::ScriptedWire::new());
    wire.answer(
        &lb_url("/1/user/listener/listens"),
        200,
        &[],
        payload.to_string(),
    );
    let providers = listenbrainz_providers(&wire);

    let listens = providers
        .listenbrainz
        .user_listens("listener", 25, &lb_anonymous(), LB_PRIORITY)
        .await
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
    assert_eq!(wire.sent()[0].query("count").as_deref(), Some("25"));
}

#[tokio::test]
async fn listenbrainz_popularity_malformed_never_reads_as_zeros() {
    // v2's poisoning guard: a malformed answer returns partial results with
    // nothing written, never an authoritative empty map from an outage.
    let popularity = lb_url("/1/popularity/release-group");
    let wire = Arc::new(droppedneedle_providers::executor::ScriptedWire::new());
    wire.on(
        droppedneedle_providers::executor::Method::Post,
        &popularity,
        droppedneedle_providers::executor::Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: br#"{"oops": true}"#.to_vec(),
        },
    );
    wire.on(
        droppedneedle_providers::executor::Method::Post,
        &popularity,
        droppedneedle_providers::executor::Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: b"[]".to_vec(),
        },
    );
    let providers = listenbrainz_providers(&wire);

    for answer in [
        "malformed popularity stays soft",
        "empty list is legitimate",
    ] {
        let counts = providers
            .listenbrainz
            .release_group_popularity(&["a".to_owned()], &lb_anonymous(), LB_PRIORITY)
            .await
            .expect(answer);
        assert!(counts.is_empty(), "{answer}");
    }
}
