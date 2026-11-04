//! The AcoustID, Last.fm and ListenBrainz clients against the scripted
//! provider wire: what goes out on the wire, what the answers that feed
//! identification mean, and credential handling. What each status and error
//! code becomes is pinned in `providers_core`; the decode of each recorded
//! answer in `providers_models`.

use droppedneedle_providers::executor::{Body, Method, Scripted, ScriptedWire, Sent};
use droppedneedle_providers::{ProviderEndpoints, ProviderError, Providers, RequestPriority};
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

/// What goes out to AcoustID (the form fields, twenty prints per batch, no
/// call without a key) and the recording and release ids read back from the
/// recorded answers. These ids feed identification.
#[tokio::test]
async fn acoustid_requests_and_recorded_answers_map_to_recordings() {
    // Twenty-five prints go out as two batch requests (20 + 5); answers map
    // back by index, and every recording above 0.5 is kept.
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
        .await
        .expect("batch: answered");
    let sent = wire.sent();
    assert_eq!(sent.len(), 2, "batch");
    assert_eq!(form_value(&sent[0], "batch").as_deref(), Some("1"), "batch");
    assert_eq!(
        form_value(&sent[0], "fingerprint.19").as_deref(),
        Some("print-19"),
        "batch"
    );
    assert_eq!(form_value(&sent[0], "fingerprint.20"), None, "batch");
    assert_eq!(
        form_value(&sent[1], "fingerprint.4").as_deref(),
        Some("print-24"),
        "batch"
    );
    assert_eq!(
        form_value(&sent[1], "duration.4").as_deref(),
        Some("200"),
        "batch"
    );
    assert_eq!(found.len(), 25, "batch");
    let last = &found[&24];
    assert_eq!(last.recording_ids, vec!["rec-4", "rec-4-single"], "batch");
    assert_eq!(last.release_ids, vec!["rel-1"], "batch");
    // An empty key never dials out.
    assert!(
        providers
            .acoustid
            .lookup_batch("", &queries, PRIORITY)
            .await
            .expect("no key hears nothing")
            .is_empty(),
        "batch without a key"
    );
    assert_eq!(wire.sent().len(), 2, "batch without a key sends nothing");

    // The recorded batch answer decodes with AcoustID's own track id and
    // score, and identification reads the same recording and releases.
    let (_, providers) =
        acoustid_providers([bytes_answer(200, acoustid_fixture("batch_lookup.json"))]);
    let outcome = providers
        .acoustid
        .lookup_batch_request("key", &one_query("print"), PRIORITY)
        .await;
    let acoustid::BatchOutcome::Answered(entries) = outcome else {
        panic!("recorded batch: expected an answer, got {outcome:?}");
    };
    assert_eq!(entries.len(), 1, "recorded batch");
    let (index, results) = &entries[0];
    assert_eq!(*index, 0, "recorded batch");
    assert_eq!(results.len(), 1, "recorded batch");
    assert_eq!(
        results[0].id, "a2558b28-8f9d-44ca-b97b-f800ff935a1b",
        "recorded batch"
    );
    assert_eq!(results[0].score(), 0.8158072, "recorded batch");
    let recordings = results[0].recordings();
    assert_eq!(recordings.len(), 1, "recorded batch");
    assert_eq!(
        recordings[0].id, "fe000616-c72c-43d4-8cba-a4083e99636b",
        "recorded batch"
    );
    assert_eq!(
        recordings[0].releases.as_deref().map(<[_]>::len),
        Some(43),
        "recorded batch"
    );
    let found = providers
        .acoustid
        .lookup_batch("key", &one_query("print"), PRIORITY)
        .await
        .expect("recorded batch: answered");
    assert_eq!(
        found[&0].recording_ids,
        vec!["fe000616-c72c-43d4-8cba-a4083e99636b"],
        "recorded batch"
    );
    assert_eq!(found[&0].release_ids.len(), 43, "recorded batch");
}

/// An AcoustID answer that could not be had is never "heard nothing". One
/// request AcoustID does not serve fails the whole lookup; a print it cannot
/// read is steady, not an outage: left out of an answer or refused with its
/// code, it is absent and the other files still answer. Only error code 4 is
/// a refused key, which reads as no key; an outage, a redirect, a bare 401, a
/// 400 naming another code, a 429 or a result without its id is unavailable,
/// so the work waits.
#[tokio::test(start_paused = true)]
async fn acoustid_never_reads_an_unanswered_print_as_heard_nothing() {
    use acoustid::{BatchOutcome, Unanswered};

    let prints: Vec<String> = (0..25).map(|index| format!("print-{index}")).collect();
    let queries: Vec<acoustid::BatchQuery<'_>> = prints
        .iter()
        .map(|print| acoustid::BatchQuery {
            fingerprint: print,
            duration_secs: 200,
        })
        .collect();
    let error = |code: i64| {
        json_answer(
            400,
            &serde_json::json!({"status": "error", "error": {"code": code, "message": "x"}}),
        )
    };
    let mut outcomes = Vec::new();
    for second in [
        error(2),
        error(3),
        json_answer(200, &batch_payload(4)),
        error(4),
    ] {
        let (_, providers) = acoustid_providers([json_answer(200, &batch_payload(20)), second]);
        let found = providers
            .acoustid
            .lookup_batch("key", &queries, PRIORITY)
            .await;
        outcomes.push(found.map(|found| found.len()));
    }
    assert_eq!(
        outcomes,
        [Err(Unanswered::Code(2)), Ok(20), Ok(24), Ok(0)],
        "second batch answered with code 2, code 3, a short answer, code 4"
    );

    let lookup = |answer: Scripted| async move {
        let (_, providers) = acoustid_providers([answer]);
        providers
            .acoustid
            .lookup_batch_request("key", &one_query("print"), PRIORITY)
            .await
    };
    let unavailable = BatchOutcome::Unavailable(Unanswered::Failed);
    assert_eq!(
        lookup(bytes_answer(400, acoustid_fixture("invalid_key.json"))).await,
        BatchOutcome::KeyRefused,
        "recorded invalid key"
    );
    assert_eq!(
        lookup(bytes_answer(503, Vec::new())).await,
        unavailable,
        "503"
    );
    let moved = Scripted::Answer {
        status: 302,
        headers: vec![(
            "location".to_owned(),
            "https://api.acoustid.org/elsewhere".to_owned(),
        )],
        body: Vec::new(),
    };
    assert_eq!(lookup(moved).await, unavailable, "redirect");
    assert_eq!(
        lookup(bytes_answer(401, Vec::new())).await,
        unavailable,
        "bare 401"
    );
    let other_code = serde_json::json!({
        "status": "error",
        "error": {"code": 3, "message": "invalid fingerprint"}
    });
    assert_eq!(
        lookup(json_answer(400, &other_code)).await,
        BatchOutcome::Unavailable(Unanswered::Code(3)),
        "400 naming code 3"
    );
    let limited = Scripted::Answer {
        status: 429,
        headers: vec![("retry-after".to_owned(), "30".to_owned())],
        body: Vec::new(),
    };
    assert_eq!(
        lookup(limited).await,
        BatchOutcome::Unavailable(Unanswered::Paused),
        "429"
    );
    let no_id = serde_json::json!({
        "status": "ok",
        "fingerprints": [{"index": 0, "results": [
            {"score": 0.9, "recordings": [{"id": "fe000616-c72c-43d4-8cba-a4083e99636b"}]}
        ]}]
    });
    assert_eq!(
        lookup(json_answer(200, &no_id)).await,
        unavailable,
        "result without its id"
    );
    let (wire, providers) = acoustid_providers([]);
    assert_eq!(
        providers
            .acoustid
            .lookup_batch_request("", &one_query("print"), PRIORITY)
            .await,
        BatchOutcome::NoKey,
        "no key"
    );
    assert!(wire.sent().is_empty(), "no key sends nothing");
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

/// Each user's key and signature go out on their own call: a token request
/// is signed with the caller's secret and carries no session, and one client
/// serving two users sends each call with its own key and stores neither.
#[tokio::test]
async fn lastfm_calls_carry_the_callers_own_credentials() {
    let (wire, providers) = lastfm_providers(&serde_json::json!({"token": "approve-me"}));
    let token = providers
        .lastfm
        .request_token(&lastfm_creds(), PRIORITY)
        .await
        .expect("token found");
    assert_eq!(token.token, "approve-me", "token request");
    let sent = wire.sent();
    assert_eq!(sent.len(), 1, "token request");
    assert_eq!(
        sent[0].query("method").as_deref(),
        Some("auth.getToken"),
        "token request"
    );
    assert_eq!(
        sent[0].query("api_key").as_deref(),
        Some("user-key"),
        "token request"
    );
    assert_eq!(
        sent[0].query("format").as_deref(),
        Some("json"),
        "token request"
    );
    assert_eq!(
        sent[0].query("sk"),
        None,
        "token request: no session rides yet"
    );
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
        "token request: wire signature matches the documented scheme"
    );

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
    assert_eq!(sent.len(), 2, "two users");
    assert_eq!(
        sent[0].query("api_key").as_deref(),
        Some("user-key"),
        "two users: the first"
    );
    assert_eq!(
        sent[1].query("api_key").as_deref(),
        Some("other-key"),
        "two users: the second"
    );
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

/// A ListenBrainz token goes out only in the `Authorization: Token` header,
/// beside the JSON headers, and a token that could break out of that header
/// (a newline, a space, or past the length cap) is refused before the wire.
#[tokio::test]
async fn listenbrainz_tokens_go_out_only_in_a_safe_header() {
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
    assert_eq!(sent.len(), 1, "safe token");
    let header = |name: &str| {
        sent[0]
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("accept"), Some("application/json"), "safe token");
    assert_eq!(
        header("content-type"),
        Some("application/json"),
        "safe token"
    );
    assert_eq!(
        header("authorization"),
        Some("Token lb-token"),
        "safe token: Token scheme, exactly like v2"
    );

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
            "unsafe token {token:?} on a read: {error:?}"
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
            "unsafe token {token:?} on a submit: {refused:?}"
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

/// An answer missing its field is never zero counts or a verdict: a
/// malformed popularity answer is a payload error, never an empty map a
/// caller could store as "no listens", and a credential check whose answer
/// lacks its verdict field is never "User found with 0 listens" or "Token
/// invalid".
#[tokio::test]
async fn listenbrainz_answers_missing_their_field_give_no_verdict() {
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
    let (ids, creds) = (["a".to_owned()], lb_anonymous());
    let ask = || {
        providers
            .listenbrainz
            .release_group_popularity(&ids, &creds, LB_PRIORITY)
    };
    let malformed = ask().await.expect_err("a malformed answer is no counts");
    assert!(
        matches!(malformed, ProviderError::Payload { .. }),
        "malformed popularity: {malformed:?}"
    );
    let counts = ask().await.expect("an empty list is legitimate");
    assert!(counts.is_empty(), "empty popularity list");

    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &lb_url("/1/user/listener/listen-count"),
        200,
        &[],
        r#"{"payload": {}}"#,
    );
    wire.answer(&lb_url("/1/validate-token"), 200, &[], r#"{"code": 200}"#);
    let providers = listenbrainz_providers(&wire);
    let user = providers
        .listenbrainz
        .verify("listener", "", LB_PRIORITY)
        .await;
    let token = providers
        .listenbrainz
        .verify("listener", "made-up-token", LB_PRIORITY)
        .await;
    for (case, outcome) in [("user check", user), ("token check", token)] {
        assert!(!outcome.valid, "{case}: {outcome:?}");
        assert_eq!(
            outcome.message, "ListenBrainz is temporarily unavailable. Try again shortly.",
            "{case}: {outcome:?}"
        );
    }
}
