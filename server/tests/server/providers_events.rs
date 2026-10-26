//! Geocoding, Skiddle, Ticketmaster, YouTube and GitHub clients over a
//! scripted wire: the decode of each live wire shape, required identity
//! fields, rate limits, Ticketmaster's page cap, and the YouTube quota file
//! (reserved before HTTP, charged for dispatched calls, kept across
//! restarts).

use crate::common::ScratchDir;
use droppedneedle_providers::executor::{ScriptedWire, Sent};
use droppedneedle_providers::{PayloadKind, ProviderError, Providers, RequestPriority};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_kernel::clock::ManualClock;
use droppedneedle_providers::geocoding::DEFAULT_CITY_COUNT;
use droppedneedle_providers::youtube::{SearchKind, YouTubeClient, YouTubeSettings, YoutubeError};
use serde_json::json;

const USER: RequestPriority = RequestPriority::UserInitiated;
const ORIGIN: &str = "http://127.0.0.1:9";

// ---------------------------------------------------------------------------
// Scripted wire
// ---------------------------------------------------------------------------

/// The providers over a scripted wire, every address on the closed origin.
fn scripted() -> (Arc<ScriptedWire>, Providers) {
    let wire = Arc::new(ScriptedWire::new());
    let providers = Providers::unpaced(wire.clone());
    (wire, providers)
}

fn url(path: &str) -> String {
    format!("{ORIGIN}/{path}")
}

fn json_answer(wire: &ScriptedWire, path: &str, body: serde_json::Value) {
    wire.answer(&url(path), 200, &[], body.to_string());
}

fn sent_to(wire: &ScriptedWire, path: &str) -> Vec<Sent> {
    let target = url(path);
    wire.sent()
        .into_iter()
        .filter(|sent| {
            let mut bare = sent.url.clone();
            bare.set_query(None);
            bare.as_str() == target
        })
        .collect()
}

/// A not-yet-created quota path nested in a fresh scratch dir (so the
/// atomic write's parent-dir creation is exercised too). Keep the guard
/// alive for the test.
fn quota_path(name: &str) -> (ScratchDir, PathBuf) {
    let scratch = ScratchDir::new(&format!("youtube-{name}"));
    let path = scratch.join("quota/youtube_quota.json");
    (scratch, path)
}

fn quota_file_json(path: &PathBuf) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn enabled_settings(limit: u32) -> YouTubeSettings {
    YouTubeSettings {
        api_key: "secret".to_owned(),
        enabled: true,
        api_enabled: true,
        daily_quota_limit: limit,
    }
}

/// A clock at noon UTC on 2026-05-05.
fn may_fifth() -> Arc<ManualClock> {
    let noon = time::Date::from_calendar_date(2026, time::Month::May, 5)
        .unwrap()
        .midnight()
        .assume_utc()
        .unix_timestamp()
        + 12 * 3600;
    Arc::new(ManualClock::at(noon * 1000))
}

fn youtube(providers: &Providers, path: &Path, limit: u32) -> YouTubeClient {
    YouTubeClient::new(
        providers.youtube.clone(),
        path.to_path_buf(),
        enabled_settings(limit),
        may_fifth(),
        providers.cache.clone(),
    )
    .unwrap()
}

const SEARCH: &str = "youtube/youtube/v3/search";

// ---------------------------------------------------------------------------
// Geocoding
// ---------------------------------------------------------------------------

/// The recorded Liverpool answer: the first match is Liverpool, GB, and the
/// fields the client does not read (`id`, `population`, `postcodes`) decode
/// past silently.
#[tokio::test]
async fn geocoding_search_decodes_recorded_answer() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("geocoding"),
        200,
        &[],
        include_str!("../fixtures/geocoding/search_liverpool.json"),
    );

    let cities = providers
        .geocoding
        .search_cities("Liverpool", DEFAULT_CITY_COUNT, USER)
        .await
        .unwrap();

    assert_eq!(cities.len(), 8);
    let city = &cities[0];
    assert_eq!(city.name, "Liverpool");
    assert!((city.latitude - 53.41058).abs() < 1e-9);
    assert!((city.longitude - -2.97794).abs() < 1e-9);
    assert_eq!(city.country_code.as_deref(), Some("GB"));
    assert_eq!(city.country.as_deref(), Some("United Kingdom"));
    assert_eq!(city.admin1.as_deref(), Some("England"));
    let calls = sent_to(&wire, "geocoding");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].query("name").as_deref(), Some("Liverpool"));
    assert_eq!(
        calls[0].query("count"),
        Some(DEFAULT_CITY_COUNT.to_string())
    );
    assert_eq!(calls[0].query("language").as_deref(), Some("en"));
    assert_eq!(calls[0].query("format").as_deref(), Some("json"));
}

#[tokio::test]
async fn geocoding_429_maps_to_rate_limited() {
    let (wire, providers) = scripted();
    wire.answer(&url("geocoding"), 429, &[], "{}");

    assert!(matches!(
        providers
            .geocoding
            .search_cities("Liverpool", 8, USER)
            .await,
        Err(ProviderError::RateLimited { .. })
    ));
}

/// The geocoder knows no such place: the answer has no `results` key, which
/// is absence, not a failure.
#[tokio::test]
async fn geocoding_answer_without_results_is_absence() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("geocoding"),
        200,
        &[],
        include_str!("../fixtures/geocoding/search_no_results.json"),
    );

    let cities = providers
        .geocoding
        .search_cities("No Such Place For A Probe", DEFAULT_CITY_COUNT, USER)
        .await
        .unwrap();
    assert!(cities.is_empty());
}

/// A refused request answers HTTP 400 with `error` and `reason` and no
/// `results` key. It is a typed failure, never the empty list that means no
/// such place.
#[tokio::test]
async fn geocoding_refused_request_is_a_failure_not_absence() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("geocoding"),
        400,
        &[],
        include_str!("../fixtures/geocoding/search_bad_count.json"),
    );

    assert!(matches!(
        providers
            .geocoding
            .search_cities("Liverpool", DEFAULT_CITY_COUNT, USER)
            .await,
        Err(ProviderError::BadRequest { .. })
    ));
}

// ---------------------------------------------------------------------------
// Skiddle
// ---------------------------------------------------------------------------

/// A recorded events answer for one act: two gigs with lineups, `cancelled`
/// as the string `'0'`, and empty strings for absent ticket URLs and
/// reschedule dates.
#[tokio::test]
async fn skiddle_events_decode_recorded_answer() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("skiddle/events/search/"),
        200,
        &[],
        include_str!("../fixtures/skiddle/events_for_artist.json"),
    );

    let events = providers
        .skiddle
        .events_for_artist("k", "123487626", USER)
        .await
        .unwrap();

    assert_eq!(events.len(), 2);
    let event = &events[0];
    assert_eq!(event.id, "42612509");
    assert_eq!(event.date.as_deref(), Some("2026-10-30"));
    assert!(!event.is_cancelled());
    assert!(!event.is_rescheduled());
    assert_eq!(event.ticket_url.as_deref(), Some(""));
    assert_eq!(
        event.venue.as_ref().and_then(|venue| venue.town.as_deref()),
        Some("Whitstable")
    );
    assert_eq!(event.artists.len(), 3);
    assert_eq!(event.artists[0].artistid.as_deref(), Some("123487626"));
    let calls = sent_to(&wire, "skiddle/events/search/");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].query("a").as_deref(), Some("123487626"));
    assert_eq!(calls[0].query("description").as_deref(), Some("1"));
    assert_eq!(calls[0].query("api_key").as_deref(), Some("k"));
}

/// An empty results list is no match. An answer without Skiddle's error code,
/// or a success without its results, is a payload failure, not an empty list.
#[tokio::test]
async fn skiddle_answers_need_their_code_and_results() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("skiddle/artists/"),
        200,
        &[],
        include_str!("../fixtures/skiddle/artists_no_match.json"),
    );
    assert!(
        providers
            .skiddle
            .search_artists("k", "zzqxkvwnomatch", USER)
            .await
            .unwrap()
            .is_empty()
    );

    for body in [json!({"results": []}), json!({"error": 0})] {
        let (wire, providers) = scripted();
        json_answer(&wire, "skiddle/artists/", body.clone());
        assert!(
            matches!(
                providers.skiddle.search_artists("k", "x", USER).await,
                Err(ProviderError::Payload {
                    kind: PayloadKind::Contract,
                    ..
                })
            ),
            "{body}"
        );
    }
}

/// A key Skiddle does not know comes back as HTTP 400 with its error
/// envelope. The status decides the failure, so the envelope is never decoded.
#[tokio::test]
async fn skiddle_bad_key_is_a_bad_request() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("skiddle/artists/"),
        400,
        &[],
        include_str!("../fixtures/skiddle/artists_bad_key.json"),
    );

    assert!(matches!(
        providers.skiddle.search_artists("k", "Orbital", USER).await,
        Err(ProviderError::BadRequest { status: 400, .. })
    ));
}

/// The key check reads only the error code of a recorded answer.
#[tokio::test]
async fn skiddle_key_check_accepts_a_recorded_answer() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("skiddle/events/search/"),
        200,
        &[],
        include_str!("../fixtures/skiddle/events_key_check.json"),
    );

    providers.skiddle.check_key("k", USER).await.unwrap();
}

/// Skiddle's own failure envelope arrives on HTTP 200 with `error != 0`;
/// only its code is kept.
#[tokio::test]
async fn skiddle_error_envelope_on_http_200_raises() {
    let (wire, providers) = scripted();
    json_answer(
        &wire,
        "skiddle/artists/",
        json!({"error": 1, "errormessage": "bad key"}),
    );

    match providers.skiddle.search_artists("k", "x", USER).await {
        Err(ProviderError::Payload {
            kind: PayloadKind::Contract,
            message,
            ..
        }) => assert!(message.contains("error 1"), "{message}"),
        other => panic!("expected the envelope as a payload error, got {other:?}"),
    }
}

/// Required identity: a result without an id fails the decode instead of
/// materializing as an empty act.
#[tokio::test]
async fn skiddle_missing_id_fails_decode() {
    let (wire, providers) = scripted();
    json_answer(
        &wire,
        "skiddle/artists/",
        json!({"error": 0, "results": [{"name": "Nameless"}]}),
    );

    assert!(matches!(
        providers.skiddle.search_artists("k", "x", USER).await,
        Err(ProviderError::Payload {
            kind: PayloadKind::Decode,
            ..
        })
    ));
}

#[tokio::test]
async fn skiddle_429_maps_to_rate_limited() {
    let (wire, providers) = scripted();
    wire.answer(&url("skiddle/artists/"), 429, &[], "{}");

    assert!(matches!(
        providers.skiddle.search_artists("k", "x", USER).await,
        Err(ProviderError::RateLimited { .. })
    ));
}

// ---------------------------------------------------------------------------
// Ticketmaster
// ---------------------------------------------------------------------------

/// The recorded events answer for an act with 56 upcoming events: local and
/// absolute start, sale status, a venue with string coordinates, and the
/// MusicBrainz id of each lineup act.
#[tokio::test]
async fn ticketmaster_events_decode_recorded_answer() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("ticketmaster/events.json"),
        200,
        &[],
        include_str!("../fixtures/ticketmaster/events_fontaines_dc.json")
            .as_bytes()
            .to_vec(),
    );

    let events = providers
        .ticketmaster
        .events_for_attraction("k", "K8vZ9179LP7", USER)
        .await
        .unwrap();

    assert_eq!(events.len(), 56);
    let event = &events[0];
    assert_eq!(event.id, "ZG9rmIYnyZdk1k");
    assert_eq!(event.name, "Fontaines D.C.");
    assert_eq!(
        event.url.as_deref(),
        Some(
            "https://www.ticketmaster.it/biglietti/fontaines-dc-casalecchio-di-reno-bologna-26-10-2026/event/h5qsmyc9wmts"
        )
    );
    let dates = event.dates.as_ref().unwrap();
    assert_eq!(
        dates.start.as_ref().unwrap().local_date.as_deref(),
        Some("2026-10-26")
    );
    assert_eq!(
        dates.start.as_ref().unwrap().date_time.as_deref(),
        Some("2026-10-26T20:00:00Z")
    );
    assert_eq!(
        dates.status.as_ref().unwrap().code.as_deref(),
        Some("offsale")
    );
    let inner = event.embedded.as_ref().unwrap();
    let venue = &inner.venues[0];
    assert_eq!(venue.name.as_deref(), Some("Unipol Arena"));
    assert_eq!(
        venue.city.as_ref().unwrap().name.as_deref(),
        Some("Casalecchio di Reno (Bologna)")
    );
    assert_eq!(
        venue.country.as_ref().unwrap().country_code.as_deref(),
        Some("IT")
    );
    let location = venue.location.as_ref().unwrap();
    assert_eq!(location.latitude.as_deref(), Some("44.485673"));
    assert_eq!(location.longitude.as_deref(), Some("11.248983"));
    assert_eq!(
        inner.attractions[0].musicbrainz_ids(),
        ["fd87acc7-e0a0-4a45-bc2a-d2ab5c10be68"]
    );
    let calls = sent_to(&wire, "ticketmaster/events.json");
    assert_eq!(calls[0].query("apikey").as_deref(), Some("k"));
    assert_eq!(
        calls[0].query("attractionId").as_deref(),
        Some("K8vZ9179LP7")
    );
}

/// A result set deeper than the cap stops after three pages: no silent
/// fourth request.
#[tokio::test]
async fn ticketmaster_pagination_truncates_at_three_pages() {
    let (wire, providers) = scripted();
    for call in 0..5 {
        json_answer(
            &wire,
            "ticketmaster/events.json",
            json!({
                "_embedded": {"events": [{"id": format!("e{call}")}]},
                "page": {"totalPages": 99, "number": call},
            }),
        );
    }

    let events = providers
        .ticketmaster
        .events_for_attraction("k", "A1", USER)
        .await
        .unwrap();

    assert_eq!(events.len(), 3);
    let pages: Vec<Option<String>> = sent_to(&wire, "ticketmaster/events.json")
        .iter()
        .map(|sent| sent.query("page"))
        .collect();
    assert_eq!(
        pages,
        [
            Some("0".to_owned()),
            Some("1".to_owned()),
            Some("2".to_owned())
        ]
    );
}

/// The server's `Retry-After` hint travels on the rate-limit error.
#[tokio::test]
async fn ticketmaster_429_carries_retry_after_hint() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("ticketmaster/attractions.json"),
        429,
        &[("retry-after", "7")],
        "slow down",
    );

    match providers
        .ticketmaster
        .search_attractions("k", "x", USER)
        .await
    {
        Err(ProviderError::RateLimited { retry_after, .. }) => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(7)));
        }
        other => panic!("expected RateLimited error, got {other:?}"),
    }
}

/// Zero results are empty answers, not failures: the attractions search omits
/// `_embedded`, and the events answer has no pages.
#[tokio::test]
async fn ticketmaster_zero_results_are_empty_answers() {
    let (wire, providers) = scripted();
    wire.answer(
        &url("ticketmaster/attractions.json"),
        200,
        &[],
        include_str!("../fixtures/ticketmaster/attractions_no_results.json")
            .as_bytes()
            .to_vec(),
    );
    wire.answer(
        &url("ticketmaster/events.json"),
        200,
        &[],
        include_str!("../fixtures/ticketmaster/events_radiohead_none.json")
            .as_bytes()
            .to_vec(),
    );

    let acts = providers
        .ticketmaster
        .search_attractions("k", "zzqxvnosuchact", USER)
        .await
        .unwrap();
    let events = providers
        .ticketmaster
        .events_for_attraction("k", "K8vZ91713wV", USER)
        .await
        .unwrap();

    assert!(acts.is_empty());
    assert!(events.is_empty());
}

/// A MusicBrainz link with no id is supporting evidence: it is dropped and the
/// act still reads. Padded and cased ids are trimmed and lowercased, and blank
/// ids are skipped. The body is made up, since no recorded answer has a link
/// without an id.
#[tokio::test]
async fn ticketmaster_link_without_an_id_is_dropped_and_the_act_reads() {
    let (wire, providers) = scripted();
    json_answer(
        &wire,
        "ticketmaster/events.json",
        json!({
            "_embedded": {"events": [{
                "id": "e1",
                "_embedded": {"attractions": [{
                    "id": "K8vZ9179LP7",
                    "name": "Fontaines D.C.",
                    "externalLinks": {"musicbrainz": [
                        {"id": "FD87ACC7-E0A0-4A45-BC2A-D2AB5C10BE68  "},
                        {"url": "https://musicbrainz.org/artist/unlinked"},
                        {"id": null},
                        null,
                        {"id": "   "}
                    ]}
                }]}
            }]},
            "page": {"totalPages": 1},
        }),
    );

    let events = providers
        .ticketmaster
        .events_for_attraction("k", "K8vZ9179LP7", USER)
        .await
        .unwrap();

    let act = &events[0].embedded.as_ref().unwrap().attractions[0];
    assert_eq!(act.id, "K8vZ9179LP7");
    assert_eq!(act.name, "Fontaines D.C.");
    assert_eq!(
        act.musicbrainz_ids(),
        ["fd87acc7-e0a0-4a45-bc2a-d2ab5c10be68"]
    );
}

// ---------------------------------------------------------------------------
// YouTube
// ---------------------------------------------------------------------------

fn search_hit(video_id: &str) -> serde_json::Value {
    json!({
        "kind": "youtube#searchListResponse", "etag": "x",
        "pageInfo": {"totalResults": 1},
        "items": [{"kind": "youtube#searchResult", "etag": "y",
                   "id": {"kind": "youtube#video", "videoId": video_id}}],
    })
}

/// Unknown answer fields decode past silently, at the top level and
/// inside the result id; the key goes out as the `key` query pair.
#[tokio::test]
async fn youtube_search_tolerates_unknown_fields() {
    let (wire, providers) = scripted();
    let mut hit = search_hit("abcdefghijk");
    hit["futureTop"] = json!(1);
    hit["items"][0]["id"]["futureId"] = json!(1);
    json_answer(&wire, SEARCH, hit);
    let (_scratch, path) = quota_path("tolerant");
    let client = youtube(&providers, &path, 80);

    let found = client
        .search_video("Neil", "After the Gold Rush", USER)
        .await
        .unwrap();

    assert_eq!(found.as_deref(), Some("abcdefghijk"));
    let calls = sent_to(&wire, SEARCH);
    assert_eq!(calls[0].query("key").as_deref(), Some("secret"));
    assert_eq!(
        calls[0].query("q").as_deref(),
        Some("Neil After the Gold Rush full album")
    );
}

/// The quota governor: the first search reserves durably, the second (on a
/// fresh key) is refused before any HTTP, and the status snapshot matches,
/// dated by the clock the client was given.
#[tokio::test]
async fn youtube_quota_exhaustion_blocks_second_search() {
    let (wire, providers) = scripted();
    json_answer(&wire, SEARCH, search_hit("abcdefghijk"));
    let (_scratch, path) = quota_path("exhausted");
    let client = youtube(&providers, &path, 1);

    assert_eq!(client.quota_remaining().await, 1);
    assert!(client.search_available().await);
    assert_eq!(
        client
            .search_video("a", "one", USER)
            .await
            .unwrap()
            .as_deref(),
        Some("abcdefghijk")
    );
    assert!(matches!(
        client.search_track("a", "two", USER).await,
        Err(YoutubeError::QuotaExhausted)
    ));

    assert_eq!(sent_to(&wire, SEARCH).len(), 1);
    assert_eq!(
        quota_file_json(&path),
        json!({"date": "2026-05-05", "count": 1})
    );
    assert_eq!(client.quota_remaining().await, 0);
    assert!(!client.search_available().await);
    let status = client.get_quota_status().await;
    assert_eq!(status.used, 1);
    assert_eq!(status.limit, 1);
    assert_eq!(status.remaining, 0);
    assert_eq!(status.date, "2026-05-05");
}

/// A recreated client on the same quota path remembers what today spent,
/// and the next day by the clock starts from zero.
#[tokio::test]
async fn youtube_quota_survives_client_recreation() {
    let (wire, providers) = scripted();
    json_answer(&wire, SEARCH, search_hit("abcdefghijk"));
    let (_scratch, path) = quota_path("recreate");
    let first = youtube(&providers, &path, 1);
    first.search_video("a", "one", USER).await.unwrap();
    drop(first);

    let clock = may_fifth();
    let second = YouTubeClient::new(
        providers.youtube.clone(),
        path.clone(),
        enabled_settings(1),
        clock.clone(),
        providers.cache.clone(),
    )
    .unwrap();
    assert!(matches!(
        second.search_video("a", "blocked", USER).await,
        Err(YoutubeError::QuotaExhausted)
    ));
    clock.advance(24 * 3600 * 1000);
    assert_eq!(second.quota_remaining().await, 1);
}

/// A dispatched search that fails stays charged and uncached; a later search
/// for the same pair tries the network again. A 403 (the API's quota
/// answer among others) is a refusal.
#[tokio::test]
async fn youtube_failures_are_charged_not_cached() {
    let (wire, providers) = scripted();
    wire.answer(&url(SEARCH), 403, &[], "{}");
    json_answer(&wire, SEARCH, search_hit("abcdefghijk"));
    let (_scratch, path) = quota_path("charged");
    let client = youtube(&providers, &path, 2);

    assert!(matches!(
        client.search_video("a", "album", USER).await,
        Err(YoutubeError::Provider(ProviderError::Forbidden { .. }))
    ));
    assert_eq!(quota_file_json(&path)["count"], json!(1));
    assert!(!client.is_cached("a", "album", SearchKind::Album).await);

    assert_eq!(
        client
            .search_video("a", "album", USER)
            .await
            .unwrap()
            .as_deref(),
        Some("abcdefghijk")
    );
    assert_eq!(sent_to(&wire, SEARCH).len(), 2);
    assert_eq!(quota_file_json(&path)["count"], json!(2));
}

/// Undecodable bodies and an upstream 429 map distinctly and are charged
/// once each, never resent. While the API rests after the 429, a person's
/// search is refused before a unit is spent.
#[tokio::test]
async fn youtube_upstream_429_and_bad_payload_map() {
    let (wire, providers) = scripted();
    wire.answer(&url(SEARCH), 200, &[], "not json");
    json_answer(&wire, SEARCH, json!({"items": [{"id": {}}]}));
    wire.answer(&url(SEARCH), 429, &[("retry-after", "30")], "{}");
    let (_scratch, path) = quota_path("upstream");
    let client = youtube(&providers, &path, 80);

    assert!(matches!(
        client.search_track("x", "broken", USER).await,
        Err(YoutubeError::Provider(ProviderError::Payload {
            kind: PayloadKind::Decode,
            ..
        }))
    ));
    assert!(matches!(
        client.search_track("x", "no-id", USER).await,
        Err(YoutubeError::Provider(ProviderError::Payload {
            kind: PayloadKind::Contract,
            ..
        }))
    ));
    assert!(matches!(
        client.search_track("x", "limited", USER).await,
        Err(YoutubeError::Provider(ProviderError::RateLimited { .. }))
    ));
    // All three dispatched once, so all three were charged.
    assert_eq!(sent_to(&wire, SEARCH).len(), 3);
    assert_eq!(quota_file_json(&path)["count"], json!(3));

    assert!(matches!(
        client.search_track("x", "resting", USER).await,
        Err(YoutubeError::Provider(ProviderError::RateLimited { .. }))
    ));
    assert_eq!(sent_to(&wire, SEARCH).len(), 3);
    assert_eq!(quota_file_json(&path)["count"], json!(3));
}

/// Two users sharing the one client of a quota path cannot spend the last
/// slot twice.
#[tokio::test]
async fn youtube_two_instances_share_last_slot() {
    let (wire, providers) = scripted();
    json_answer(&wire, SEARCH, search_hit("abcdefghijk"));
    let (_scratch, path) = quota_path("last-slot");
    let first = youtube(&providers, &path, 1);
    let second = first.clone();

    let (one, two) = tokio::join!(
        first.search_video("a", "one", USER),
        second.search_track("a", "two", USER),
    );
    let outcomes = [one, two];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, Ok(Some(id)) if id == "abcdefghijk"))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, Err(YoutubeError::QuotaExhausted)))
            .count(),
        1
    );
    assert_eq!(sent_to(&wire, SEARCH).len(), 1);
    assert_eq!(quota_file_json(&path)["count"], json!(1));
}

// ---------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------

const GITHUB_RELEASES: &str = include_str!("../fixtures/github/releases_list.json");
const GITHUB_NOT_FOUND: &str = include_str!("../fixtures/github/releases_not_found.json");

fn releases_payload() -> serde_json::Value {
    serde_json::from_str(GITHUB_RELEASES).expect("the recorded list parses")
}

/// The recorded list decodes in full. A missing name or body falls back to
/// the tag and `""`, a draft is filtered out, and the GitHub media type
/// header goes out on the call.
#[tokio::test]
async fn github_releases_decode_filter_and_fall_back() {
    let (wire, providers) = scripted();
    let mut payload = releases_payload();
    payload[0]["name"] = serde_json::Value::Null;
    payload[1]
        .as_object_mut()
        .expect("a release is an object")
        .remove("body");
    payload[2]["draft"] = json!(true);
    json_answer(&wire, "releases", payload);

    let releases = providers.github.fetch_releases(USER).await;

    assert_eq!(releases.len(), 29);
    assert_eq!(releases[0].tag_name, "v2.15.0");
    assert_eq!(releases[0].name, "v2.15.0");
    assert_eq!(releases[0].published_at, "2026-09-20T01:49:51Z");
    assert_eq!(
        releases[0].html_url,
        "https://github.com/DroppedNeedle/DroppedNeedle/releases/tag/v2.15.0"
    );
    assert!(!releases[0].prerelease);
    assert_eq!(releases[1].tag_name, "v2.14.1");
    assert_eq!(releases[1].body, "");
    assert_eq!(releases[2].tag_name, "v2.13.0");
    let calls = sent_to(&wire, "releases");
    assert_eq!(
        calls[0].headers,
        [(
            "accept".to_owned(),
            "application/vnd.github+json".to_owned()
        )]
    );
}

/// Error answers degrade to `[]`, quietly, and are not remembered: the
/// next check asks again, while the row has a request to give, and keeps
/// what it gets.
#[tokio::test]
async fn github_failures_degrade_to_empty_and_are_not_kept() {
    let (wire, providers) = scripted();
    wire.answer(&url("releases"), 404, &[], GITHUB_NOT_FOUND);
    json_answer(&wire, "releases", releases_payload());

    assert_eq!(providers.github.fetch_releases(USER).await, vec![]);
    assert_eq!(
        providers
            .github
            .fetch_latest_release(USER)
            .await
            .map(|release| release.tag_name),
        Some("v2.15.0".to_owned())
    );
    assert_eq!(providers.github.fetch_releases(USER).await.len(), 30);
    assert_eq!(sent_to(&wire, "releases").len(), 2);
}

/// A person's check never waits for a request. Once failing answers have
/// spent the row's burst (50 an hour, so the next request is 72 s away),
/// the check answers `[]` at once and sends nothing.
#[tokio::test(start_paused = true)]
async fn github_check_never_waits_for_a_request() {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(&url("releases"), 404, &[], "{}");
    let providers = Providers::over_wire(
        wire.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        droppedneedle_providers::ProviderEndpoints::closed(),
    );
    let burst = droppedneedle_providers::rates::row("github").unwrap().burst as usize;
    for _ in 0..burst {
        assert_eq!(providers.github.fetch_releases(USER).await, vec![]);
    }
    assert_eq!(sent_to(&wire, "releases").len(), burst);

    let started = tokio::time::Instant::now();
    assert_eq!(providers.github.fetch_releases(USER).await, vec![]);
    assert!(
        started.elapsed() <= droppedneedle_providers::executor::USER_COOLDOWN_WAIT,
        "waited {:?}",
        started.elapsed()
    );
    assert_eq!(sent_to(&wire, "releases").len(), burst, "nothing sent");
}
