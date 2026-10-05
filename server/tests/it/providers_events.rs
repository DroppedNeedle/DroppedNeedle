//! Geocoding, Skiddle, Ticketmaster, YouTube and GitHub clients against a
//! scripted local server: the decode of each live wire shape, required
//! identity fields, rate limits, Ticketmaster's page cap, and the YouTube
//! quota file (reserved before HTTP, charged for dispatched calls, kept
//! across restarts).

use crate::common::ScratchDir;
use droppedneedle::providers::{geocoding, github, skiddle, ticketmaster, youtube};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::Query;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use geocoding::{DEFAULT_CITY_COUNT, GeocodingClient, GeocodingError};
use github::GitHubClient;
use serde_json::json;
use skiddle::{SkiddleClient, SkiddleError};
use ticketmaster::{TicketmasterClient, TicketmasterError};
use youtube::{SearchKind, YouTubeClient, YouTubeSettings, YoutubeError};

// ---------------------------------------------------------------------------
// Scripted fakes
// ---------------------------------------------------------------------------

/// Recorded query params per call, in arrival order.
type Calls = Arc<std::sync::Mutex<Vec<HashMap<String, String>>>>;

fn recorder() -> Calls {
    Arc::new(std::sync::Mutex::new(Vec::new()))
}

/// Serve a scripted router on an ephemeral localhost port; returns the base URL.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    base
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
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

// ---------------------------------------------------------------------------
// Geocoding
// ---------------------------------------------------------------------------

/// Live-verified 2026-07-06 shape: `?name=Liverpool` returns Liverpool GB
/// first with float coordinates, `country_code`, and `admin1`. Unknown
/// fields (`id`, `population`) decode past silently.
#[tokio::test]
async fn geocoding_search_decodes_live_shape_and_tolerates_unknown_fields() {
    let seen = recorder();
    let handler_seen = Arc::clone(&seen);
    let app = Router::new().route(
        "/v1/search",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_seen = Arc::clone(&handler_seen);
            async move {
                handler_seen.lock().unwrap().push(params);
                axum::Json(json!({
                    "results": [{
                        "id": 2644210, "name": "Liverpool",
                        "latitude": 53.41058, "longitude": -2.97794,
                        "country_code": "GB", "country": "United Kingdom",
                        "admin1": "England", "population": 496770,
                    }],
                }))
            }
        }),
    );
    let base = serve(app).await;
    let client = GeocodingClient::with_base_url(http(), format!("{base}/v1/search"));

    let cities = client
        .search_cities("Liverpool", DEFAULT_CITY_COUNT)
        .await
        .unwrap();

    assert_eq!(cities.len(), 1);
    let city = &cities[0];
    assert_eq!(city.name, "Liverpool");
    assert!((city.latitude - 53.41058).abs() < 1e-9);
    assert!((city.longitude - -2.97794).abs() < 1e-9);
    assert_eq!(city.country_code.as_deref(), Some("GB"));
    assert_eq!(city.country.as_deref(), Some("United Kingdom"));
    assert_eq!(city.admin1.as_deref(), Some("England"));
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].get("name").map(String::as_str), Some("Liverpool"));
    assert_eq!(
        calls[0].get("count").map(String::as_str),
        Some(DEFAULT_CITY_COUNT.to_string()).as_deref()
    );
    assert_eq!(calls[0].get("language").map(String::as_str), Some("en"));
    assert_eq!(calls[0].get("format").map(String::as_str), Some("json"));
}

#[tokio::test]
async fn geocoding_429_maps_to_rate_limited() {
    let app = Router::new().route(
        "/v1/search",
        get(|| async { (StatusCode::TOO_MANY_REQUESTS, "{}") }),
    );
    let base = serve(app).await;
    let client = GeocodingClient::with_base_url(http(), format!("{base}/v1/search"));

    assert!(matches!(
        client.search_cities("Liverpool", 8).await,
        Err(GeocodingError::RateLimited)
    ));
}

/// City rows are all-defaulted (the v2 struct shape): a name-only row
/// decodes with zeroed coordinates rather than failing, and there is no
/// skip-and-continue: one mistyped identity field fails the whole decode.
#[tokio::test]
async fn geocoding_name_only_row_decodes_mistyped_identity_fails() {
    let app = Router::new().route(
        "/v1/search",
        get(|Query(params): Query<HashMap<String, String>>| async move {
            match params.get("name").map(String::as_str) {
                Some("X") => axum::Json(json!({"results": [{"name": "X"}]})).into_response(),
                _ => axum::Json(json!({"results": [{"name": 123}]})).into_response(),
            }
        }),
    );
    let base = serve(app).await;
    let client = GeocodingClient::with_base_url(http(), format!("{base}/v1/search"));

    let cities = client.search_cities("X", 8).await.unwrap();
    assert_eq!(cities.len(), 1);
    assert_eq!(cities[0].name, "X");
    assert_eq!(cities[0].latitude, 0.0);
    assert_eq!(cities[0].country_code, None);
    assert!(matches!(
        client.search_cities("bad", 8).await,
        Err(GeocodingError::Decode(_))
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
    let seen = recorder();
    let handler_seen = Arc::clone(&seen);
    let app = Router::new().route(
        "/events/search/",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_seen = Arc::clone(&handler_seen);
            async move {
                handler_seen.lock().unwrap().push(params);
                axum::Json(json!({
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
                }))
            }
        }),
    );
    let base = serve(app).await;
    let client = SkiddleClient::with_base_url(http(), "k", &base);

    let events = client.events_for_artist("123568993").await.unwrap();

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
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].get("a").map(String::as_str), Some("123568993"));
    assert_eq!(calls[0].get("description").map(String::as_str), Some("1"));
    assert_eq!(calls[0].get("api_key").map(String::as_str), Some("k"));
}

/// Skiddle's own failure envelope arrives on HTTP 200 with `error != 0`.
#[tokio::test]
async fn skiddle_error_envelope_on_http_200_raises() {
    let app = Router::new().route(
        "/artists/",
        get(|| async { axum::Json(json!({"error": 1, "errormessage": "bad key"})) }),
    );
    let base = serve(app).await;
    let client = SkiddleClient::with_base_url(http(), "k", &base);

    match client.search_artists("x").await {
        Err(SkiddleError::Envelope(detail)) => assert!(detail.contains("bad key")),
        other => panic!("expected Envelope error, got {other:?}"),
    }
}

/// Required identity: a result without an id fails the decode instead of
/// materializing as an empty act.
#[tokio::test]
async fn skiddle_missing_id_fails_decode() {
    let app = Router::new().route(
        "/artists/",
        get(|| async { axum::Json(json!({"error": 0, "results": [{"name": "Nameless"}]})) }),
    );
    let base = serve(app).await;
    let client = SkiddleClient::with_base_url(http(), "k", &base);

    assert!(matches!(
        client.search_artists("x").await,
        Err(SkiddleError::Decode(_))
    ));
}

#[tokio::test]
async fn skiddle_429_maps_to_rate_limited() {
    let app = Router::new().route(
        "/artists/",
        get(|| async { (StatusCode::TOO_MANY_REQUESTS, "{}") }),
    );
    let base = serve(app).await;
    let client = SkiddleClient::with_base_url(http(), "k", &base);

    assert!(matches!(
        client.search_artists("x").await,
        Err(SkiddleError::RateLimited)
    ));
}

// ---------------------------------------------------------------------------
// Ticketmaster
// ---------------------------------------------------------------------------

/// Live shape (`tm_events.json`, 2026-07-06): local date, sale status, venue
/// with STRING coordinates, festival lineup with per-act MBIDs.
#[tokio::test]
async fn ticketmaster_events_decode_live_shape() {
    let app = Router::new().route(
        "/events.json",
        get(|| async {
            axum::Json(json!({
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
            }))
        }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);

    let events = client.events_for_attraction("K8vZ9179LP7").await.unwrap();

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
}

/// A result set deeper than the cap stops after three pages: no silent
/// fourth request.
#[tokio::test]
async fn ticketmaster_pagination_truncates_at_three_pages() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/events.json",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                let call = handler_calls.fetch_add(1, Ordering::SeqCst);
                let number: u32 = params
                    .get("page")
                    .and_then(|page| page.parse().ok())
                    .unwrap_or(0);
                axum::Json(json!({
                    "_embedded": {"events": [{"id": format!("e{call}")}]},
                    "page": {"totalPages": 99, "number": number},
                }))
            }
        }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);

    let events = client.events_for_attraction("A1").await.unwrap();

    assert_eq!(events.len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// The server's `Retry-After` hint travels on the rate-limit error.
#[tokio::test]
async fn ticketmaster_429_carries_retry_after_hint() {
    let app = Router::new().route(
        "/attractions.json",
        get(|| async {
            let mut headers = HeaderMap::new();
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("7"));
            (StatusCode::TOO_MANY_REQUESTS, headers, "slow down")
        }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);

    match client.search_attractions("x").await {
        Err(TicketmasterError::RateLimited { retry_after_secs }) => {
            assert_eq!(retry_after_secs, Some(7.0));
        }
        other => panic!("expected RateLimited error, got {other:?}"),
    }
}

/// Required identity: an event without an id fails the decode instead of
/// materializing as an empty event.
#[tokio::test]
async fn ticketmaster_missing_id_fails_decode() {
    let app = Router::new().route(
        "/events.json",
        get(|| async {
            axum::Json(json!({
                "_embedded": {"events": [{"name": "Nameless"}]},
                "page": {"totalPages": 1, "number": 0},
            }))
        }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);

    assert!(matches!(
        client.events_for_attraction("A1").await,
        Err(TicketmasterError::Decode(_))
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
/// inside the result id.
#[tokio::test]
async fn youtube_search_tolerates_unknown_fields() {
    let app = Router::new().route(
        "/youtube/v3/search",
        get(|| async {
            let mut hit = search_hit("abcdefghijk");
            hit["futureTop"] = json!(1);
            hit["items"][0]["id"]["futureId"] = json!(1);
            axum::Json(hit)
        }),
    );
    let base = serve(app).await;
    let (_scratch, path) = quota_path("tolerant");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();

    let found = client
        .search_video("Neil", "After the Gold Rush")
        .await
        .unwrap();

    assert_eq!(found.as_deref(), Some("abcdefghijk"));
    let _ = std::fs::remove_file(&path);
}

/// The quota governor: the first search reserves durably, the second (on a
/// fresh key) is refused before any HTTP, and the status snapshot matches.
#[tokio::test]
async fn youtube_quota_exhaustion_blocks_second_search() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                axum::Json(search_hit("abcdefghijk"))
            }
        }),
    );
    let base = serve(app).await;
    let (_scratch, path) = quota_path("exhausted");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(1), &base).unwrap();
    client.set_today_override(Some("2026-05-05".to_owned()));

    assert_eq!(client.quota_remaining().await, 1);
    assert!(client.search_available().await);
    assert_eq!(
        client.search_video("a", "one").await.unwrap().as_deref(),
        Some("abcdefghijk")
    );
    assert!(matches!(
        client.search_track("a", "two").await,
        Err(YoutubeError::QuotaExhausted)
    ));

    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
    let _ = std::fs::remove_file(&path);
}

/// A recreated client on the same quota path remembers what today spent.
#[tokio::test]
async fn youtube_quota_survives_client_recreation() {
    let app = Router::new().route(
        "/youtube/v3/search",
        get(|| async { axum::Json(search_hit("abcdefghijk")) }),
    );
    let base = serve(app).await;
    let (_scratch, path) = quota_path("recreate");
    let first =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(1), &base).unwrap();
    first.set_today_override(Some("2026-05-05".to_owned()));
    first.search_video("a", "one").await.unwrap();
    drop(first);

    let second =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(1), &base).unwrap();
    second.set_today_override(Some("2026-05-05".to_owned()));
    assert!(matches!(
        second.search_video("a", "blocked").await,
        Err(YoutubeError::QuotaExhausted)
    ));
    let _ = std::fs::remove_file(&path);
}

/// A dispatched search that fails stays charged and uncached; a later search
/// for the same pair tries the network again.
#[tokio::test]
async fn youtube_failures_are_charged_not_cached() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                let call = handler_calls.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    (StatusCode::FORBIDDEN, axum::Json(json!({}))).into_response()
                } else {
                    axum::Json(search_hit("abcdefghijk")).into_response()
                }
            }
        }),
    );
    let base = serve(app).await;
    let (_scratch, path) = quota_path("charged");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(2), &base).unwrap();
    client.set_today_override(Some("2026-05-05".to_owned()));

    assert!(matches!(
        client.search_video("a", "album").await,
        Err(YoutubeError::Api { status: 403 })
    ));
    assert_eq!(quota_file_json(&path)["count"], json!(1));
    assert!(!client.is_cached("a", "album", SearchKind::Album));

    assert_eq!(
        client.search_video("a", "album").await.unwrap().as_deref(),
        Some("abcdefghijk")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(quota_file_json(&path)["count"], json!(2));
    let _ = std::fs::remove_file(&path);
}

/// Upstream 429 and undecodable bodies map distinctly; both stay charged.
#[tokio::test]
async fn youtube_upstream_429_and_bad_payload_map() {
    let app = Router::new().route(
        "/youtube/v3/search",
        get(|Query(params): Query<HashMap<String, String>>| async move {
            match params.get("q").map(String::as_str) {
                Some("x limited") => (StatusCode::TOO_MANY_REQUESTS, "{}").into_response(),
                Some("x broken") => "not json".into_response(),
                _ => axum::Json(json!({"items": [{"id": {}}]})).into_response(),
            }
        }),
    );
    let base = serve(app).await;
    let (_scratch, path) = quota_path("upstream");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();
    client.set_today_override(Some("2026-05-05".to_owned()));

    assert!(matches!(
        client.search_track("x", "limited").await,
        Err(YoutubeError::UpstreamRateLimited)
    ));
    assert!(matches!(
        client.search_track("x", "broken").await,
        Err(YoutubeError::InvalidPayload(_))
    ));
    assert!(matches!(
        client.search_track("x", "no-id").await,
        Err(YoutubeError::InvalidPayload(_))
    ));
    // All three dispatched, so all three were charged.
    assert_eq!(quota_file_json(&path)["count"], json!(3));
    let _ = std::fs::remove_file(&path);
}

/// Two client instances on one quota path cannot spend the last slot twice.
#[tokio::test]
async fn youtube_two_instances_share_last_slot() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                // Overlap window, as above: 10ms forces the race with margin.
                tokio::time::sleep(Duration::from_millis(10)).await;
                axum::Json(search_hit("abcdefghijk"))
            }
        }),
    );
    let base = serve(app).await;
    let (_scratch, path) = quota_path("last-slot");
    let first =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(1), &base).unwrap();
    let second =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(1), &base).unwrap();

    let (one, two) = tokio::join!(
        first.search_video("a", "one"),
        second.search_track("a", "two"),
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
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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

fn releases_app(seen_accept: Arc<std::sync::Mutex<Vec<String>>>) -> Router {
    Router::new().route(
        "/repos/DroppedNeedle/DroppedNeedle/releases",
        get(move |headers: HeaderMap| {
            let seen_accept = Arc::clone(&seen_accept);
            async move {
                let accept = headers
                    .get(header::ACCEPT)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                seen_accept.lock().unwrap().push(accept);
                axum::Json(releases_payload())
            }
        }),
    )
}

/// Drafts filter out, prereleases stay, and missing name/body fall back to
/// the tag and `""`. The GitHub media type header goes out on the call.
#[tokio::test]
async fn github_releases_decode_filter_and_fall_back() {
    let seen_accept = Arc::new(std::sync::Mutex::new(Vec::new()));
    let base = serve(releases_app(Arc::clone(&seen_accept))).await;
    let client = GitHubClient::with_base_url(
        http(),
        format!("{base}/repos/DroppedNeedle/DroppedNeedle/releases"),
    );

    let releases = client.fetch_releases().await;

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
    assert_eq!(
        seen_accept.lock().unwrap().as_slice(),
        ["application/vnd.github+json"]
    );
}

/// Non-200 answers degrade to `[]`, quietly.
#[tokio::test]
async fn github_non_200_degrades_to_empty() {
    let app = Router::new().route("/releases", get(|| async { (StatusCode::FORBIDDEN, "{}") }));
    let base = serve(app).await;
    let client = GitHubClient::with_base_url(http(), format!("{base}/releases"));

    assert_eq!(client.fetch_releases().await, vec![]);
    assert_eq!(client.fetch_latest_release().await, None);
}
