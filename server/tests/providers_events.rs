//! Provider briefs: the events/utility slice.
//!
//! Contract briefs, one per provider. Every brief runs against a scripted
//! axum fake on localhost; no live network anywhere.
//!
//! - Geocoding (Open-Meteo): `[]` only ever means "no such city"; provider
//!   failures raise. No API key. Live-verified 2026-07-06.
//! - Skiddle: actionable failures (`error != 0` on HTTP 200 included),
//!   string ids, the string `'0'`/`'1'` cancelled flag, empty-string
//!   absences, mixed key casing, and `totalcount` as int-or-string. Shapes
//!   verified against the live API on 2026-07-06.
//! - Ticketmaster: tolerant decode (missing `_embedded` is "no results"),
//!   string venue coordinates, normalized MusicBrainz ids, pagination to a
//!   loud 3-page cap, and the `Retry-After` hint carried on 429. Shapes
//!   verified against the live API on 2026-07-06.
//! - YouTube: preview search behind the quota-file governor (reserve before
//!   HTTP, refund what never dispatches, dispatched calls stay charged),
//!   an LRU preview cache that also caches absences, and key verification.
//! - GitHub: the quiet client; every failure degrades to `[]` with a log
//!   line, drafts filtered, prereleases skipped for latest, hourly memo.
//!
//! Each provider's briefs cover the same spine: tolerant unknown fields,
//! required-identity-field decode failure (no `default()` empties), the
//! cited wire quirks, and degradation behavior.

use droppedneedle::providers::{geocoding, github, skiddle, ticketmaster, youtube};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::Query;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use geocoding::{DEFAULT_CITY_COUNT, GeocodingClient, GeocodingError};
use github::GitHubClient;
use serde_json::json;
use skiddle::{SkiddleClient, SkiddleError, TotalCount};
use ticketmaster::{TicketmasterClient, TicketmasterError, parse_retry_after};
use youtube::{
    DEFAULT_DAILY_QUOTA_LIMIT, SearchKind, YouTubeClient, YouTubeSettings, YoutubeError,
};

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

/// A port nobody listens on: deterministic transport failure, no network.
fn closed_port() -> String {
    "http://127.0.0.1:1/".to_owned()
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

static QUOTA_SEQ: AtomicU64 = AtomicU64::new(0);

/// A unique, not-yet-created quota path nested in a fresh temp dir (so the
/// atomic write's parent-dir creation is exercised too).
fn quota_path(name: &str) -> PathBuf {
    let seq = QUOTA_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "dn-providers-events-{}-{seq}-{name}/quota/youtube_quota.json",
        std::process::id()
    ))
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
// Shared: production constructors build without touching the network
// ---------------------------------------------------------------------------

#[test]
fn production_constructors_build_without_touching_the_network() {
    let http = http();
    let _ = GeocodingClient::new(http.clone());
    let _ = SkiddleClient::new(http.clone(), "key");
    let _ = TicketmasterClient::new(http.clone(), "key");
    let _ = GitHubClient::new(http.clone());
    let _ = YouTubeClient::new(http, quota_path("ctor"), enabled_settings(1)).unwrap();
}

// ---------------------------------------------------------------------------
// Geocoding briefs
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

/// Open-Meteo omits `results` entirely for unknown places: that is `[]`,
/// never a phantom empty city.
#[tokio::test]
async fn geocoding_missing_results_key_is_empty_list_not_error() {
    let app = Router::new().route("/v1/search", get(|| async { axum::Json(json!({})) }));
    let base = serve(app).await;
    let client = GeocodingClient::with_base_url(http(), format!("{base}/v1/search"));

    assert_eq!(client.search_cities("xyzzy", 8).await.unwrap(), vec![]);
}

/// A failed city search must surface as "geocoding unavailable", never as
/// an empty list: non-200 raises.
#[tokio::test]
async fn geocoding_non_200_raises_api_error() {
    let app = Router::new().route(
        "/v1/search",
        get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "{}") }),
    );
    let base = serve(app).await;
    let client = GeocodingClient::with_base_url(http(), format!("{base}/v1/search"));

    match client.search_cities("Liverpool", 8).await {
        Err(GeocodingError::Api { status }) => assert_eq!(status, 500),
        other => panic!("expected Api error, got {other:?}"),
    }
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

#[tokio::test]
async fn geocoding_garbage_body_raises_decode_error() {
    let app = Router::new().route("/v1/search", get(|| async { "<html>not json</html>" }));
    let base = serve(app).await;
    let client = GeocodingClient::with_base_url(http(), format!("{base}/v1/search"));

    assert!(matches!(
        client.search_cities("Liverpool", 8).await,
        Err(GeocodingError::Decode(_))
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

#[tokio::test]
async fn geocoding_refused_connection_is_transport_error() {
    let client = GeocodingClient::with_base_url(http(), closed_port());

    assert!(matches!(
        client.search_cities("Liverpool", 8).await,
        Err(GeocodingError::Transport(_))
    ));
}

// ---------------------------------------------------------------------------
// Skiddle briefs
// ---------------------------------------------------------------------------

/// Live shape (`sk_artists.json`, 2026-07-06): string ids, duplicate listings
/// sharing one Spotify URI, int `totalcount`, unknown fields tolerated.
#[tokio::test]
async fn skiddle_artists_decode_live_shape() {
    let seen = recorder();
    let handler_seen = Arc::clone(&seen);
    let app = Router::new().route(
        "/artists/",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_seen = Arc::clone(&handler_seen);
            async move {
                handler_seen.lock().unwrap().push(params);
                axum::Json(json!({
                    "error": 0, "totalcount": 3, "pagecount": 3,
                    "results": [
                        {"id": "123617147", "name": "Fontaines CD",
                         "imageurl": "https://example.com/a.jpg",
                         "nextevent": null, "favourite": 0,
                         "spotifyartisturl": null},
                        {"id": "123568993", "name": "Fontaines D.C.",
                         "spotifyartisturl": "spotify:artist:3SXwqSqAoBz9WCI9PDQzY6"},
                        {"id": "123604351", "name": "Fontaines DC",
                         "spotifyartisturl": "spotify:artist:3SXwqSqAoBz9WCI9PDQzY6"},
                    ],
                    "requestId": "api_x",
                }))
            }
        }),
    );
    let base = serve(app).await;
    let client = SkiddleClient::with_base_url(http(), "k", &base);

    let artists = client.search_artists("Fontaines").await.unwrap();

    assert_eq!(
        artists
            .iter()
            .map(|artist| (artist.name.as_str(), artist.id.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Fontaines CD", "123617147"),
            ("Fontaines D.C.", "123568993"),
            ("Fontaines DC", "123604351"),
        ]
    );
    assert_eq!(artists[1].spotifyartisturl, artists[2].spotifyartisturl);
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].get("name").map(String::as_str), Some("Fontaines"));
    assert_eq!(calls[0].get("api_key").map(String::as_str), Some("k"));
}

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

/// The `'0'`/`'1'` cancelled flag and the blank-means-unset reschedule date.
#[tokio::test]
async fn skiddle_cancelled_and_rescheduled_helpers() {
    let app = Router::new().route(
        "/events/search/",
        get(|| async {
            axum::Json(json!({
                "error": 0, "totalcount": "2",
                "results": [
                    {"id": "1", "eventname": "off",
                     "cancelled": "1", "rescheduledDate": "2026-09-01"},
                    {"id": "2", "eventname": "on",
                     "cancelled": "0", "rescheduledDate": "   "},
                ],
            }))
        }),
    );
    let base = serve(app).await;
    let client = SkiddleClient::with_base_url(http(), "k", &base);

    let events = client.events_for_artist("9").await.unwrap();
    assert!(events[0].is_cancelled());
    assert!(events[0].is_rescheduled());
    assert!(!events[1].is_cancelled());
    assert!(!events[1].is_rescheduled());
}

/// `totalcount` decodes as both an int (artists endpoint) and a string
/// (events endpoint, e.g. `"392"`); both shapes coexist.
#[test]
fn skiddle_totalcount_accepts_int_and_string() {
    let as_int: TotalCount = serde_json::from_str("3").unwrap();
    let as_text: TotalCount = serde_json::from_str("\"392\"").unwrap();
    assert_eq!(as_int, TotalCount::Int(3));
    assert_eq!(as_text, TotalCount::Text("392".to_owned()));
    assert_eq!(TotalCount::default(), TotalCount::Int(0));
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

/// Non-2xx is not retriable at this layer: one call, one error.
#[tokio::test]
async fn skiddle_non_200_raises_without_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/events/search/",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                (StatusCode::INTERNAL_SERVER_ERROR, "{}")
            }
        }),
    );
    let base = serve(app).await;
    let client = SkiddleClient::with_base_url(http(), "k", &base);

    assert!(matches!(
        client.events_for_artist("1").await,
        Err(SkiddleError::Api { status: 500 })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn skiddle_garbage_body_raises_decode_error() {
    let app = Router::new().route("/artists/", get(|| async { "<html>not json</html>" }));
    let base = serve(app).await;
    let client = SkiddleClient::with_base_url(http(), "k", &base);

    assert!(matches!(
        client.search_artists("x").await,
        Err(SkiddleError::Decode(_))
    ));
}

#[tokio::test]
async fn skiddle_test_connection_reports_reachability() {
    async fn probe(body: serde_json::Value, status: StatusCode) -> bool {
        let app = Router::new().route(
            "/events/search/",
            get(move || {
                let body = body.clone();
                async move { (status, axum::Json(body)) }
            }),
        );
        let base = serve(app).await;
        SkiddleClient::with_base_url(http(), "k", &base)
            .test_connection()
            .await
    }

    assert!(probe(json!({"error": 0, "results": []}), StatusCode::OK).await);
    assert!(!probe(json!({"error": 1}), StatusCode::OK).await);
    assert!(!probe(json!({}), StatusCode::INTERNAL_SERVER_ERROR).await);
}

// ---------------------------------------------------------------------------
// Ticketmaster briefs
// ---------------------------------------------------------------------------

/// Live shape (`tm_attr.json`, 2026-07-06): the MusicBrainz link map, the
/// DJ-set sibling with no MBIDs, unknown fields tolerated.
#[tokio::test]
async fn ticketmaster_attractions_decode_live_shape() {
    let seen = recorder();
    let handler_seen = Arc::clone(&seen);
    let app = Router::new().route(
        "/attractions.json",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_seen = Arc::clone(&handler_seen);
            async move {
                handler_seen.lock().unwrap().push(params);
                axum::Json(json!({
                    "_embedded": {"attractions": [
                        {"name": "Fontaines D.C.", "type": "attraction",
                         "id": "K8vZ9179LP7", "test": false,
                         "externalLinks": {
                             "youtube": [{"url": "https://youtube.example/x"}],
                             "musicbrainz": [
                                 {"id": "fd87acc7-e0a0-4a45-bc2a-d2ab5c10be68",
                                  "url": "https://musicbrainz.example/x"}]}},
                        {"name": "Fontaines D.C. DJ Set", "id": "K8vZ9179LQ0"},
                    ]},
                }))
            }
        }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);

    let attractions = client.search_attractions("Fontaines D.C.").await.unwrap();

    assert_eq!(
        attractions
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["Fontaines D.C.", "Fontaines D.C. DJ Set"]
    );
    assert_eq!(
        attractions[0].musicbrainz_ids(),
        ["fd87acc7-e0a0-4a45-bc2a-d2ab5c10be68"]
    );
    assert_eq!(attractions[1].musicbrainz_ids(), Vec::<String>::new());
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].get("keyword").map(String::as_str),
        Some("Fontaines D.C.")
    );
    assert_eq!(
        calls[0].get("classificationName").map(String::as_str),
        Some("Music")
    );
    assert_eq!(calls[0].get("size").map(String::as_str), Some("50"));
    assert_eq!(calls[0].get("apikey").map(String::as_str), Some("k"));
}

/// MusicBrainz ids arrive padded or cased; reads trim, lowercase, and skip
/// blanks.
#[test]
fn ticketmaster_musicbrainz_ids_normalize() {
    let attraction: ticketmaster::TmAttraction = serde_json::from_value(json!({
        "id": "x",
        "externalLinks": {"musicbrainz": [
            {"id": "  FD87ACC7-E0A0-4A45-BC2A-D2AB5C10BE68  "},
            {"id": "   "},
            {},
        ]},
    }))
    .unwrap();
    assert_eq!(
        attraction.musicbrainz_ids(),
        ["fd87acc7-e0a0-4a45-bc2a-d2ab5c10be68"]
    );
}

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

/// Ticketmaster omits `_embedded` entirely on zero results: that is `[]`,
/// not a decode error.
#[tokio::test]
async fn ticketmaster_missing_embedded_is_empty_list() {
    let app = Router::new().route(
        "/attractions.json",
        get(|| async { axum::Json(json!({"page": {"totalPages": 0}})) }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);

    assert_eq!(client.search_attractions("nobody").await.unwrap(), vec![]);
}

/// Pagination follows `page` cursors oldest-first until the last page.
#[tokio::test]
async fn ticketmaster_pagination_follows_next_pages() {
    let pages: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let handler_pages = Arc::clone(&pages);
    let app = Router::new().route(
        "/events.json",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_pages = Arc::clone(&handler_pages);
            async move {
                let page = params.get("page").cloned().unwrap_or_default();
                handler_pages.lock().unwrap().push(page.clone());
                let number: u32 = page.parse().unwrap_or(0);
                let id = if number == 1 { "e2" } else { "e1" };
                axum::Json(json!({
                    "_embedded": {"events": [{"id": id, "name": id}]},
                    "page": {"size": 200, "totalElements": 2,
                             "totalPages": 2, "number": number},
                }))
            }
        }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);

    let events = client.events_for_attraction("A1").await.unwrap();

    assert_eq!(
        events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["e1", "e2"]
    );
    assert_eq!(pages.lock().unwrap().as_slice(), ["0", "1"]);
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

/// Only positive numeric hints count; anything else means "no hint".
#[test]
fn ticketmaster_retry_after_parsing_ignores_garbage() {
    assert_eq!(parse_retry_after(Some("7")), Some(7.0));
    assert_eq!(parse_retry_after(Some(" 2.5 ")), Some(2.5));
    assert_eq!(parse_retry_after(None), None);
    assert_eq!(parse_retry_after(Some("soon")), None);
    assert_eq!(parse_retry_after(Some("0")), None);
    assert_eq!(parse_retry_after(Some("-3")), None);
    assert_eq!(parse_retry_after(Some("")), None);
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

#[tokio::test]
async fn ticketmaster_non_200_raises_without_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/attractions.json",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                (StatusCode::INTERNAL_SERVER_ERROR, "{}")
            }
        }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);

    assert!(matches!(
        client.search_attractions("x").await,
        Err(TicketmasterError::Api { status: 500 })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Reachability only: a 200 with an undecodable body still counts, like v2.
#[tokio::test]
async fn ticketmaster_test_connection_checks_reachability_only() {
    let app = Router::new().route(
        "/attractions.json",
        get(|| async { "not json, still reachable" }),
    );
    let base = serve(app).await;
    let client = TicketmasterClient::with_base_url(http(), "k", &base);
    assert!(client.test_connection().await);

    let failing = TicketmasterClient::with_base_url(http(), "k", closed_port());
    assert!(!failing.test_connection().await);
}

// ---------------------------------------------------------------------------
// YouTube briefs
// ---------------------------------------------------------------------------

fn search_hit(video_id: &str) -> serde_json::Value {
    json!({
        "kind": "youtube#searchListResponse", "etag": "x",
        "pageInfo": {"totalResults": 1},
        "items": [{"kind": "youtube#searchResult", "etag": "y",
                   "id": {"kind": "youtube#video", "videoId": video_id}}],
    })
}

/// Album search asks `{artist} {album} full album` with the fixed param set.
/// Unknown answer fields decode past silently.
#[tokio::test]
async fn youtube_album_search_query_shape() {
    let seen = recorder();
    let handler_seen = Arc::clone(&seen);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_seen = Arc::clone(&handler_seen);
            async move {
                handler_seen.lock().unwrap().push(params);
                axum::Json(search_hit("abcdefghijk"))
            }
        }),
    );
    let base = serve(app).await;
    let path = quota_path("album-query");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();

    let found = client
        .search_video("Neil", "After the Gold Rush")
        .await
        .unwrap();

    assert_eq!(found.as_deref(), Some("abcdefghijk"));
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].get("q").map(String::as_str),
        Some("Neil After the Gold Rush full album")
    );
    assert_eq!(calls[0].get("part").map(String::as_str), Some("id"));
    assert_eq!(calls[0].get("type").map(String::as_str), Some("video"));
    assert_eq!(calls[0].get("maxResults").map(String::as_str), Some("1"));
    assert_eq!(calls[0].get("key").map(String::as_str), Some("secret"));
    let _ = std::fs::remove_file(&path);
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
    let path = quota_path("tolerant");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();

    let found = client
        .search_video("Neil", "After the Gold Rush")
        .await
        .unwrap();

    assert_eq!(found.as_deref(), Some("abcdefghijk"));
    let _ = std::fs::remove_file(&path);
}

/// Track search asks the bare `{artist} {title}`.
#[tokio::test]
async fn youtube_track_search_query_shape() {
    let seen = recorder();
    let handler_seen = Arc::clone(&seen);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_seen = Arc::clone(&handler_seen);
            async move {
                handler_seen.lock().unwrap().push(params);
                axum::Json(search_hit("track-video"))
            }
        }),
    );
    let base = serve(app).await;
    let path = quota_path("track-query");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();

    let found = client.search_track("Neil", "Heart of Gold").await.unwrap();

    assert_eq!(found.as_deref(), Some("track-video"));
    let calls = seen.lock().unwrap();
    assert_eq!(
        calls[0].get("q").map(String::as_str),
        Some("Neil Heart of Gold")
    );
    let _ = std::fs::remove_file(&path);
}

/// A disabled client fails before any HTTP and leaves the quota file
/// untouched (not even created).
#[tokio::test]
async fn youtube_disabled_client_fails_before_http() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                axum::Json(search_hit("x"))
            }
        }),
    );
    let base = serve(app).await;
    let path = quota_path("disabled");
    let settings = YouTubeSettings {
        enabled: false,
        ..enabled_settings(80)
    };
    let client = YouTubeClient::with_base_url(http(), path.clone(), settings, &base).unwrap();

    assert!(!client.is_configured());
    assert!(!client.search_available().await);
    assert!(matches!(
        client.search_video("a", "b").await,
        Err(YoutubeError::NotConfigured(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!path.exists());
}

/// The governor brief: the first search reserves durably, the second (on a
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
    let path = quota_path("exhausted");
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
    let path = quota_path("recreate");
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

/// Rollover: a file stamped with a stale date reads as zero, and the next
/// reservation restamps it for today.
#[tokio::test]
async fn youtube_quota_rolls_over_on_new_day() {
    let app = Router::new().route(
        "/youtube/v3/search",
        get(|| async { axum::Json(search_hit("abcdefghijk")) }),
    );
    let base = serve(app).await;
    let path = quota_path("rollover");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, br#"{"date":"2026-01-01","count":5}"#).unwrap();
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(1), &base).unwrap();
    client.set_today_override(Some("2026-05-06".to_owned()));

    assert_eq!(
        client.search_video("a", "new").await.unwrap().as_deref(),
        Some("abcdefghijk")
    );
    assert_eq!(
        quota_file_json(&path),
        json!({"date": "2026-05-06", "count": 1})
    );
    let _ = std::fs::remove_file(&path);
}

/// A corrupt or negative quota file fails the constructor, like v2.
#[tokio::test]
async fn youtube_corrupt_quota_file_is_store_error() {
    for (name, bytes) in [
        ("corrupt", b"not json".as_slice()),
        (
            "negative",
            br#"{"date":"2026-05-05","count":-1}"#.as_slice(),
        ),
    ] {
        let path = quota_path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let outcome = YouTubeClient::with_base_url(
            http(),
            path.clone(),
            enabled_settings(DEFAULT_DAILY_QUOTA_LIMIT),
            "http://127.0.0.1:1/",
        );
        assert!(
            matches!(outcome, Err(YoutubeError::QuotaStore(_))),
            "{name} quota file should fail the constructor"
        );
        let _ = std::fs::remove_file(&path);
    }
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
    let path = quota_path("charged");
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

/// An empty result is a cached absence: one HTTP call, then memory.
#[tokio::test]
async fn youtube_empty_result_is_cached_absence() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"items": []}))
            }
        }),
    );
    let base = serve(app).await;
    let path = quota_path("absence");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();

    assert_eq!(client.search_track("a", "t").await.unwrap(), None);
    assert_eq!(client.search_track("a", "t").await.unwrap(), None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(client.is_cached("a", "t", SearchKind::Track));
    let _ = std::fs::remove_file(&path);
}

/// Album and track searches are separate cache identities, and `are_cached`
/// answers for track pairs under `artist|track` keys.
#[tokio::test]
async fn youtube_album_track_identity_and_are_cached() {
    let app = Router::new().route(
        "/youtube/v3/search",
        get(|| async { axum::Json(search_hit("abcdefghijk")) }),
    );
    let base = serve(app).await;
    let path = quota_path("identity");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();

    client.search_video("Artist", "Same").await.unwrap();
    assert!(client.is_cached("artist", "same", SearchKind::Album));
    assert!(!client.is_cached("artist", "same", SearchKind::Track));
    assert_eq!(
        client.are_cached(&[("artist", "same")]).get("artist|same"),
        Some(&false)
    );
    client.search_track("artist", "same").await.unwrap();
    assert_eq!(
        client.are_cached(&[("Artist", "Same")]).get("artist|same"),
        Some(&true)
    );
    let _ = std::fs::remove_file(&path);
}

/// Concurrent identical searches share one HTTP call and one quota unit.
#[tokio::test]
async fn youtube_concurrent_identical_searches_share_one_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                // Overlap window: both join!'d searches arrive within ~1ms on
                // loopback, so 10ms holds the flight open with margin.
                tokio::time::sleep(Duration::from_millis(10)).await;
                axum::Json(search_hit("abcdefghijk"))
            }
        }),
    );
    let base = serve(app).await;
    let path = quota_path("shared-call");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();
    client.set_today_override(Some("2026-05-05".to_owned()));

    let (first, second) = tokio::join!(
        client.search_video("a", "album"),
        client.search_video("A", "Album"),
    );

    assert_eq!(first.unwrap().as_deref(), Some("abcdefghijk"));
    assert_eq!(second.unwrap().as_deref(), Some("abcdefghijk"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(quota_file_json(&path)["count"], json!(1));
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
    let path = quota_path("last-slot");
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
    let _ = std::fs::remove_file(&path);
}

/// `configure` swaps the key the next search sends.
#[tokio::test]
async fn youtube_configure_swaps_api_key() {
    let seen = recorder();
    let handler_seen = Arc::clone(&seen);
    let app = Router::new().route(
        "/youtube/v3/search",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let handler_seen = Arc::clone(&handler_seen);
            async move {
                handler_seen.lock().unwrap().push(params);
                axum::Json(search_hit("abcdefghijk"))
            }
        }),
    );
    let base = serve(app).await;
    let path = quota_path("configure");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();

    client.configure("rotated");
    client.search_video("a", "b").await.unwrap();

    assert_eq!(
        seen.lock().unwrap()[0].get("key").map(String::as_str),
        Some("rotated")
    );
    let _ = std::fs::remove_file(&path);
}

/// Settings updates validate the budget range and take effect on next use.
#[tokio::test]
async fn youtube_update_settings_validates_and_applies() {
    let app = Router::new().route(
        "/youtube/v3/search",
        get(|| async { axum::Json(search_hit("abcdefghijk")) }),
    );
    let base = serve(app).await;
    let path = quota_path("settings");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(80), &base).unwrap();

    let invalid = YouTubeSettings {
        daily_quota_limit: 0,
        ..enabled_settings(80)
    };
    assert!(matches!(
        client.update_settings(invalid),
        Err(YoutubeError::NotConfigured(_))
    ));
    let too_big = YouTubeSettings {
        daily_quota_limit: 10_001,
        ..enabled_settings(80)
    };
    assert!(matches!(
        client.update_settings(too_big),
        Err(YoutubeError::NotConfigured(_))
    ));

    let disabled = YouTubeSettings {
        api_enabled: false,
        ..enabled_settings(80)
    };
    client.update_settings(disabled).unwrap();
    assert!(!client.is_configured());
    assert!(matches!(
        client.search_video("a", "b").await,
        Err(YoutubeError::NotConfigured(_))
    ));
    let _ = std::fs::remove_file(&path);
}

/// Key verification never touches the quota and reports plain verdicts.
#[tokio::test]
async fn youtube_verify_api_key_reports_verdicts() {
    let app = Router::new().route(
        "/youtube/v3/videos",
        get(|Query(params): Query<HashMap<String, String>>| async move {
            match params.get("key").map(String::as_str) {
                Some("good") => (StatusCode::OK, "ok").into_response(),
                Some("bad") => (StatusCode::FORBIDDEN, "no").into_response(),
                _ => (StatusCode::BAD_GATEWAY, "weird").into_response(),
            }
        }),
    );
    let base = serve(app).await;
    let path = quota_path("verify");
    let client =
        YouTubeClient::with_base_url(http(), path.clone(), enabled_settings(1), &base).unwrap();

    assert_eq!(
        client.verify_api_key("good").await,
        (true, "YouTube API key is valid".to_owned())
    );
    assert_eq!(
        client.verify_api_key("bad").await,
        (
            false,
            "API key is invalid or YouTube Data API is not enabled".to_owned()
        )
    );
    let (ok, message) = client.verify_api_key("huh").await;
    assert!(!ok);
    assert!(message.contains("502"), "unexpected message: {message}");
    // Verification spends no quota.
    assert_eq!(client.quota_remaining().await, 1);

    let unreachable = YouTubeClient::with_base_url(
        http(),
        quota_path("verify-down"),
        enabled_settings(1),
        closed_port(),
    )
    .unwrap();
    let (ok, message) = unreachable.verify_api_key("good").await;
    assert!(!ok);
    assert!(message.starts_with("Connection error: "), "got: {message}");
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
    let path = quota_path("upstream");
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

// ---------------------------------------------------------------------------
// GitHub briefs
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

/// Latest skips prereleases; all-prerelease means no latest.
#[tokio::test]
async fn github_latest_release_skips_prereleases() {
    let seen_accept = Arc::new(std::sync::Mutex::new(Vec::new()));
    let base = serve(releases_app(seen_accept)).await;
    let client = GitHubClient::with_base_url(
        http(),
        format!("{base}/repos/DroppedNeedle/DroppedNeedle/releases"),
    );

    assert_eq!(
        client.fetch_latest_release().await.map(|r| r.tag_name),
        Some("v3.0.0".to_owned())
    );

    let pre_only = Router::new().route(
        "/releases",
        get(|| async {
            axum::Json(json!([
                {"tag_name": "v4.0.0-rc.1",
                 "published_at": "2026-09-01T00:00:00Z",
                 "html_url": "https://example.com/rc",
                 "prerelease": true, "draft": false},
            ]))
        }),
    );
    let pre_base = serve(pre_only).await;
    let pre_client = GitHubClient::with_base_url(http(), format!("{pre_base}/releases"));
    assert_eq!(pre_client.fetch_latest_release().await, None);
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

/// Undecodable bodies (including entries missing identity fields) degrade
/// to `[]`.
#[tokio::test]
async fn github_garbage_body_degrades_to_empty() {
    let app = Router::new().route("/releases", get(|| async { "<html>not json</html>" }));
    let base = serve(app).await;
    let client = GitHubClient::with_base_url(http(), format!("{base}/releases"));
    assert_eq!(client.fetch_releases().await, vec![]);

    let missing_id = Router::new().route(
        "/releases",
        get(|| async { axum::Json(json!([{"name": "Nameless"}])) }),
    );
    let missing_base = serve(missing_id).await;
    let missing_client = GitHubClient::with_base_url(http(), format!("{missing_base}/releases"));
    assert_eq!(missing_client.fetch_releases().await, vec![]);
}

/// Transport failures degrade to `[]`.
#[tokio::test]
async fn github_refused_connection_degrades_to_empty() {
    let client = GitHubClient::with_base_url(http(), closed_port());
    assert_eq!(client.fetch_releases().await, vec![]);
}

/// The hourly memo: a second check reuses the first answer without a second
/// HTTP call.
#[tokio::test]
async fn github_second_check_within_ttl_reuses_memo() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let app = Router::new().route(
        "/releases",
        get(move || {
            let handler_calls = Arc::clone(&handler_calls);
            async move {
                handler_calls.fetch_add(1, Ordering::SeqCst);
                axum::Json(releases_payload())
            }
        }),
    );
    let base = serve(app).await;
    let client = GitHubClient::with_base_url(http(), format!("{base}/releases"));

    let first = client.fetch_releases().await;
    let second = client.fetch_releases().await;
    assert!(!first.is_empty());
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
