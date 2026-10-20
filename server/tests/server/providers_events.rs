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

/// Live shape (`sk_byartist.json`, 2026-07-06): STRING `totalcount`,
/// `'0'` cancelled flag, empty-string absences, float venue coordinates,
/// mixed key casing (`eventname` vs `ticketUrl`).
#[tokio::test]
async fn skiddle_events_decode_live_shape() {
    let (wire, providers) = scripted();
    json_answer(
        &wire,
        "skiddle/events/search/",
        json!({
            "error": 0, "totalcount": "4", "pagecount": 4,
            "results": [{
                "id": "41414028", "EventCode": "FEST",
                "eventname": "Reading Festival",
                "cancelled": "0", "cancellationDate": "",
                "rescheduledDate": "",
                "link": "https://www.skiddle.com/festivals/Reading/",
                "ticketUrl": "",
                "date": "2026-08-27",
                "startdate": "2026-08-27T11:00:00+00:00",
                "venue": {"id": 8138, "name": "Little John's Farm",
                          "town": "Reading", "region": "Berkshire",
                          "country": "GB",
                          "latitude": 51.456062, "longitude": -0.991697},
                "artists": [{"artistid": "123611539", "name": "Sombr"}],
            }],
        }),
    );

    let events = providers
        .skiddle
        .events_for_artist("k", "123568993", USER)
        .await
        .unwrap();

    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.eventname, "Reading Festival");
    assert_eq!(event.date.as_deref(), Some("2026-08-27"));
    assert_eq!(
        event.startdate.as_deref(),
        Some("2026-08-27T11:00:00+00:00")
    );
    assert!(!event.is_cancelled());
    assert!(!event.is_rescheduled());
    assert_eq!(event.ticket_url.as_deref(), Some(""));
    assert!(event.link.as_deref().unwrap().starts_with("https://"));
    let venue = event.venue.as_ref().unwrap();
    assert_eq!(venue.name.as_deref(), Some("Little John's Farm"));
    assert_eq!(venue.town.as_deref(), Some("Reading"));
    assert_eq!(venue.region.as_deref(), Some("Berkshire"));
    assert_eq!(venue.country.as_deref(), Some("GB"));
    assert!((venue.latitude.unwrap() - 51.456062).abs() < 1e-9);
    assert!((venue.longitude.unwrap() - -0.991697).abs() < 1e-9);
    assert_eq!(event.artists.len(), 1);
    assert_eq!(event.artists[0].artistid.as_deref(), Some("123611539"));
    assert_eq!(event.artists[0].name.as_deref(), Some("Sombr"));
    let calls = sent_to(&wire, "skiddle/events/search/");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].query("a").as_deref(), Some("123568993"));
    assert_eq!(calls[0].query("description").as_deref(), Some("1"));
    assert_eq!(calls[0].query("api_key").as_deref(), Some("k"));
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

/// Live shape (`tm_events.json`, 2026-07-06): local date, sale status, venue
/// with STRING coordinates, festival lineup with per-act MBIDs.
#[tokio::test]
async fn ticketmaster_events_decode_live_shape() {
    let (wire, providers) = scripted();
    json_answer(
        &wire,
        "ticketmaster/events.json",
        json!({
            "_embedded": {"events": [{
                "name": "Reading Festival 2026 - Friday",
                "id": "1AdjZbVGklbLnsr",
                "url": "https://www.ticketmaster.co.uk/event/1",
                "dates": {"start": {"localDate": "2026-08-28",
                                   "dateTime": "2026-08-28T11:00:00Z"},
                          "status": {"code": "onsale"}},
                "_embedded": {
                    "venues": [{"name": "Richfield Avenue",
                                "city": {"name": "Reading"},
                                "state": {"name": "Berkshire"},
                                "country": {"countryCode": "GB"},
                                "location": {"latitude": "51.46368200",
                                             "longitude": "-0.97305600"}}],
                    "attractions": [
                        {"name": "Fontaines D.C.", "id": "K8vZ9179LP7",
                         "externalLinks": {"musicbrainz": [
                             {"id": "FD87ACC7-E0A0-4A45-BC2A-D2AB5C10BE68  "}]}}],
                }},
            ]},
            "page": {"size": 200, "totalElements": 1, "totalPages": 1, "number": 0},
        }),
    );

    let events = providers
        .ticketmaster
        .events_for_attraction("k", "K8vZ9179LP7", USER)
        .await
        .unwrap();

    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.name, "Reading Festival 2026 - Friday");
    assert_eq!(
        event.url.as_deref(),
        Some("https://www.ticketmaster.co.uk/event/1")
    );
    let dates = event.dates.as_ref().unwrap();
    assert_eq!(
        dates.start.as_ref().unwrap().local_date.as_deref(),
        Some("2026-08-28")
    );
    assert_eq!(
        dates.start.as_ref().unwrap().date_time.as_deref(),
        Some("2026-08-28T11:00:00Z")
    );
    assert_eq!(
        dates.status.as_ref().unwrap().code.as_deref(),
        Some("onsale")
    );
    let inner = event.embedded.as_ref().unwrap();
    let venue = &inner.venues[0];
    assert_eq!(venue.name.as_deref(), Some("Richfield Avenue"));
    assert_eq!(
        venue.city.as_ref().unwrap().name.as_deref(),
        Some("Reading")
    );
    assert_eq!(
        venue.state.as_ref().unwrap().name.as_deref(),
        Some("Berkshire")
    );
    assert_eq!(
        venue.country.as_ref().unwrap().country_code.as_deref(),
        Some("GB")
    );
    let location = venue.location.as_ref().unwrap();
    assert_eq!(location.latitude.as_deref(), Some("51.46368200"));
    assert_eq!(location.longitude.as_deref(), Some("-0.97305600"));
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

/// Required identity: an event without an id fails the decode instead of
/// materializing as an empty event.
#[tokio::test]
async fn ticketmaster_missing_id_fails_decode() {
    let (wire, providers) = scripted();
    json_answer(
        &wire,
        "ticketmaster/events.json",
        json!({
            "_embedded": {"events": [{"name": "Nameless"}]},
            "page": {"totalPages": 1, "number": 0},
        }),
    );

    assert!(matches!(
        providers
            .ticketmaster
            .events_for_attraction("k", "A1", USER)
            .await,
        Err(ProviderError::Payload {
            kind: PayloadKind::Decode,
            ..
        })
    ));
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
    assert!(!client.is_cached("a", "album", SearchKind::Album));

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

fn releases_payload() -> serde_json::Value {
    json!([
        {"tag_name": "v3.1.0", "name": "Sneak peek",
         "body": "notes", "published_at": "2026-09-01T00:00:00Z",
         "html_url": "https://example.com/1", "prerelease": true, "draft": false,
         "futureField": {"n": 1}},
        {"tag_name": "v3.0.0",
         "published_at": "2026-08-01T00:00:00Z",
         "html_url": "https://example.com/0", "prerelease": false, "draft": false},
        {"tag_name": "v3.2.0-draft",
         "published_at": "2026-09-02T00:00:00Z",
         "html_url": "https://example.com/2", "prerelease": false, "draft": true},
    ])
}

/// Drafts filter out, prereleases stay, and missing name/body fall back to
/// the tag and `""`. The GitHub media type header goes out on the call.
#[tokio::test]
async fn github_releases_decode_filter_and_fall_back() {
    let (wire, providers) = scripted();
    json_answer(&wire, "releases", releases_payload());

    let releases = providers.github.fetch_releases(USER).await;

    assert_eq!(releases.len(), 2);
    assert_eq!(releases[0].tag_name, "v3.1.0");
    assert_eq!(releases[0].name, "Sneak peek");
    assert_eq!(releases[0].body, "notes");
    assert_eq!(releases[0].published_at, "2026-09-01T00:00:00Z");
    assert_eq!(releases[0].html_url, "https://example.com/1");
    assert!(releases[0].prerelease);
    assert_eq!(releases[1].tag_name, "v3.0.0");
    assert_eq!(releases[1].name, "v3.0.0");
    assert_eq!(releases[1].body, "");
    assert!(!releases[1].prerelease);
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
    wire.answer(&url("releases"), 404, &[], "{}");
    json_answer(&wire, "releases", releases_payload());

    assert_eq!(providers.github.fetch_releases(USER).await, vec![]);
    assert_eq!(
        providers
            .github
            .fetch_latest_release(USER)
            .await
            .map(|release| release.tag_name),
        Some("v3.0.0".to_owned())
    );
    assert_eq!(providers.github.fetch_releases(USER).await.len(), 2);
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
